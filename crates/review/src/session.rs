//! Reviews opened on a script, and decisions made through a [`Document`].
//!
//! A decision is stored as document metadata under a key per suggestion
//! (`review/<review id>/<entry>`), set in the same transaction as the text
//! change. The review id is the file's `Id:` header, else a hash of its
//! contents, so a saved session still applies when review files are added,
//! removed or reordered, and (with an `Id:`) when a file is appended to.
//! `<entry>` is the entry's position in the file, so appending entries moves
//! no key; inserting or reordering entries by hand mid-file does.
//! The status shown to the user is derived from that value and the paragraph's
//! current text, so a later hand edit to an accepted line shows as a rewrite.
//!
//! Values are `accepted:<k>@<para>`, `rejected@<para>`, `rewritten@<para>`:
//! the paragraph the decision was made on (built and read only through
//! [`crate::meta`]). A value whose paragraph isn't the item's is ignored
//! (pending), so deleting a block mid-file by hand orphans the decisions that
//! shift onto other entries instead of moving them there. Values without `@`
//! (written before this, and in the rebuilt starting point of a version 1
//! session, whose saved events expect them) are read as they are.
//!
//! **Blocks** ([`Item::block`]) always store full values naming every line
//! they were made on and what the decision left:
//! `accepted:<k>@<before ids>=>(<after ids> | after:<id> | start)`,
//! `rewritten@…=>…`, `rejected@<before ids>`. Each decision is one
//! `Document::edit` transaction (`replace_run` / `restore_run` plus the
//! value). Accepting a block over a line another review has decided is
//! refused (C9).
//!
//! **Structural edits** to a suggested line (see [`LineChange`]): the item
//! always stays on its paragraph id, so its number and decision survive.
//! - The line is gone (removed, or joined into the line above): a pending
//!   suggestion shows as [`Status::Removed`] ("line removed") and a decided one
//!   as [`Status::Rewritten`] (its text was cut); neither can be decided
//!   ([`Session::can_apply`]). Reopening a saved session keeps them
//!   (see [`crate::matching::match_live`]).
//! - The line was split (the item stays on the head, which keeps the id) or
//!   had the next line joined onto it: its text is now only part of (or more
//!   than) one paragraph, so writing a whole-line text into it would
//!   duplicate (or drop) the rest. Accepting is refused ([`Refusal::LineChanged`],
//!   which each host phrases with its own way to rewrite), as is keeping the
//!   original over a decision; keeping the current text, rewriting and going
//!   back to undecided (which leaves the text as it is) are allowed. A
//!   pending one shows as outdated. A split counts only if it cut the line's
//!   text as it was then (Enter at the end of a shortened line is not a
//!   split), and a split undone by joining the same two lines back (or the
//!   reverse) is no change.
//!
//! Hosts name these the same way everywhere ([`Session::status_label`],
//! [`Session::line_label`]): "line removed" (pending, gone), "line cut"
//! (decided, then gone; `STATUS: rewritten` and `MINE: [cut]` in the file).
//! - The line was retyped: nothing is refused; a pending one shows as outdated.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use scriptorium_core::doc::{DocError, Op};
use scriptorium_core::text::{normalize, similarity};
use scriptorium_core::{
    Document, ElementKind, Group, NewPara, ParaId, Paragraph, Pos, Replaced, Run, Script, Selection,
};

use crate::file::{Entry, FileStatus, Line, ReviewFile, RowRef, append_block, insert_id};
use crate::matching::{FUZZY_THRESHOLD, Found, Full, Miss, Para, RowResolver, match_review, typed};
use crate::meta::{MetaValue, Now, Stored};

/// What the writer decides about a suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Take option `k` (0-based, into [`Item::options`]).
    Accept(usize),
    /// Keep the paragraph as it is (or as it was, if this review changed it).
    KeepMine,
    /// The writer's own text (one line).
    Rewrite(String),
    /// The writer's own lines, for a block (CLI and tools): untyped lines
    /// take the kind of the `BEFORE` line in their place. `[]` cuts the lines.
    RewriteLines(Vec<Line>),
    /// Back to pending, undoing this review's change to the text.
    Undecided,
}

/// Where a suggestion stands, as shown to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pending,
    Accepted(usize),
    Rejected,
    Rewritten,
    /// Pending, but its line is gone (removed, or joined into the line
    /// above): it can't be decided any more. "line removed".
    Removed,
}

/// How a suggestion's line has been changed structurally since the
/// suggestion was written (see the module docs for what each allows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineChange {
    /// The paragraph no longer exists: removed, or joined into the line above.
    Removed,
    /// The paragraph was split in the middle; the item stays on the head.
    Split,
    /// The next line was joined onto the paragraph.
    Joined,
    /// The paragraph's element type changed.
    Retyped,
}

impl LineChange {
    /// How listings and the card name it. A line removed after it was
    /// decided is "line cut" instead (see [`Session::line_label`]).
    pub fn describe(self) -> &'static str {
        match self {
            Self::Removed => "line removed",
            Self::Split => "line split",
            Self::Joined => "line joined with the next",
            Self::Retyped => "line retyped",
        }
    }
}

/// Why a decision can't be made now ([`Session::check`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// `Accept(k)` with no option `k` (0-based); there are `count`.
    NoOption { k: usize, count: usize },
    /// The line is gone ([`LineChange::Removed`]): nothing can be decided.
    LineRemoved { para: ParaId },
    /// The line was split or joined: accepting (or keeping the original over
    /// a decision) would write a whole-line text into part of the line.
    LineChanged { para: ParaId, how: LineChange },
    /// A pending block whose lines (starting at `para`) were split, partly
    /// removed or moved apart: accepting would replace lines it wasn't
    /// written for.
    Restructured { para: ParaId },
    /// A pending block whose lines are gone because a decision in another
    /// review removed them.
    RemovedBy { para: ParaId, review: String },
    /// Accepting a block over line `para`, which another review has decided
    /// (C9): undecide it there first.
    DecidedElsewhere { para: ParaId, review: String },
}

impl Refusal {
    /// The message, ending with `rewrite`: how this host rewrites a line
    /// (the app: "press E to rewrite this line"; the CLI: "use decide <n>
    /// rewrite <text>"). Only [`Refusal::LineChanged`] uses it.
    pub fn message(&self, rewrite: &str) -> String {
        match self {
            Self::NoOption { k, count } => format!("no option {} (1 to {count})", k + 1),
            Self::LineRemoved { para } => format!(
                "line removed: ¶{} was removed (or joined into the line above) since the suggestion was written, so \
                 it can't be decided",
                para.0
            ),
            Self::LineChanged { para, how } => {
                let how = if *how == LineChange::Split { "split" } else { "joined with the next line" };
                format!(
                    "line changed: ¶{} was {how} since the suggestion was written, so its text no longer fits the \
                     line; {rewrite}",
                    para.0
                )
            }
            Self::Restructured { para } => format!(
                "lines changed: the lines from ¶{} were split, partly removed or moved apart since the suggestion was \
                 written, so it can't be accepted; {rewrite}",
                para.0
            ),
            Self::RemovedBy { para, review } => {
                format!("line removed by {review}: ¶{} is gone, so this can't be decided", para.0)
            }
            Self::DecidedElsewhere { para, review } => {
                format!("¶{} is decided in {review}; undecide it there first", para.0)
            }
        }
    }
}

impl std::fmt::Display for Refusal {
    /// The neutral wording (tools and other hosts without a key for it).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message("rewrite it instead"))
    }
}

/// [`LineChange`]s of every paragraph a document has had, worked out in one
/// pass over its log ([`line_changes`]).
#[derive(Debug, Default)]
struct LineChanges {
    changed: HashMap<ParaId, LineChange>,
    live: HashSet<ParaId>,
    /// Each metadata key's last change: its op's position among the ops
    /// applied (a key set in the starting point has none).
    set_at: HashMap<String, usize>,
    /// Every split inside a line's text and every join, with its op's
    /// position, in order ([`LineChanges::tails_since`]).
    cuts: Vec<(usize, Cut)>,
}

/// A split or join, for following the lines a decision left.
#[derive(Debug, Clone, Copy)]
enum Cut {
    /// `para` split strictly inside its text; `tail` is the new paragraph.
    Split { para: ParaId, tail: ParaId },
    /// `next` joined onto `para`.
    Join { para: ParaId, next: ParaId },
}

impl LineChanges {
    fn of(&self, para: ParaId) -> Option<LineChange> {
        self.changed.get(&para).copied().or_else(|| (!self.live.contains(&para)).then_some(LineChange::Removed))
    }

    /// The live paragraphs split off `ids` (or off lines split off them)
    /// since metadata `key` last changed, and not joined back: text the
    /// writer cut from the lines a decision left, which still belongs to
    /// them. A split undoing a join onto them is not one.
    fn tails_since(&self, key: &str, ids: &[ParaId]) -> Vec<ParaId> {
        let since = self.set_at.get(key).copied();
        let mut tracked: HashSet<ParaId> = ids.iter().copied().collect();
        let (mut tails, mut joined): (Vec<ParaId>, HashSet<ParaId>) = (vec![], HashSet::new());
        for (_, cut) in self.cuts.iter().filter(|(n, _)| since.is_none_or(|s| *n > s)) {
            match *cut {
                Cut::Split { para, tail } if tracked.contains(&para) => {
                    if !joined.remove(&tail) {
                        tracked.insert(tail);
                        tails.push(tail);
                    }
                }
                Cut::Join { para, next } if tracked.contains(&para) => {
                    if let Some(j) = tails.iter().position(|t| *t == next) {
                        tails.remove(j);
                        tracked.remove(&next);
                    } else {
                        joined.insert(next);
                    }
                }
                _ => {}
            }
        }
        tails.retain(|t| self.live.contains(t));
        tails
    }
}

/// What a document is when its line changes were last worked out: the log's
/// length (it only grows), plus the document's origin (a rebase starts a new one, however alike
/// the counts) and a little more besides.
type CacheKey = (u64, usize, u64, usize);

fn cache_key(doc: &Document) -> CacheKey {
    (doc.origin(), doc.log().len(), doc.next_id(), doc.paragraphs().len())
}

/// [`Session`]'s cache of [`LineChanges`]. Clones start empty.
#[derive(Debug, Default)]
struct LineCache(Mutex<Option<(CacheKey, Arc<LineChanges>)>>);

impl Clone for LineCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

/// What has happened to one paragraph's line, while scanning the log.
#[derive(Default)]
struct Track {
    /// Paragraphs split off it (cutting its text) and not joined back.
    tails: Vec<ParaId>,
    /// Paragraphs joined onto it and not split off again.
    joined: Vec<ParaId>,
    /// Its kind before its first retype.
    first_kind: Option<ElementKind>,
}

/// Every paragraph's [`LineChange`] in `doc`, from the ops applied since
/// its starting point (undo and redo included): one pass, tracking each
/// paragraph's length to tell a split inside the text from Enter at its end.
fn line_changes(doc: &Document) -> LineChanges {
    use scriptorium_core::spans;
    let mut len: HashMap<ParaId, usize> =
        doc.initial().paragraphs.iter().map(|p| (p.id, spans::len(&p.spans))).collect();
    let mut tracks: HashMap<ParaId, Track> = HashMap::new();
    let (mut set_at, mut cuts) = (HashMap::new(), vec![]);
    let mut n = 0;
    doc.for_each_applied_op(|op| {
        n += 1;
        match op {
            Op::Splice { para, removed, inserted, .. } => {
                let l = len.entry(*para).or_default();
                *l = (*l + spans::len(inserted)).saturating_sub(spans::len(removed));
            }
            Op::Split { para, at, new_para, .. } => {
                let l = len.get(para).copied().unwrap_or_default();
                if *at > 0 && *at < l {
                    cuts.push((n, Cut::Split { para: *para, tail: *new_para }));
                }
                let t = tracks.entry(*para).or_default();
                if let Some(j) = t.joined.iter().position(|p| p == new_para) {
                    t.joined.swap_remove(j); // a join undone
                } else if *at > 0 && *at < l {
                    t.tails.push(*new_para);
                }
                len.insert(*para, (*at).min(l));
                len.insert(*new_para, l.saturating_sub(*at));
            }
            Op::Merge { para, next, .. } => {
                cuts.push((n, Cut::Join { para: *para, next: *next }));
                let t = tracks.entry(*para).or_default();
                if let Some(j) = t.tails.iter().position(|p| p == next) {
                    t.tails.swap_remove(j); // a split joined back: no change
                } else {
                    t.joined.push(*next);
                }
                let n = len.get(next).copied().unwrap_or_default();
                *len.entry(*para).or_default() += n;
            }
            Op::SetKind { para, from, .. } => {
                tracks.entry(*para).or_default().first_kind.get_or_insert_with(|| from.clone());
            }
            Op::InsertPara { para, .. } => {
                len.insert(para.id, spans::len(&para.spans));
            }
            Op::SetMeta { key, .. } => {
                set_at.insert(key.clone(), n);
            }
            Op::RemovePara { .. } => {}
        }
    });
    let live: HashMap<ParaId, &ElementKind> = doc.paragraphs().iter().map(|p| (p.id, &p.kind)).collect();
    let mut changed = HashMap::new();
    for (id, t) in &tracks {
        let Some(kind) = live.get(id) else { continue };
        let change = if t.tails.iter().any(|p| live.contains_key(p)) {
            LineChange::Split
        } else if t.joined.iter().any(|p| !live.contains_key(p)) {
            LineChange::Joined
        } else if t.first_kind.as_ref().is_some_and(|k| k != *kind) {
            LineChange::Retyped
        } else {
            continue;
        };
        changed.insert(*id, change);
    }
    LineChanges { changed, live: live.into_keys().collect(), set_at, cuts }
}

/// A suggestion matched to a paragraph, or (a block) to a run of them.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// The first of [`Self::paras`]; for a block placed where it cut lines
    /// (no `paras`), the first line it was made on (a gone line).
    pub para: ParaId,
    /// The paragraphs it was placed on when the review was opened: while
    /// pending, the lines it was written for; a block placed from its
    /// decision (C1), the lines that decision left (empty for a cut). Where
    /// it is now: [`Session::live_run`].
    pub paras: Vec<ParaId>,
    /// The paragraph's position in the live script it was matched against.
    pub index: usize,
    /// Which entry of the review file it came from.
    pub entry: usize,
    /// The text the entry's `BEFORE` was matched against (what the suggestion
    /// is for, and what undoing a decision goes back to): the paragraph's
    /// original text, else its live text; never the text after a decision.
    ///
    /// Legacy one-line field, for display only: for a block, the texts of
    /// [`Self::before_lines`] joined with `" / "` (kinds dropped). Prefer
    /// [`Self::before_lines`] (and [`Self::before_paras`] for ids).
    pub before: String,
    /// Replacement texts that would visibly change the paragraph, in order.
    ///
    /// Legacy one-line field, for display only: for a block, each option's
    /// [`Self::option_lines`] texts joined with `" / "` (kinds dropped; a cut
    /// is `""`). Prefer [`Self::option_lines`] and [`Self::shape`].
    pub options: Vec<String>,
    /// `options[i]` (and `option_lines[i]`) is the file's `AFTER` number
    /// `option_src[i]` (0-based): kept in step with [`Self::option_lines`],
    /// which is the field to read options from.
    pub option_src: Vec<usize>,
    pub why: String,
    /// Matched approximately: the `BEFORE` text wasn't found exactly.
    pub fuzzy: bool,
    /// A block suggestion (any entry but one untyped line, see
    /// [`Entry::is_line`]): decided with `replace_run` / `restore_run`, and
    /// its decision values name every line.
    pub block: bool,
    /// [`Self::before`] as lines, every kind filled in.
    pub before_lines: Vec<Line>,
    /// [`Self::options`] as lines, every kind filled in (untyped lines take
    /// the kind of the `BEFORE` line in their place).
    pub option_lines: Vec<Vec<Line>>,
    /// What the suggestion was made against, with the ids of its lines (C2):
    /// the entry's `BEFORE` lines (untyped ones with the matched paragraph's
    /// kind), as the paragraphs read when matched (so styles survive). Keep
    /// mine and undecide put these back.
    pub before_paras: Vec<Paragraph>,
    /// The entry's `ROW:` / `ROW TEXT:` (a suggestion for a row of a pair of
    /// versions), as written; opaque to review.
    pub row: Option<RowRef>,
}

impl Item {
    /// The paragraph the item is on (for a block, its first).
    pub fn para(&self) -> ParaId {
        self.para
    }

    /// The ids of the lines it was made on ([`Self::before_paras`]).
    pub fn before_ids(&self) -> Vec<ParaId> {
        self.before_paras.iter().map(|p| p.id).collect()
    }

    /// What option `option` (an index into [`Self::option_lines`]) does to
    /// the lines it was made on. The one place the addition rule lives: a
    /// block on one line whose option has several lines, the first of which
    /// is that line unchanged (same kind, same text up to whitespace, as
    /// `replace_run` pairs lines), adds the rest after it.
    ///
    /// Panics if there is no such option.
    pub fn shape(&self, option: usize) -> Shape {
        let lines = &self.option_lines[option];
        if !self.block {
            return Shape::Line;
        }
        let n = self.before_lines.len();
        let m = lines.len();
        if m == 0 {
            return Shape::Cut { n };
        }
        let same = |a: &Line, b: &Line| a.kind == b.kind && normalize(&a.text) == normalize(&b.text);
        if n == 1
            && m >= 2
            && same(&lines[0], &self.before_lines[0])
            && let Some(anchor) = self.before_paras.first()
        {
            return Shape::Add { after: anchor.id, n: m - 1 };
        }
        Shape::Replace { n, m }
    }
}

/// What one option of an [`Item`] does ([`Item::shape`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// A line suggestion (not a block): one line's text replaced.
    Line,
    /// The `n` lines it was made on are cut (the option has no lines).
    Cut { n: usize },
    /// `n` new lines added after line `after`, which stays as it is (the
    /// option's first line is that line unchanged).
    Add { after: ParaId, n: usize },
    /// The `n` lines it was made on replaced by `m` lines.
    Replace { n: usize, m: usize },
}

/// Where an item is now ([`Session::current_run`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CurrentRun {
    /// The live lines it covers ([`Session::live_run`]); empty for a cut,
    /// or once its lines are gone.
    pub ids: Vec<ParaId>,
    /// The live line just before where it is (or, with no `ids`, where its
    /// lines were: a cut's anchor, else the line now before where they
    /// stood); `None` at the start of the script.
    pub after: Option<ParaId>,
}

/// A review-file entry whose paragraph wasn't found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmatched {
    pub entry: usize,
    pub before: String,
    /// Why, when known (blocks).
    pub why: Option<Unplaced>,
}

#[derive(Debug, Clone)]
pub struct Review {
    /// Identifies the review: the file's `Id:` header, else the first 12 hex
    /// digits of [`crate::sessionfile::hash`] of its text.
    pub id: String,
    pub name: String,
    /// Where the file was opened from, as the session file stores it (see
    /// [`crate::sessionfile::stored_path`]). Empty if it doesn't matter.
    pub path: String,
    pub file: ReviewFile,
    /// In script order when opened; items added by [`Session::append_entry`]
    /// go at the end, so item indices held elsewhere stay valid.
    pub items: Vec<Item>,
    pub unmatched: Vec<Unmatched>,
    /// Entries whose every option only changes what the script doesn't show
    /// (e.g. letter case in a character name). Not reviewed.
    pub invisible: Vec<usize>,
    pub warnings: Vec<String>,
    /// The file's text as read (and as extended by [`Session::append_entry`]
    /// or [`Review::pin_id`]), for splicing into rather than reserializing.
    text: String,
    /// Decisions saved in the file, per entry index.
    restore: Vec<(usize, Decision)>,
}

/// Where an entry ended up after matching.
enum Placed {
    Item(Box<Item>),
    Invisible,
    Unmatched(Option<Miss>),
    /// A `ROW:` entry with no row: the resolver's reason.
    NoRow(String),
}

/// The one way an entry and its match become an [`Item`] (shared by
/// [`Review::open`] and [`Session::append_entries`]). `kind_of` gives a
/// paragraph's kind (live, else original).
fn place(entry: usize, e: &Entry, m: Found, kind_of: &dyn Fn(ParaId) -> Option<ElementKind>) -> Placed {
    match m {
        Found::None(miss) => Placed::Unmatched(miss),
        Found::NoRow(why) => Placed::NoRow(why),
        Found::Line(m) if m.options.is_empty() => Placed::Invisible,
        Found::Block(m) if m.options.is_empty() => Placed::Invisible,
        Found::Line(m) => {
            let kind = kind_of(m.para).unwrap_or(ElementKind::Action);
            Placed::Item(Box::new(Item {
                para: m.para,
                paras: vec![m.para],
                index: m.index,
                entry,
                before_lines: vec![Line::typed(kind.clone(), m.before.clone())],
                option_lines: m.options.iter().map(|o| vec![Line::typed(kind.clone(), o.clone())]).collect(),
                before_paras: vec![Paragraph::plain(m.para, kind, m.before.clone())],
                before: m.before,
                options: m.options,
                option_src: m.option_src,
                why: e.why.clone(),
                fuzzy: m.fuzzy,
                block: false,
                row: e.row.clone(),
            }))
        }
        Found::Block(m) => Placed::Item(Box::new(Item {
            para: m.paras.first().copied().unwrap_or(m.before[0].id),
            paras: m.paras,
            index: m.index,
            entry,
            before: m.before.iter().map(|p| p.text()).collect::<Vec<_>>().join(" / "),
            options: m.options.iter().map(|o| Line::join(o)).collect(),
            option_src: m.option_src,
            why: e.why.clone(),
            fuzzy: m.fuzzy,
            block: true,
            before_lines: m.before.iter().map(|p| Line::typed(p.kind.clone(), p.text())).collect(),
            option_lines: m.options,
            before_paras: m.before,
            row: e.row.clone(),
        })),
    }
}

fn fulls<'a>(
    paragraphs: &'a [Paragraph],
    texts: &'a [String],
    is_caps: &dyn Fn(&ElementKind) -> bool,
) -> Vec<Full<'a>> {
    paragraphs
        .iter()
        .zip(texts)
        .map(|(p, t)| Full {
            para: Para { id: p.id, text: t, editable: p.kind.is_editable(), caps: is_caps(&p.kind) },
            source: p,
        })
        .collect()
}

/// [`Unplaced`] for a block's [`Miss`]; `claimed` as in [`unplaced`].
fn block_unplaced(miss: Miss, claimed: &HashMap<ParaId, (usize, Option<usize>)>) -> Unplaced {
    match miss {
        Miss::Taken(id) => match claimed.get(&id) {
            Some(&(entry, item)) => Unplaced::Taken { entry, item },
            None => Unplaced::Mismatch,
        },
        Miss::NoPara => Unplaced::NoPara,
        Miss::Missing => Unplaced::Missing,
        Miss::NotEditable => Unplaced::NotEditable,
        Miss::Mismatch => Unplaced::Mismatch,
        Miss::Ambiguous => Unplaced::Ambiguous,
        Miss::Restructured => Unplaced::Restructured,
    }
}

/// Paragraphs of `doc` from the first to the last live of `ids` (lines
/// added between them included); empty if none is live.
fn live_span(doc: &Document, ids: &[ParaId]) -> Vec<ParaId> {
    let pos: Vec<usize> = ids.iter().filter_map(|id| doc.index_of(*id)).collect();
    match (pos.iter().min(), pos.iter().max()) {
        (Some(&a), Some(&b)) => doc.paragraphs()[a..=b].iter().map(|p| p.id).collect(),
        _ => vec![],
    }
}

/// `lines` (kinds filled in) as new paragraphs.
fn new_paras(lines: &[Line], before: &[Paragraph]) -> Vec<NewPara> {
    typed(lines, before).into_iter().map(|l| NewPara::plain(l.kind.expect("typed"), l.text)).collect()
}

/// What a block decision left, from what `replace_run` did.
fn now_of(r: &Replaced) -> Now {
    if r.ids.is_empty() { r.after.map_or(Now::Start, Now::After) } else { Now::Ids(r.ids.clone()) }
}

/// Why `e` (matched by [`Session::append_entries`]) found no paragraph.
/// `claimed`: paragraphs taken by the review's items or earlier entries of the
/// batch, with the entry that took each and the item it became.
fn unplaced(e: &Entry, para_only: bool, live: &[Para], claimed: &HashMap<ParaId, (usize, Option<usize>)>) -> Unplaced {
    let taken = |&(entry, item): &(usize, Option<usize>)| Unplaced::Taken { entry, item };
    let named = e.para.first().map(|&id| live.iter().find(|p| p.id == id));
    if let Some(Some(p)) = named
        && let Some(c) = claimed.get(&p.id)
    {
        return taken(c);
    }
    // A `ROW:` entry is placed by its row alone, never by its text.
    if !para_only && e.row.is_none() {
        // No free line fits BEFORE: say so if a taken one does, exactly or
        // (as matching would have) approximately.
        let before = Line::join(&e.before);
        let key = normalize(&before);
        let claimed_lines = || live.iter().filter(|p| p.editable).filter_map(|p| claimed.get(&p.id).map(|c| (p, c)));
        if let Some((_, c)) = claimed_lines().find(|(p, _)| normalize(p.text) == key) {
            return taken(c);
        }
        let fuzzy = claimed_lines()
            .filter(|(p, _)| !key.is_empty() && !p.text.trim().is_empty())
            .map(|(p, c)| (similarity(&before, p.text), c))
            .filter(|(s, _)| *s >= FUZZY_THRESHOLD)
            .max_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, c)) = fuzzy {
            return taken(c);
        }
    }
    match named {
        None if para_only => Unplaced::NoPara,
        None | Some(None) => Unplaced::Missing,
        Some(Some(p)) if !p.editable => Unplaced::NotEditable,
        Some(Some(_)) => Unplaced::Mismatch,
    }
}

/// Where paragraph `id`, which `doc` has had (in `order`, see
/// [`Document::history_order`]) but no longer has, would be now: the live position of
/// the next line after it in `order` that still exists, else the end.
/// `None` if `doc` never had it.
fn gone_position(order: &[ParaId], doc: &Document, id: ParaId) -> Option<usize> {
    let at = order.iter().position(|&p| p == id)?;
    Some(order[at + 1..].iter().find_map(|&p| doc.index_of(p)).unwrap_or(doc.paragraphs().len()))
}

impl Review {
    /// The review id a file's text gives: its `Id:` header, else the first 12
    /// hex digits of [`crate::sessionfile::hash`] of the text.
    pub fn id_of(text: &str) -> String {
        Self::id_from(&ReviewFile::parse(text), text)
    }

    fn id_from(file: &ReviewFile, text: &str) -> String {
        file.id.clone().unwrap_or_else(|| crate::sessionfile::hash(text)[..12].to_string())
    }

    /// Read a review file and match it against `script`, an unedited script
    /// (as read from its file). `script_name` is the file name the script was
    /// opened from, to check the file's `Script:` header.
    ///
    /// A thin wrapper for pasted files and tests: [`Self::open_live`] with no
    /// edits since the script was read. Hosts with a session (whose document
    /// may have been edited) must use [`Self::open_live`], or a suggestion on
    /// an edited or new line isn't found.
    pub fn open(
        name: impl Into<String>,
        text: &str,
        script: &Script,
        is_caps: impl Fn(&ElementKind) -> bool,
        script_name: &str,
    ) -> Self {
        let texts: Vec<String> = script.paragraphs.iter().map(|p| p.text()).collect();
        let paras = fulls(&script.paragraphs, &texts, &is_caps);
        Self::build(name.into(), text, &paras, &paras, &|_, _| false, &|_| None, &|_| None, None, script_name)
    }

    /// [`Self::open`] for a script that may have been edited since it was
    /// read: `original` is the script as read from its file, `live` the
    /// document now. Entries are matched by `PARA:` against the live document
    /// and by `BEFORE` against the original (see [`crate::matching::match_live`]);
    /// a block already decided is placed from its decision (see
    /// [`crate::matching`]). [`Item::index`] is the live position.
    ///
    /// `ROW:` entries (suggestions for rows of a pair of versions) are placed
    /// through `rows`, built by the host for `live` (see [`RowResolver`]):
    /// only through it, never by `PARA:` or `BEFORE`. An entry is placed on
    /// the first candidate none of whose lines another entry of the review
    /// has, decided entries before pending ones; its status comes from its
    /// decision as for any entry. No candidate (or no `rows`): unmatched with
    /// the reason ([`Unplaced::NoRow`]).
    pub fn open_live(
        name: impl Into<String>,
        text: &str,
        original: &Script,
        live: &Document,
        is_caps: impl Fn(&ElementKind) -> bool,
        script_name: &str,
        rows: Option<&RowResolver>,
    ) -> Self {
        let original_texts: Vec<String> = original.paragraphs.iter().map(|p| p.text()).collect();
        let live_texts: Vec<String> = live.paragraphs().iter().map(|p| p.text()).collect();
        let original_paras = fulls(&original.paragraphs, &original_texts, &is_caps);
        let live_paras = fulls(live.paragraphs(), &live_texts, &is_caps);
        let id = Self::id_of(text);
        let has_meta = |entry: usize, para: ParaId| {
            live.meta(&format!("review/{id}/{entry}")).and_then(|v| MetaValue::on(v, para)).is_some()
        };
        let decided = |entry: usize| {
            live.meta(&format!("review/{id}/{entry}")).and_then(MetaValue::parse).filter(|v| v.paras.is_some())
        };
        // Worked out only if an entry is on a gone line.
        let history = std::cell::OnceCell::new();
        let gone_at = |p: ParaId| gone_position(history.get_or_init(|| live.history_order()), live, p);
        Self::build(name.into(), text, &original_paras, &live_paras, &has_meta, &decided, &gone_at, rows, script_name)
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        name: String,
        text: &str,
        original: &[Full],
        live: &[Full],
        has_meta: &dyn Fn(usize, ParaId) -> bool,
        decided: &dyn Fn(usize) -> Option<MetaValue>,
        gone_at: &dyn Fn(ParaId) -> Option<usize>,
        rows: Option<&RowResolver>,
        script_name: &str,
    ) -> Self {
        let file = ReviewFile::parse(text);
        let mut warnings = file.warnings.clone();
        if !file.script.is_empty()
            && !script_name.is_empty()
            && normalize(&file.script).to_lowercase() != normalize(script_name).to_lowercase()
        {
            warnings.push(format!(
                "The review file says it is for \"{}\", but the script opened is \"{script_name}\".",
                file.script
            ));
        }
        let matches =
            match_review(original, live, &file.entries, &HashSet::new(), false, has_meta, decided, gone_at, rows);
        let kinds: HashMap<ParaId, &ElementKind> =
            original.iter().chain(live).map(|f| (f.para.id, &f.source.kind)).collect();
        let kind_of = |id: ParaId| kinds.get(&id).map(|k| (*k).clone());

        let (mut items, mut unmatched, mut invisible, mut restore) = (vec![], vec![], vec![], vec![]);
        let mut misses: Vec<(usize, Miss)> = vec![];
        // Gone lines sort before the surviving line at their position.
        let mut gone: HashSet<usize> = HashSet::new();
        for (entry, (e, m)) in file.entries.iter().zip(matches).enumerate() {
            if match &m {
                Found::Line(m) => m.gone,
                Found::Block(m) => m.gone,
                Found::None(_) | Found::NoRow(_) => false,
            } {
                gone.insert(entry);
            }
            let item = match place(entry, e, m, &kind_of) {
                Placed::Unmatched(miss) => {
                    misses.extend(miss.map(|m| (unmatched.len(), m)));
                    unmatched.push(Unmatched { entry, before: Line::join(&e.before), why: None });
                    continue;
                }
                Placed::NoRow(why) => {
                    unmatched.push(Unmatched { entry, before: Line::join(&e.before), why: Some(Unplaced::NoRow(why)) });
                    continue;
                }
                Placed::Invisible => {
                    invisible.push(entry);
                    continue;
                }
                Placed::Item(item) => item,
            };
            // Only decisions that can apply: an accepted option that would
            // change nothing visible (filtered out of the item) is dropped.
            let accepted = || item.option_src.iter().position(|&s| s == e.chosen.unwrap_or(0)).map(Decision::Accept);
            let decision = match (e.status, &e.mine) {
                // A block's `[cut]` is the lines it left: none.
                (FileStatus::Rewritten, Some(mine)) if item.block => Some(Decision::RewriteLines(mine.clone())),
                // A cut line (A7): never written in as text.
                (FileStatus::Rewritten, Some(mine)) if mine.is_empty() => None,
                (FileStatus::Rewritten, Some(mine)) => Some(Decision::Rewrite(Line::join(mine))),
                (FileStatus::Rejected, _) => Some(Decision::KeepMine),
                (FileStatus::Accepted, _) => accepted(),
                _ => None,
            };
            if let Some(d) = decision {
                restore.push((entry, d));
            }
            items.push(*item);
        }
        items.sort_by_key(|it| (it.index, !gone.contains(&it.entry)));
        let claimed: HashMap<ParaId, (usize, Option<usize>)> = items
            .iter()
            .enumerate()
            .flat_map(|(i, it)| it.paras.iter().map(move |&p| (p, (it.entry, Some(i)))))
            .collect();
        for (u, miss) in misses {
            unmatched[u].why = Some(block_unplaced(miss, &claimed));
        }
        let id = Self::id_from(&file, text);
        Self {
            id,
            name,
            path: String::new(),
            file,
            items,
            unmatched,
            invisible,
            warnings,
            text: text.to_string(),
            restore,
        }
    }

    /// The file's text as this review last read or wrote it (including
    /// appends and an inserted `Id:`).
    ///
    /// Shells must compare it with the file on disk before writing the
    /// review file (an append, or progress over the same file): another
    /// process (the CLI, an agent) may have changed the file since. If they
    /// differ, reload the review or refuse the write; never overwrite.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// For a file without an `Id:` header: write the review's current id into
    /// its text (one inserted line, see [`crate::file::insert_id`]) and return
    /// the new text for the caller to save, so later edits to the file don't
    /// change the id and orphan its decisions. `None` if it already has one.
    /// Shells call this when a review is attached.
    pub fn pin_id(&mut self) -> Option<String> {
        if self.file.id.is_some() {
            return None;
        }
        self.text = insert_id(&self.text, &self.id);
        self.file.id = Some(self.id.clone());
        Some(self.text.clone())
    }

    /// The shell wrote `text` to this review's file itself (e.g. progress
    /// from [`Session::save_progress`] over the same file): remember it as the
    /// file's text, so [`Self::text`] and the hash the session records (see
    /// [`Session::attached`]) match the file. `text` must hold the same
    /// entries in the same order (as progress does); only its text is kept.
    ///
    /// Fails, changing nothing, if `text` holds a different number of
    /// entries: entry positions are the decision keys, so such a file isn't
    /// progress over this review and must be reopened instead.
    pub fn wrote_progress(&mut self, text: impl Into<String>) -> Result<(), String> {
        let text = text.into();
        let file = ReviewFile::parse(&text);
        if file.entries.len() != self.file.entries.len() {
            return Err(format!(
                "{}: the text written has {} suggestions, the review has {}; reopen the review",
                self.name,
                file.entries.len(),
                self.file.entries.len()
            ));
        }
        self.text = text;
        self.file = file;
        Ok(())
    }

    /// Record where the file was opened from (kept in the session file).
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    /// How many decisions saved in the file can be applied (on suggestions
    /// that matched a line, with a visible option).
    pub fn restored(&self) -> usize {
        self.restore.len()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    pub total: usize,
    pub pending: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub rewritten: usize,
    /// Pending suggestions whose line is gone ([`Status::Removed`]): neither
    /// pending nor decided, since they can't be decided.
    pub removed: usize,
}

impl Progress {
    /// How many are decided (accepted, kept or rewritten).
    pub fn done(&self) -> usize {
        self.total - self.pending - self.removed
    }
}

/// What [`Session::append_entry`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Appended {
    /// The review file's new contents, for the caller to save.
    pub file_text: String,
    /// The new item's index in [`Review::items`]; `None` if the entry didn't
    /// match a paragraph (and wasn't written) or none of its options would be
    /// visible.
    pub item: Option<usize>,
}

/// What [`Session::append_entries`] did with one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendedEntry {
    /// The new item's index in [`Review::items`]; `None` if the entry didn't
    /// match a paragraph or none of its options would be visible.
    pub item: Option<usize>,
    /// The entry's index in the file (the stable part of its decision key);
    /// `None` if it matched no paragraph and so wasn't written.
    pub entry: Option<usize>,
    /// Why there's no item; `None` when there is one.
    pub unplaced: Option<Unplaced>,
}

/// Why an entry given to [`Session::append_entries`] got no item. Only
/// [`Unplaced::Invisible`] entries are written to the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unplaced {
    /// Its paragraph (the one its `PARA:` names, or the only lines its
    /// `BEFORE` fits) already has a suggestion in this review: `items[item]`,
    /// or (`None`) an earlier entry of the same batch that got no item
    /// because it was invisible. `entry` is the file entry that claimed it
    /// (stable, unlike `item`: the `e<entry>` hosts name). Not written.
    Taken { entry: usize, item: Option<usize> },
    /// Matching by `PARA:` only, and the entry has none. Not written.
    NoPara,
    /// Its `PARA:` paragraph doesn't exist (any more), or, without `PARA:`,
    /// no line fits its `BEFORE`. Not written.
    Missing,
    /// Its `PARA:` paragraph can't take suggestions (e.g. "End of Act"), and
    /// no other line fits. Not written.
    NotEditable,
    /// Its `PARA:` paragraph exists but its `BEFORE` doesn't fit it, and no
    /// other line fits. Not written.
    Mismatch,
    /// Matched a paragraph, but none of its options would visibly change it
    /// (e.g. only letter case in a character name). **Written** to the file
    /// (with the paragraph matched as its `PARA:`), but not reviewed.
    Invisible,
    /// A block without `PARA:` whose `BEFORE` fits several runs equally
    /// well. Not written.
    Ambiguous,
    /// A block whose lines were split, partly removed or moved apart since.
    /// Not written.
    Restructured,
    /// It can't be written so that it reads back the same
    /// ([`Entry::check`]; e.g. an untyped line in an option of several
    /// lines). Not written. Holds [`Entry::check`]'s reason.
    Invalid(String),
    /// Opening only ([`Unmatched::why`]): a pending block whose lines are
    /// all gone because a decision in another attached review (index
    /// `review` in [`Session::reviews`]) cut them. Set by
    /// [`Session::note_removed_by`]; named by [`Session::unplaced_label`].
    RemovedBy { review: usize },
    /// A `ROW:` entry (a suggestion for a row of a pair of versions) that
    /// the host's resolver placed on no row, with its reason as a sentence
    /// ("the English line changed since this suggestion"), or no resolver
    /// was given ([`crate::matching::NO_LINKS`]). Not written.
    NoRow(String),
}

/// What [`Session::append_entries`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendedMany {
    /// The review file's new contents, for the caller to save (unchanged if
    /// nothing was written).
    pub file_text: String,
    /// One per entry given, in order.
    pub results: Vec<AppendedEntry>,
}

/// Every review opened on one script.
#[derive(Debug, Clone, Default)]
pub struct Session {
    pub reviews: Vec<Review>,
    /// Line changes of the document last asked about (see [`cache_key`]).
    lines: LineCache,
}

impl Session {
    pub fn new(mut reviews: Vec<Review>) -> Self {
        // Two identical review files would share an id; keep them apart.
        let mut seen = std::collections::HashSet::new();
        for r in &mut reviews {
            let base = r.id.clone();
            let mut n = 1;
            while !seen.insert(r.id.clone()) {
                n += 1;
                r.id = format!("{base}-{n}");
            }
        }
        Self { reviews, lines: LineCache::default() }
    }

    pub fn item(&self, r: usize, i: usize) -> &Item {
        &self.reviews[r].items[i]
    }

    /// The document metadata key holding the decision on item `i` of review
    /// `r`. Its values are built and read only through [`crate::meta`].
    pub fn key(&self, r: usize, i: usize) -> String {
        format!("review/{}/{}", self.reviews[r].id, self.item(r, i).entry)
    }

    /// Add a review opened on the same script. Returns its index, or an
    /// explanation if a review with the same id (same `Id:` header, or the
    /// same contents) is already open.
    pub fn add(&mut self, review: Review) -> Result<usize, String> {
        if let Some(open) = self.reviews.iter().find(|r| r.id == review.id) {
            return Err(if open.name == review.name {
                format!("{} is already open.", review.name)
            } else if review.text != open.text && (review.file.id.is_some() || open.file.id.is_some()) {
                format!(
                    "{} and {} share `Id: {}`, so {} wasn't added. Give one of them a different Id: line.",
                    review.name, open.name, review.id, review.name
                )
            } else {
                format!("{} is the same file as {}, which is already open.", review.name, open.name)
            });
        }
        self.reviews.push(review);
        Ok(self.reviews.len() - 1)
    }

    /// The review files to remember in the session file.
    /// Each records the hash of the review's [`Review::text`], which shells
    /// keep equal to the file on disk by writing it only through this library
    /// (appends, [`Review::pin_id`], [`Review::wrote_progress`]).
    pub fn attached(&self) -> Vec<crate::sessionfile::Attached> {
        self.reviews
            .iter()
            .map(|r| crate::sessionfile::Attached {
                path: r.path.clone(),
                id: r.id.clone(),
                text_hash: crate::sessionfile::hash(&r.text),
            })
            .collect()
    }

    /// Start a document for `script` with the decisions saved in the review
    /// files already applied. They are the starting point, so undo stops there.
    pub fn open_document(&self, script: Script) -> Document {
        self.open_document_with_base(script).0
    }

    /// [`Self::open_document`], plus the events that applied the saved
    /// decisions (to keep in the session file, so the starting point can be
    /// rebuilt without the review files).
    pub fn open_document_with_base(&self, script: Script) -> (Document, Vec<scriptorium_core::doc::Event>) {
        self.document_with_base(script, false)
    }

    /// The starting point, with decision values written as they were before
    /// paragraphs were recorded (`legacy`, for a version 1 session: its saved
    /// events check the values they replace).
    pub(crate) fn document_with_base(
        &self,
        script: Script,
        legacy: bool,
    ) -> (Document, Vec<scriptorium_core::doc::Event>) {
        let mut doc = Document::new(script);
        for r in 0..self.reviews.len() {
            self.restore_into(&mut doc, r, legacy);
        }
        let base = doc.log().to_vec();
        (doc.rebase(), base)
    }

    /// Apply the decisions saved in review `r`'s file to a document that
    /// already has history (a review added later): one undo step. Returns how
    /// many applied.
    pub fn apply_saved(&self, doc: &mut Document, r: usize) -> usize {
        let depth = doc.undo_depth();
        let n = self.restore_into(doc, r, false);
        doc.merge_undo_since(depth);
        n
    }

    fn restore_into(&self, doc: &mut Document, r: usize, legacy: bool) -> usize {
        let sel = Selection::cursor(doc.start());
        let review = &self.reviews[r];
        // A restore that doesn't apply (paragraph missing) is skipped.
        review
            .restore
            .iter()
            .filter_map(|(e, d)| review.items.iter().position(|it| it.entry == *e).map(|i| (i, d)))
            .filter(|(i, d)| self.decide_as(doc, sel, r, *i, (*d).clone(), legacy).is_ok())
            .count()
    }

    /// How many decisions saved in review `r`'s file (its `STATUS` lines)
    /// differ from what `doc` records. A pending suggestion in the file isn't
    /// a disagreement: the file is just older than the session.
    pub fn file_disagreements(&self, doc: &Document, r: usize) -> usize {
        let review = &self.reviews[r];
        review
            .restore
            .iter()
            .filter_map(|(e, d)| review.items.iter().position(|it| it.entry == *e).map(|i| (i, d)))
            .filter(|&(i, d)| {
                let agrees = match (d, self.status(doc, r, i)) {
                    (Decision::Accept(k), Status::Accepted(s)) => *k == s,
                    (Decision::KeepMine, Status::Rejected) => true,
                    (Decision::Rewrite(t), Status::Rewritten) => {
                        normalize(t) == normalize(&Self::text(doc, review.items[i].para))
                    }
                    (Decision::RewriteLines(lines), Status::Rewritten) => {
                        let mine = self.mine(doc, r, i);
                        lines.len() == mine.len()
                            && lines.iter().zip(&mine).all(|(a, b)| normalize(&a.text) == normalize(&b.text))
                    }
                    _ => false,
                };
                !agrees
            })
            .count()
    }

    /// Add one suggestion to the end of review `r`'s file: a thin wrapper
    /// over [`Self::append_entries`] with one entry, matched by `PARA:` then
    /// `BEFORE`. An unmatched entry is not written (`file_text` is then the
    /// file as it was); use [`Self::append_entries`] to learn why
    /// ([`AppendedEntry::unplaced`]).
    pub fn append_entry(
        &mut self,
        doc: &Document,
        r: usize,
        entry: Entry,
        is_caps: impl Fn(&ElementKind) -> bool,
    ) -> Result<Appended, String> {
        let many = self.append_entries(doc, r, vec![entry], false, is_caps, None)?;
        Ok(Appended { file_text: many.file_text, item: many.results[0].item })
    }

    /// Add suggestions to the end of review `r`'s file, in one write. Every
    /// entry is matched first (by `PARA:`, then `BEFORE`, as
    /// [`Review::open_live`] does), against the document as it is now
    /// (`doc`, which stands in for the original here too), skipping
    /// paragraphs this review already has a suggestion on and paragraphs
    /// claimed by earlier entries of the batch. Only matched entries are
    /// spliced onto the file (never reserialized); unmatched ones are not
    /// written. A matched entry is written with the paragraph it matched as
    /// its `PARA:` (and, if its `BEFORE` only fitted approximately, that
    /// paragraph's text as its `BEFORE`), so reopening finds the same line.
    /// Each matched entry's [`Item`] is pushed at the **end** of
    /// `items`, so the indices of existing items stay valid (reopening sorts
    /// by script position). The review's id doesn't change: a file without an
    /// `Id:` header gets one when something is written. The entries' own
    /// `STATUS`/`CHOSEN`/`MINE` are ignored here.
    ///
    /// With `para_only`, an entry is matched by its `PARA:` alone: one whose
    /// paragraph doesn't fit it (or without `PARA:`) is unmatched and not
    /// written, never placed by `BEFORE` on another line with the same text.
    /// Tools always pass `true`; reopening still falls back to `BEFORE`.
    ///
    /// Fails, writing nothing, if an entry has no `BEFORE` or no `AFTER`. An
    /// entry that wouldn't read back the same ([`Entry::check`]) gets
    /// [`Unplaced::Invalid`] and isn't written.
    ///
    /// Blocks are matched as [`Review::open_live`] matches them (every
    /// `PARA:` line, or a `BEFORE` window), written with every line's id as
    /// `PARA:`, and turn the file's header into `SCRIPT REVIEW v2`.
    ///
    /// `ROW:` entries are placed through `rows` (built by the host for
    /// `doc`) as [`Review::open_live`] places them, whatever `para_only`
    /// says; each is written with the lines it went on as its `PARA:`. With
    /// `rows`, a `ROW:` entry may have a blank `BEFORE` (`BEFORE: [empty]`, a
    /// blank line to fill in). Without it they are not placed
    /// ([`Unplaced::NoRow`]).
    pub fn append_entries(
        &mut self,
        doc: &Document,
        r: usize,
        entries: Vec<Entry>,
        para_only: bool,
        is_caps: impl Fn(&ElementKind) -> bool,
        rows: Option<&RowResolver>,
    ) -> Result<AppendedMany, String> {
        if r >= self.reviews.len() {
            return Err(format!("no review {r}"));
        }
        let mut read = Vec::with_capacity(entries.len());
        for entry in &entries {
            // A blank line to fill in: only a `ROW:` entry, placed by its row.
            let blank_ok = entry.row.is_some() && rows.is_some();
            let empty = if entry.is_line() {
                (Line::join(&entry.before).trim().is_empty() && !blank_ok)
                    || entry.options.iter().all(|o| Line::join(o).trim().is_empty())
            } else {
                // A block's option may be a cut (`[]`).
                (entry.before.iter().all(|l| l.text.trim().is_empty()) && !blank_ok) || entry.options.is_empty()
            };
            if empty {
                return Err("a suggestion needs a BEFORE and at least one AFTER".into());
            }
            if let Err(why) = entry.check() {
                read.push(Err(why));
                continue;
            }
            // What the file will say, as a reader sees the block (newlines flattened…).
            let Some(e) = ReviewFile::parse(&append_block("", entry, "x")).entries.pop() else {
                return Err("the suggestion couldn't be written as a review block".into());
            };
            read.push(Ok(e));
        }
        // Entries that can't round-trip are refused here, never written.
        let refused: Vec<Option<String>> = read.iter().map(|r| r.as_ref().err().cloned()).collect();
        let read: Vec<Entry> = read.into_iter().flatten().collect();
        // Lines this review covers: where each item was placed, and where it is now.
        let mut used: HashSet<ParaId> = HashSet::new();
        let mut claimed: HashMap<ParaId, (usize, Option<usize>)> = HashMap::new();
        for (i, it) in self.reviews[r].items.iter().enumerate() {
            for p in it.paras.iter().copied().chain(self.live_run(doc, r, i)) {
                used.insert(p);
                claimed.entry(p).or_insert((it.entry, Some(i)));
            }
        }
        let review = &mut self.reviews[r];
        let texts: Vec<String> = doc.paragraphs().iter().map(|p| p.text()).collect();
        let full = fulls(doc.paragraphs(), &texts, &is_caps);
        let live: Vec<Para> = full.iter().map(|f| f.para.clone()).collect();
        let kind_of = |id: ParaId| doc.paragraph(id).map(|p| p.kind.clone());
        // New entries have no decisions yet.
        let matches = match_review(&full, &full, &read, &used, para_only, &|_, _| false, &|_| None, &|_| None, rows);

        let mut file_text = review.text.clone();
        let mut results = Vec::with_capacity(refused.len());
        let mut matched = read.into_iter().zip(matches);
        for refused in refused {
            if let Some(why) = refused {
                results.push(AppendedEntry { item: None, entry: None, unplaced: Some(Unplaced::Invalid(why)) });
                continue;
            }
            let (mut e, m) = matched.next().expect("one match per entry read");
            let index = review.file.entries.len();
            // Written as matched, so reopening (PARA pass) finds the same lines.
            let paras = match &m {
                Found::Line(m) => {
                    e.para = vec![m.para];
                    if normalize(&Line::join(&e.before)) != normalize(&m.before) {
                        e.before = vec![Line::untyped(m.before.trim())];
                    }
                    vec![m.para]
                }
                Found::Block(m) => {
                    e.para = m.before.iter().map(|p| p.id).collect();
                    if m.fuzzy {
                        e.before = m.before.iter().map(|p| Line::typed(p.kind.clone(), p.text())).collect();
                    }
                    m.paras.clone()
                }
                Found::None(_) | Found::NoRow(_) => vec![],
            };
            let (item, unplaced) = match place(index, &e, m, &kind_of) {
                Placed::Unmatched(miss) => {
                    let why = match miss {
                        Some(miss) => block_unplaced(miss, &claimed),
                        None => unplaced(&e, para_only, &live, &claimed),
                    };
                    results.push(AppendedEntry { item: None, entry: None, unplaced: Some(why) });
                    continue;
                }
                Placed::NoRow(why) => {
                    results.push(AppendedEntry { item: None, entry: None, unplaced: Some(Unplaced::NoRow(why)) });
                    continue;
                }
                Placed::Invisible => {
                    review.invisible.push(index);
                    (None, Some(Unplaced::Invisible))
                }
                Placed::Item(item) => {
                    review.items.push(*item);
                    (Some(review.items.len() - 1), None)
                }
            };
            for p in paras {
                claimed.insert(p, (index, item));
            }
            file_text = append_block(&file_text, &e, &review.id);
            review.file.entries.push(e);
            results.push(AppendedEntry { item, entry: Some(index), unplaced });
        }
        if results.iter().any(|r| r.entry.is_some()) {
            review.file.id.get_or_insert_with(|| review.id.clone());
            review.text = file_text.clone();
        }
        Ok(AppendedMany { file_text, results })
    }

    fn text(doc: &Document, para: ParaId) -> String {
        doc.paragraph(para).map(|p| p.text()).unwrap_or_default()
    }

    /// A block item's decision value, if it was made on the item's lines.
    fn block_value(&self, doc: &Document, r: usize, i: usize) -> Option<MetaValue> {
        let ids = self.item(r, i).before_ids();
        doc.meta(&self.key(r, i)).and_then(MetaValue::parse).filter(|v| v.paras.as_deref() == Some(&ids[..]))
    }

    /// What a block decision left: the stored `=>` part, or (no `=>`, as
    /// keep mine writes) the lines it was made on.
    fn now(item: &Item, v: &MetaValue) -> Now {
        v.now.clone().unwrap_or_else(|| Now::Ids(item.before_ids()))
    }

    /// Where a block's next decision applies: its lines while pending, what
    /// its decision left once decided, with any text split off those lines
    /// since by hand (Enter inside the last line, say: the tail is still
    /// part of what the decision left).
    fn run(&self, doc: &Document, r: usize, i: usize) -> Run {
        let item = self.item(r, i);
        match self.block_value(doc, r, i).map(|v| Self::now(item, &v)) {
            None => Run::Ids(item.before_ids()),
            Some(Now::Ids(mut ids)) => {
                ids.extend(self.line_changes(doc).tails_since(&self.key(r, i), &ids));
                Run::Ids(ids)
            }
            Some(Now::After(a)) => Run::Empty { after: Some(a) },
            Some(Now::Start) => Run::Empty { after: None },
        }
    }

    /// Where item `i` of review `r` is now: its live lines
    /// ([`Self::live_run`]) and the live line before them. With no live
    /// lines (a decided cut, or lines gone), `after` is where to show it: a
    /// cut's anchor if live, else the live line now before where its lines
    /// stood (worked out from the document's history).
    pub fn current_run(&self, doc: &Document, r: usize, i: usize) -> CurrentRun {
        let ids = self.live_run(doc, r, i);
        let before = |pos: usize| pos.checked_sub(1).map(|p| doc.paragraphs()[p].id);
        if let Some(pos) = ids.first().and_then(|id| doc.index_of(*id)) {
            return CurrentRun { after: before(pos), ids };
        }
        let item = self.item(r, i);
        let run = if item.block { self.run(doc, r, i) } else { Run::Ids(vec![item.para]) };
        let gone = match run {
            Run::Empty { after: None } => return CurrentRun { ids, after: None },
            Run::Empty { after: Some(a) } if doc.index_of(a).is_some() => return CurrentRun { ids, after: Some(a) },
            // Where a gone anchor was: the line now just before that.
            Run::Empty { after: Some(a) } => a,
            Run::Ids(run) => run.first().copied().unwrap_or(item.para),
        };
        let order = doc.history_order();
        CurrentRun { after: gone_position(&order, doc, gone).and_then(before), ids }
    }

    /// The live lines item `i` of review `r` covers: a line suggestion's
    /// paragraph (if it still exists); a block's lines while pending, else
    /// the lines its decision left (from the first to the last still live,
    /// lines added between them included, and lines split off them by hand
    /// since; none for a cut).
    pub fn live_run(&self, doc: &Document, r: usize, i: usize) -> Vec<ParaId> {
        let item = self.item(r, i);
        if !item.block {
            return doc.paragraph(item.para).map(|p| vec![p.id]).unwrap_or_default();
        }
        match self.run(doc, r, i) {
            Run::Ids(ids) => live_span(doc, &ids),
            Run::Empty { .. } => vec![],
        }
    }

    /// The lines a block's [`Self::live_run`] holds, typed: what progress
    /// writes as `MINE` (`[]` is `[cut]`). For a line suggestion, its line's
    /// text, untyped (none if the line is gone).
    pub fn mine(&self, doc: &Document, r: usize, i: usize) -> Vec<Line> {
        let item = self.item(r, i);
        if !item.block {
            return doc.paragraph(item.para).map(|p| vec![Line::untyped(p.text())]).unwrap_or_default();
        }
        self.live_run(doc, r, i)
            .into_iter()
            .filter_map(|id| doc.paragraph(id))
            .map(|p| Line::typed(p.kind.clone(), p.text()))
            .collect()
    }

    /// Whether the block's decision `v` left exactly option `k`: its lines
    /// live, consecutive, and reading as the option; for a cut, no line it was
    /// made on is live.
    fn reads_as(&self, doc: &Document, item: &Item, v: &MetaValue, k: usize) -> bool {
        let Some(option) = item.option_lines.get(k) else { return false };
        match Self::now(item, v) {
            Now::Ids(ids) => {
                let pos: Vec<Option<usize>> = ids.iter().map(|id| doc.index_of(*id)).collect();
                ids.len() == option.len()
                    && pos.iter().all(Option::is_some)
                    && pos.windows(2).all(|w| w[1] == w[0].map(|a| a + 1))
                    && ids.iter().zip(option).all(|(id, l)| {
                        doc.paragraph(*id).is_some_and(|p| Some(&p.kind) == l.kind.as_ref() && p.text() == l.text)
                    })
            }
            Now::After(_) | Now::Start => {
                option.is_empty() && item.before_ids().iter().all(|id| doc.paragraph(*id).is_none())
            }
        }
    }

    /// Where item `i` of review `r` stands: its decision value under
    /// [`Self::key`] (see [`crate::meta`]), checked against the paragraph's
    /// current text (an accepted line edited since shows as rewritten). A
    /// gone line is [`Status::Removed`] if pending, else
    /// [`Status::Rewritten`].
    ///
    /// A block is accepted while the lines its decision left are live,
    /// consecutive and read as the option (a cut: none of its lines is
    /// live), else rewritten; pending with every line gone, removed; kept
    /// with every line gone, rewritten (cut).
    pub fn status(&self, doc: &Document, r: usize, i: usize) -> Status {
        let item = self.item(r, i);
        if item.block {
            let Some(v) = self.block_value(doc, r, i) else {
                let gone = item.before_ids().iter().all(|id| doc.paragraph(*id).is_none());
                return if gone { Status::Removed } else { Status::Pending };
            };
            return match v.decision {
                Stored::Rejected if self.live_run(doc, r, i).is_empty() => Status::Rewritten,
                Stored::Rejected => Status::Rejected,
                Stored::Accepted(k) if self.reads_as(doc, item, &v, k) => Status::Accepted(k),
                _ => Status::Rewritten,
            };
        }
        let gone = doc.paragraph(item.para).is_none();
        match doc.meta(&self.key(r, i)).and_then(|v| MetaValue::on(v, item.para)) {
            None if gone => Status::Removed,
            None => Status::Pending,
            // Decided, then the line was cut: the B3 rule for one line.
            Some(_) if gone => Status::Rewritten,
            Some(Stored::Rejected) => Status::Rejected,
            Some(Stored::Accepted(k)) if item.options.get(k).is_some_and(|o| *o == Self::text(doc, item.para)) => {
                Status::Accepted(k)
            }
            Some(_) => Status::Rewritten,
        }
    }

    /// Pending, but the paragraph has changed since the suggestion was written
    /// (by hand, or by another review), or was split, joined or retyped. A
    /// block: its lines no longer read as they did when it was matched, or
    /// aren't all there and consecutive.
    pub fn is_outdated(&self, doc: &Document, r: usize, i: usize) -> bool {
        let item = self.item(r, i);
        if self.status(doc, r, i) != Status::Pending {
            return false;
        }
        if item.block {
            let ids = item.before_ids();
            return live_span(doc, &ids) != ids
                || item
                    .before_paras
                    .iter()
                    .any(|b| doc.paragraph(b.id).is_none_or(|p| p.kind != b.kind || p.text() != b.text()));
        }
        Self::text(doc, item.para) != item.before || self.line_change(doc, r, i).is_some()
    }

    /// How item `i`'s line has changed structurally, if it has (see
    /// [`LineChange`]), read from the document's paragraphs and the ops its
    /// log applied (undo and redo included):
    /// - [`LineChange::Removed`]: the paragraph doesn't exist.
    /// - [`LineChange::Split`]: the paragraph was split at a point strictly
    ///   inside its text as it was then (not at its end, which is Enter after
    ///   the line), the tail still exists and wasn't joined back.
    /// - [`LineChange::Joined`]: another line was joined onto it, wasn't split
    ///   off again, and doesn't exist on its own.
    /// - [`LineChange::Retyped`]: its type differs from its type before the
    ///   first retype. A retype made before the suggestion was written (in
    ///   the same session) counts too; the log doesn't say when the
    ///   suggestion was written.
    ///
    /// The log runs from the session's starting point, which is saved, so the
    /// answer survives reopening. Worked out for every paragraph in one pass
    /// and cached until the document changes.
    ///
    /// A block has only [`LineChange::Removed`]: none of the lines it covers
    /// ([`Self::live_run`]) is live (removed by hand, or cut by its decision).
    /// Other changes to its lines show as outdated or rewritten.
    pub fn line_change(&self, doc: &Document, r: usize, i: usize) -> Option<LineChange> {
        let item = self.item(r, i);
        if item.block {
            return self.live_run(doc, r, i).is_empty().then_some(LineChange::Removed);
        }
        self.line_changes(doc).of(item.para)
    }

    fn line_changes(&self, doc: &Document) -> Arc<LineChanges> {
        let key = cache_key(doc);
        let mut cache = self.lines.0.lock().unwrap_or_else(|e| e.into_inner());
        match &*cache {
            Some((k, changes)) if *k == key => changes.clone(),
            _ => {
                let changes = Arc::new(line_changes(doc));
                *cache = Some((key, changes.clone()));
                changes
            }
        }
    }

    /// The status as every host names it: "pending", "accepted <n>", "kept",
    /// "rewritten", "line cut" (decided, then the line went) or "line
    /// removed" (pending, the line went).
    pub fn status_label(&self, doc: &Document, r: usize, i: usize) -> String {
        match self.status(doc, r, i) {
            Status::Pending => "pending".into(),
            Status::Accepted(k) => format!("accepted {}", k + 1),
            Status::Rejected => "kept".into(),
            Status::Rewritten if self.line_change(doc, r, i) == Some(LineChange::Removed) => "line cut".into(),
            Status::Rewritten => "rewritten".into(),
            Status::Removed => "line removed".into(),
        }
    }

    /// [`LineChange::describe`] of item `i`'s line change, with a decided
    /// line that is gone named "line cut".
    pub fn line_label(&self, doc: &Document, r: usize, i: usize) -> Option<&'static str> {
        let change = self.line_change(doc, r, i)?;
        Some(if change == LineChange::Removed && self.status(doc, r, i) != Status::Removed {
            "line cut"
        } else {
            change.describe()
        })
    }

    /// The other review whose decision removed lines of pending block `i` of
    /// review `r`, if some (or all) are gone and one did (it names one of the
    /// gone lines as made on). The same before and after reopening: a block
    /// partly removed stays an item, one wholly removed becomes
    /// [`Unplaced::RemovedBy`].
    pub fn removed_by(&self, doc: &Document, r: usize, i: usize) -> Option<&str> {
        let item = self.item(r, i);
        let ids: Vec<ParaId> = item.before_ids().into_iter().filter(|id| doc.paragraph(*id).is_none()).collect();
        if !item.block || ids.is_empty() || self.block_value(doc, r, i).is_some() {
            return None;
        }
        self.reviews.iter().enumerate().filter(|&(r2, _)| r2 != r).find_map(|(r2, review)| {
            (0..review.items.len())
                .any(|i2| {
                    let decided = !matches!(self.status(doc, r2, i2), Status::Pending | Status::Removed);
                    decided && review.items[i2].before_ids().iter().any(|p| ids.contains(p))
                })
                .then_some(review.name.as_str())
        })
    }

    /// After opening every review on `doc`: mark each unmatched block whose
    /// `PARA:` lines are all gone because another review's decision (one
    /// made on any of them) cut them [`Unplaced::RemovedBy`], so the name
    /// survives reopening (such a block can't be placed then, so
    /// [`Self::removed_by`] has no item to ask about). Hosts that open
    /// reviews themselves call it once they are all added;
    /// [`crate::sessionfile::resume`] does.
    pub fn note_removed_by(&mut self, doc: &Document) {
        let mut marks = vec![];
        for (r, review) in self.reviews.iter().enumerate() {
            for (u, unmatched) in review.unmatched.iter().enumerate() {
                let Some(e) = review.file.entries.get(unmatched.entry) else { continue };
                if e.is_line() || e.para.is_empty() || e.para.iter().any(|id| doc.paragraph(*id).is_some()) {
                    continue;
                }
                let by = self.reviews.iter().enumerate().filter(|&(r2, _)| r2 != r).find_map(|(r2, other)| {
                    (0..other.items.len())
                        .any(|i2| {
                            let decided = !matches!(self.status(doc, r2, i2), Status::Pending | Status::Removed);
                            decided && other.items[i2].before_ids().iter().any(|p| e.para.contains(p))
                        })
                        .then_some(r2)
                });
                if let Some(review) = by {
                    marks.push((r, u, review));
                }
            }
        }
        for (r, u, review) in marks {
            self.reviews[r].unmatched[u].why = Some(Unplaced::RemovedBy { review });
        }
    }

    /// Why an entry wasn't placed, as hosts show it (e.g. "lines cut by
    /// notes.review.txt" for [`Unplaced::RemovedBy`]).
    pub fn unplaced_label(&self, why: &Unplaced) -> String {
        match why {
            Unplaced::Taken { entry, .. } => format!("lines already suggested on by e{entry}"),
            Unplaced::NoPara => "no paragraph named".into(),
            Unplaced::Missing => "line not found".into(),
            Unplaced::NotEditable => "line can't take suggestions".into(),
            Unplaced::Mismatch => "line changed".into(),
            Unplaced::Invisible => "changes nothing visible".into(),
            Unplaced::Ambiguous => "fits several places".into(),
            Unplaced::Restructured => "lines split, removed or moved apart".into(),
            Unplaced::Invalid(why) => format!("can't be written: {why}"),
            Unplaced::NoRow(why) => why.clone(),
            Unplaced::RemovedBy { review } => match self.reviews.get(*review) {
                Some(r) => format!("lines cut by {}", r.name),
                None => "lines cut by another review".into(),
            },
        }
    }

    /// Whether `decision` can be made on item `i` of review `r` now, and if
    /// not, why: nothing on a gone line; no accepting on a split or joined
    /// line, nor keeping the original over a decision there (either would
    /// write a whole-line text into part of the line). [`Self::decide`]
    /// refuses the same. Hosts phrase it with [`Refusal::message`].
    ///
    /// A block: nothing once every line is gone while pending
    /// ([`Refusal::RemovedBy`] if another review's decision removed them);
    /// no accepting while pending over lines split, partly removed or moved
    /// apart; no accepting over a line another review has decided (C9), nor
    /// keeping or undeciding an accept or rewrite there (which would put the
    /// lines it was made on back over that decision).
    pub fn check(&self, doc: &Document, r: usize, i: usize, decision: &Decision) -> Result<(), Refusal> {
        let item = self.item(r, i);
        let para = item.para;
        if let Decision::Accept(k) = decision
            && *k >= item.options.len()
        {
            return Err(Refusal::NoOption { k: *k, count: item.options.len() });
        }
        if item.block {
            return self.check_block(doc, r, i, decision);
        }
        let how = match self.line_change(doc, r, i) {
            Some(LineChange::Removed) => return Err(Refusal::LineRemoved { para }),
            Some(how @ (LineChange::Split | LineChange::Joined)) => how,
            _ => return Ok(()),
        };
        let decided = self.status(doc, r, i) != Status::Pending;
        match decision {
            Decision::Accept(_) => Err(Refusal::LineChanged { para, how }),
            Decision::KeepMine if decided => Err(Refusal::LineChanged { para, how }),
            _ => Ok(()),
        }
    }

    fn check_block(&self, doc: &Document, r: usize, i: usize, decision: &Decision) -> Result<(), Refusal> {
        let item = self.item(r, i);
        let ids = item.before_ids();
        let stored = self.block_value(doc, r, i).map(|v| v.decision);
        if stored.is_none() {
            let live: Vec<ParaId> = ids.iter().copied().filter(|id| doc.paragraph(*id).is_some()).collect();
            if live.is_empty() {
                return Err(match self.removed_by(doc, r, i) {
                    Some(review) => Refusal::RemovedBy { para: ids[0], review: review.to_string() },
                    None => Refusal::LineRemoved { para: ids[0] },
                });
            }
            if matches!(decision, Decision::Accept(_)) && live_span(doc, &ids) != ids {
                return Err(Refusal::Restructured { para: ids[0] });
            }
        }
        // C9: nothing that writes over lines another review has decided.
        // Keeping or undeciding a decision other than keep puts back the
        // lines it was made on, so it counts too.
        let writes = match decision {
            Decision::Accept(_) => true,
            Decision::KeepMine | Decision::Undecided => stored.is_some_and(|d| d != Stored::Rejected),
            Decision::Rewrite(_) | Decision::RewriteLines(_) => false,
        };
        if writes {
            let mine: HashSet<ParaId> = self.live_run(doc, r, i).into_iter().collect();
            for (r2, review) in self.reviews.iter().enumerate().filter(|&(r2, _)| r2 != r) {
                for i2 in 0..review.items.len() {
                    if matches!(self.status(doc, r2, i2), Status::Pending | Status::Removed) {
                        continue;
                    }
                    if let Some(para) = self.live_run(doc, r2, i2).into_iter().find(|p| mine.contains(p)) {
                        return Err(Refusal::DecidedElsewhere { para, review: review.name.clone() });
                    }
                }
            }
        }
        Ok(())
    }

    /// [`Self::check`] with the neutral wording.
    pub fn can_apply(&self, doc: &Document, r: usize, i: usize, decision: &Decision) -> Result<(), String> {
        self.check(doc, r, i, decision).map_err(|e| e.to_string())
    }

    /// Record a decision: change the paragraph's text and the decision as one
    /// undo step. Returns the selection to show (the end of the change).
    ///
    /// A block, as one transaction: accepting replaces its current run
    /// (its lines while pending, what its decision left once decided) with
    /// the option (`replace_run`), rewriting with the given lines; keeping
    /// and undeciding change only the decision while pending or kept (the
    /// text is then the writer's), and otherwise put back the lines it was
    /// made on ([`Item::before_paras`], `restore_run`). Values always name every line
    /// (`accepted:k@<before>=><after>|after:<id>|start`).
    pub fn decide(
        &self,
        doc: &mut Document,
        sel: Selection,
        r: usize,
        i: usize,
        decision: Decision,
    ) -> Result<Selection, DocError> {
        self.decide_as(doc, sel, r, i, decision, false)
    }

    /// [`Self::decide`], writing a value without the paragraph if `legacy`
    /// (line suggestions only).
    fn decide_as(
        &self,
        doc: &mut Document,
        sel: Selection,
        r: usize,
        i: usize,
        decision: Decision,
        legacy: bool,
    ) -> Result<Selection, DocError> {
        let item = self.item(r, i);
        match self.check(doc, r, i, &decision) {
            Ok(()) => {}
            Err(Refusal::LineRemoved { para } | Refusal::RemovedBy { para, .. }) => {
                return Err(DocError::MissingParagraph(para));
            }
            Err(Refusal::LineChanged { .. }) => {
                return Err(DocError::Mismatch("the line was split or joined since the suggestion was written"));
            }
            Err(Refusal::Restructured { .. }) => {
                return Err(DocError::Mismatch("the lines were restructured since the suggestion was written"));
            }
            Err(Refusal::DecidedElsewhere { .. }) => {
                return Err(DocError::Mismatch("a line is decided in another review"));
            }
            Err(Refusal::NoOption { .. }) => return Err(DocError::Mismatch("no such option")),
        }
        if item.block {
            return self.decide_block(doc, sel, r, i, decision);
        }
        let decision = match decision {
            Decision::RewriteLines(lines) => match &lines[..] {
                [line] => Decision::Rewrite(line.text.clone()),
                _ => return Err(DocError::Mismatch("a line suggestion is rewritten with one line")),
            },
            d => d,
        };
        let key = self.key(r, i);
        let current = Self::text(doc, item.para);
        // On a split or joined line, undoing a decision leaves the text as
        // it is: the original would duplicate (or drop) the rest of the line.
        let structural = matches!(self.line_change(doc, r, i), Some(LineChange::Split | LineChange::Joined));
        let untouched = structural || self.status(doc, r, i) == Status::Pending;
        let para = (!legacy).then_some(item.para);
        let value = |decision| Some(MetaValue::line(decision, para).to_value());
        let (text, value) = match decision {
            Decision::Accept(k) => (item.options[k].clone(), value(Stored::Accepted(k))),
            Decision::KeepMine => (if untouched { current } else { item.before.clone() }, value(Stored::Rejected)),
            Decision::Rewrite(t) => (t, value(Stored::Rewritten)),
            Decision::Undecided => (if untouched { current } else { item.before.clone() }, None),
            Decision::RewriteLines(_) => unreachable!("mapped above"),
        };
        doc.replace_text(sel, item.para, &text, &[(&key, value.as_deref())])
    }

    fn decide_block(
        &self,
        doc: &mut Document,
        sel: Selection,
        r: usize,
        i: usize,
        decision: Decision,
    ) -> Result<Selection, DocError> {
        let item = self.item(r, i);
        let key = self.key(r, i);
        let stored = self.block_value(doc, r, i).map(|v| v.decision);
        // Kept: the text is the writer's, so going back to pending (or keeping
        // again) changes only the decision, never hand edits made since.
        let restores = stored.is_some_and(|d| d != Stored::Rejected);
        let run = self.run(doc, r, i);
        let before_ids = item.before_ids();
        let value = |decision: Stored, now: Option<Now>| {
            MetaValue { decision, paras: Some(before_ids.clone()), now }.to_value()
        };
        let restore: Vec<(ParaId, NewPara)> = item
            .before_paras
            .iter()
            .map(|p| (p.id, NewPara { kind: p.kind.clone(), spans: p.spans.clone() }))
            .collect();
        // The end of the last line written (or, for a cut, of the line before).
        let end = |e: &scriptorium_core::Edit, ids: &[ParaId], after: Option<ParaId>| {
            let doc = e.doc();
            match ids.last().copied().or(after).and_then(|id| doc.paragraph(id)) {
                Some(p) => Selection::cursor(Pos::new(p.id, p.text().len())),
                None => Selection::cursor(doc.start()),
            }
        };
        doc.edit(sel, Group::Other, |e| {
            let at = match decision {
                Decision::Accept(_) | Decision::Rewrite(_) | Decision::RewriteLines(_) => {
                    let (lines, stored) = match decision {
                        Decision::Accept(k) => (item.option_lines[k].clone(), Stored::Accepted(k)),
                        Decision::Rewrite(t) => (vec![Line::untyped(t)], Stored::Rewritten),
                        Decision::RewriteLines(lines) => (lines, Stored::Rewritten),
                        _ => unreachable!(),
                    };
                    let done = e.replace_run(run, &new_paras(&lines, &item.before_paras))?;
                    e.set_meta(&key, Some(&value(stored, Some(now_of(&done)))))?;
                    end(e, &done.ids, done.after)
                }
                Decision::KeepMine | Decision::Undecided => {
                    if restores {
                        e.restore_run(run, &restore)?;
                    }
                    let v = (decision == Decision::KeepMine).then(|| value(Stored::Rejected, None));
                    e.set_meta(&key, v.as_deref())?;
                    end(e, &before_ids, None)
                }
            };
            Ok((at, at))
        })
    }

    /// After the writer has edited the paragraph themselves: record what the
    /// text now amounts to (one of the options, their original, or a rewrite).
    /// A block: what its current lines amount to, the text left as it is.
    pub fn decide_from_text(
        &self,
        doc: &mut Document,
        sel: Selection,
        r: usize,
        i: usize,
    ) -> Result<Selection, DocError> {
        let item = self.item(r, i);
        if item.block {
            let ids = self.live_run(doc, r, i);
            let lines = self.mine(doc, r, i);
            let before_ids = item.before_ids();
            let same = |a: &[Line], b: &[Line]| a == b;
            let decision = match item.option_lines.iter().position(|o| same(o, &lines)) {
                Some(k) => Stored::Accepted(k),
                None if ids == before_ids && same(&item.before_lines, &lines) => Stored::Rejected,
                None => Stored::Rewritten,
            };
            let now = if ids.is_empty() {
                match self.run(doc, r, i) {
                    Run::Empty { after: Some(a) } => Now::After(a),
                    _ => Now::Start,
                }
            } else {
                Now::Ids(ids)
            };
            let now = (decision != Stored::Rejected).then_some(now);
            let value = MetaValue { decision, paras: Some(before_ids), now }.to_value();
            let key = self.key(r, i);
            return doc.edit(sel, Group::Other, |e| {
                e.set_meta(&key, Some(&value))?;
                Ok((sel, sel))
            });
        }
        let text = Self::text(doc, item.para);
        let decision = match item.options.iter().position(|o| *o == text) {
            Some(k) => Stored::Accepted(k),
            None if text == item.before => Stored::Rejected,
            None => Stored::Rewritten,
        };
        let value = MetaValue::line(decision, Some(item.para)).to_value();
        let key = self.key(r, i);
        doc.replace_text(sel, item.para, &text, &[(&key, Some(&value))])
    }

    pub fn progress(&self, doc: &Document, r: usize) -> Progress {
        let mut p = Progress { total: self.reviews[r].items.len(), ..Progress::default() };
        for i in 0..p.total {
            match self.status(doc, r, i) {
                Status::Pending => p.pending += 1,
                Status::Accepted(_) => p.accepted += 1,
                Status::Rejected => p.rejected += 1,
                Status::Rewritten => p.rewritten += 1,
                Status::Removed => p.removed += 1,
            }
        }
        p
    }

    /// The first pending item after `after` (or from the start), wrapping round.
    pub fn next_pending(&self, doc: &Document, r: usize, after: Option<usize>) -> Option<usize> {
        let n = self.reviews[r].items.len();
        let start = after.map_or(0, |a| a + 1);
        (start..n).chain(0..start.min(n)).find(|&i| self.status(doc, r, i) == Status::Pending)
    }

    /// The review file with every decision written into it (`STATUS`,
    /// `CHOSEN`, `MINE`), so the review can be continued elsewhere or handed on.
    /// Entries stay in file order (their positions are the decision keys) and
    /// the review's id is always written as an `Id:` header, so writing
    /// progress over the file itself keeps the session's decisions attached.
    /// The file is reserialized: comments and wrapping are not kept. A
    /// rewritten block's `MINE` is its live lines, typed ([`Self::mine`]).
    /// Like a one-line `MINE`, each line is read back trimmed: spaces at the
    /// ends of a line carry nothing in screenplay text (layout comes from the
    /// element type), and the review grammar can't tell them from indentation.
    pub fn save_progress(&self, doc: &Document, r: usize) -> String {
        let review = &self.reviews[r];
        let item_of: HashMap<usize, usize> = review.items.iter().enumerate().map(|(i, it)| (it.entry, i)).collect();
        let entries: Vec<Entry> = review
            .file
            .entries
            .iter()
            .enumerate()
            .map(|(e, entry)| {
                let mut entry = Entry { status: FileStatus::Pending, chosen: None, mine: None, ..entry.clone() };
                if let Some(&i) = item_of.get(&e) {
                    let item = &review.items[i];
                    match self.status(doc, r, i) {
                        // Can't be decided: kept as a pending suggestion.
                        Status::Pending | Status::Removed => {}
                        Status::Accepted(k) => {
                            entry.status = FileStatus::Accepted;
                            entry.chosen = Some(item.option_src[k]);
                        }
                        Status::Rejected => entry.status = FileStatus::Rejected,
                        Status::Rewritten => {
                            entry.status = FileStatus::Rewritten;
                            // A gone line has no text of its own: `[cut]`
                            // (which reopening never writes in), never empty.
                            entry.mine = Some(self.mine(doc, r, i));
                        }
                    }
                } else if review.unmatched.iter().any(|u| u.entry == e) {
                    // Unmatched: kept as written.
                    return review.file.entries[e].clone();
                }
                entry
            })
            .collect();
        ReviewFile { script: review.file.script.clone(), id: Some(review.id.clone()), entries, warnings: vec![] }
            .serialize()
    }
}
