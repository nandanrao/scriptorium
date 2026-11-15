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

use scriptorium_core::doc::{DocError, Event, Op};
use scriptorium_core::text::{normalize, similarity};
use scriptorium_core::{Document, ElementKind, ParaId, Script, Selection};

use crate::file::{Entry, FileStatus, ReviewFile, append_block, insert_id};
use crate::matching::{FUZZY_THRESHOLD, Match, Para, match_live, match_para};
use crate::meta::{MetaValue, Stored};

/// What the writer decides about a suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Take option `k` (0-based, into [`Item::options`]).
    Accept(usize),
    /// Keep the paragraph as it is (or as it was, if this review changed it).
    KeepMine,
    /// The writer's own text.
    Rewrite(String),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// `Accept(k)` with no option `k` (0-based); there are `count`.
    NoOption { k: usize, count: usize },
    /// The line is gone ([`LineChange::Removed`]): nothing can be decided.
    LineRemoved { para: ParaId },
    /// The line was split or joined: accepting (or keeping the original over
    /// a decision) would write a whole-line text into part of the line.
    LineChanged { para: ParaId, how: LineChange },
}

impl Refusal {
    /// The message, ending with `rewrite`: how this host rewrites a line
    /// (the app: "press E to rewrite this line"; the CLI: "use decide <n>
    /// rewrite <text>"). Only [`Refusal::LineChanged`] uses it.
    pub fn message(&self, rewrite: &str) -> String {
        match *self {
            Self::NoOption { k, count } => format!("no option {} (1 to {count})", k + 1),
            Self::LineRemoved { para } => format!(
                "line removed: ¶{} was removed (or joined into the line above) since the suggestion was written, so \
                 it can't be decided",
                para.0
            ),
            Self::LineChanged { para, how } => {
                let how = if how == LineChange::Split { "split" } else { "joined with the next line" };
                format!(
                    "line changed: ¶{} was {how} since the suggestion was written, so its text no longer fits the \
                     line; {rewrite}",
                    para.0
                )
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
}

impl LineChanges {
    fn of(&self, para: ParaId) -> Option<LineChange> {
        self.changed.get(&para).copied().or_else(|| (!self.live.contains(&para)).then_some(LineChange::Removed))
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
    doc.for_each_applied_op(|op| match op {
        Op::Splice { para, removed, inserted, .. } => {
            let l = len.entry(*para).or_default();
            *l = (*l + spans::len(inserted)).saturating_sub(spans::len(removed));
        }
        Op::Split { para, at, new_para, .. } => {
            let l = len.get(para).copied().unwrap_or_default();
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
        Op::RemovePara { .. } | Op::SetMeta { .. } => {}
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
    LineChanges { changed, live: live.into_keys().collect() }
}

/// A suggestion matched to a paragraph.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub para: ParaId,
    /// The paragraph's position in the live script it was matched against.
    pub index: usize,
    /// Which entry of the review file it came from.
    pub entry: usize,
    /// The text the entry's `BEFORE` was matched against (what the suggestion
    /// is for, and what undoing a decision goes back to): the paragraph's
    /// original text, else its live text; never the text after a decision.
    pub before: String,
    /// Replacement texts that would visibly change the paragraph, in order.
    pub options: Vec<String>,
    /// `options[i]` is the file's `AFTER` number `option_src[i]` (0-based).
    pub option_src: Vec<usize>,
    pub why: String,
    /// Matched approximately: the `BEFORE` text wasn't found exactly.
    pub fuzzy: bool,
}

/// A review-file entry whose paragraph wasn't found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmatched {
    pub entry: usize,
    pub before: String,
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
    Item(Item),
    Invisible,
    Unmatched,
}

/// The one way an entry and its match become an [`Item`] (shared by
/// [`Review::open`] and [`Session::append_entry`]).
fn place(entry: usize, e: &Entry, m: Option<Match>) -> Placed {
    match m {
        None => Placed::Unmatched,
        Some(m) if m.options.is_empty() => Placed::Invisible,
        Some(m) => Placed::Item(Item {
            para: m.para,
            index: m.index,
            entry,
            before: m.before,
            options: m.options,
            option_src: m.option_src,
            why: e.why.clone(),
            fuzzy: m.fuzzy,
        }),
    }
}

fn paras<'a>(
    paragraphs: &'a [scriptorium_core::Paragraph],
    texts: &'a [String],
    is_caps: &dyn Fn(&ElementKind) -> bool,
) -> Vec<Para<'a>> {
    paragraphs
        .iter()
        .zip(texts)
        .map(|(p, t)| Para { id: p.id, text: t, editable: p.kind.is_editable(), caps: is_caps(&p.kind) })
        .collect()
}

/// Why `e` (matched by [`Session::append_entries`]) found no paragraph.
/// `claimed`: paragraphs taken by the review's items or earlier entries of the
/// batch, with the entry that took each and the item it became.
fn unplaced(e: &Entry, para_only: bool, live: &[Para], claimed: &HashMap<ParaId, (usize, Option<usize>)>) -> Unplaced {
    let taken = |&(entry, item): &(usize, Option<usize>)| Unplaced::Taken { entry, item };
    let named = e.para.map(|id| live.iter().find(|p| p.id == id));
    if let Some(Some(p)) = named
        && let Some(c) = claimed.get(&p.id)
    {
        return taken(c);
    }
    if !para_only {
        // No free line fits BEFORE: say so if a taken one does, exactly or
        // (as matching would have) approximately.
        let key = normalize(&e.before);
        let claimed_lines = || live.iter().filter(|p| p.editable).filter_map(|p| claimed.get(&p.id).map(|c| (p, c)));
        if let Some((_, c)) = claimed_lines().find(|(p, _)| normalize(p.text) == key) {
            return taken(c);
        }
        let fuzzy = claimed_lines()
            .filter(|(p, _)| !key.is_empty() && !p.text.trim().is_empty())
            .map(|(p, c)| (similarity(&e.before, p.text), c))
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

/// Every paragraph `doc` has had since its starting point, in script order:
/// its starting paragraphs, with each line added since placed where it was
/// added (and kept there after it's removed). Replays the log one event at a
/// time.
fn history_order(doc: &Document) -> Vec<ParaId> {
    let ids = |d: &Document| d.paragraphs().iter().map(|p| p.id).collect::<Vec<_>>();
    let mut order = ids(&Document::with_meta(doc.initial().clone(), doc.initial_meta().clone()));
    let mut sim = Document::with_meta(doc.initial().clone(), doc.initial_meta().clone());
    // Puts `new` just before `next` in `order` (or after `prev`, or last).
    let place = |order: &mut Vec<ParaId>, new: ParaId, next: Option<ParaId>, prev: Option<ParaId>| {
        if order.contains(&new) {
            return;
        }
        let at = next
            .and_then(|n| order.iter().position(|&p| p == n))
            .or_else(|| prev.and_then(|p| order.iter().position(|&q| q == p).map(|i| i + 1)))
            .unwrap_or(order.len());
        order.insert(at, new);
    };
    for ev in doc.log() {
        if let Event::Edit(t) = ev {
            let mut live = ids(&sim);
            for op in &t.ops {
                match op {
                    Op::InsertPara { index, para } => {
                        let (next, prev) =
                            (live.get(*index).copied(), index.checked_sub(1).and_then(|i| live.get(i).copied()));
                        place(&mut order, para.id, next, prev);
                        live.insert((*index).min(live.len()), para.id);
                    }
                    Op::RemovePara { para, .. } => live.retain(|&p| p != para.id),
                    Op::Split { para, new_para, .. } => {
                        let i = live.iter().position(|p| p == para).map_or(live.len(), |i| i + 1);
                        place(&mut order, *new_para, live.get(i).copied(), Some(*para));
                        live.insert(i, *new_para);
                    }
                    Op::Merge { next, .. } => live.retain(|p| p != next),
                    _ => {}
                }
            }
        }
        match sim.replay_onto(std::slice::from_ref(ev)) {
            Ok(d) => sim = d,
            Err(_) => break,
        }
    }
    order
}

/// Where paragraph `id`, which `doc` has had (in `order`, see
/// [`history_order`]) but no longer has, would be now: the live position of
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
        let paras = paras(&script.paragraphs, &texts, &is_caps);
        Self::build(name.into(), text, &paras, &paras, &|_, _| false, &|_| None, script_name)
    }

    /// [`Self::open`] for a script that may have been edited since it was
    /// read: `original` is the script as read from its file, `live` the
    /// document now. Entries are matched by `PARA:` against the live document
    /// and by `BEFORE` against the original (see [`crate::matching::match_live`]);
    /// [`Item::index`] is the live position.
    pub fn open_live(
        name: impl Into<String>,
        text: &str,
        original: &Script,
        live: &Document,
        is_caps: impl Fn(&ElementKind) -> bool,
        script_name: &str,
    ) -> Self {
        let original_texts: Vec<String> = original.paragraphs.iter().map(|p| p.text()).collect();
        let live_texts: Vec<String> = live.paragraphs().iter().map(|p| p.text()).collect();
        let original_paras = paras(&original.paragraphs, &original_texts, &is_caps);
        let live_paras = paras(live.paragraphs(), &live_texts, &is_caps);
        let id = Self::id_of(text);
        let has_meta = |entry: usize, para: ParaId| {
            live.meta(&format!("review/{id}/{entry}")).and_then(|v| MetaValue::on(v, para)).is_some()
        };
        // Worked out only if an entry is on a gone line.
        let history = std::cell::OnceCell::new();
        let gone_at = |p: ParaId| gone_position(history.get_or_init(|| history_order(live)), live, p);
        Self::build(name.into(), text, &original_paras, &live_paras, &has_meta, &gone_at, script_name)
    }

    fn build(
        name: String,
        text: &str,
        original: &[Para],
        live: &[Para],
        has_meta: &dyn Fn(usize, ParaId) -> bool,
        gone_at: &dyn Fn(ParaId) -> Option<usize>,
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
        let matches = match_live(original, live, &file.entries, &HashSet::new(), has_meta, gone_at);

        let (mut items, mut unmatched, mut invisible, mut restore) = (vec![], vec![], vec![], vec![]);
        // Gone lines sort before the surviving line at their position.
        let mut gone: HashSet<usize> = HashSet::new();
        for (entry, (e, m)) in file.entries.iter().zip(matches).enumerate() {
            if m.as_ref().is_some_and(|m| m.gone) {
                gone.insert(entry);
            }
            let item = match place(entry, e, m) {
                Placed::Unmatched => {
                    unmatched.push(Unmatched { entry, before: e.before.clone() });
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
            let decision = match (e.status, &e.mine) {
                // A cut line (Phase B's marker): never written in as text.
                (FileStatus::Rewritten, Some(mine)) if mine.trim() == "[cut]" => None,
                (FileStatus::Rewritten, Some(mine)) => Some(Decision::Rewrite(mine.clone())),
                (FileStatus::Rejected, _) => Some(Decision::KeepMine),
                (FileStatus::Accepted, _) => {
                    item.option_src.iter().position(|&s| s == e.chosen.unwrap_or(0)).map(Decision::Accept)
                }
                _ => None,
            };
            if let Some(d) = decision {
                restore.push((entry, d));
            }
            items.push(item);
        }
        items.sort_by_key(|it| (it.index, !gone.contains(&it.entry)));
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        let many = self.append_entries(doc, r, vec![entry], false, is_caps)?;
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
    /// Fails, writing nothing, if an entry has no `BEFORE` or no `AFTER`.
    pub fn append_entries(
        &mut self,
        doc: &Document,
        r: usize,
        entries: Vec<Entry>,
        para_only: bool,
        is_caps: impl Fn(&ElementKind) -> bool,
    ) -> Result<AppendedMany, String> {
        let review = self.reviews.get_mut(r).ok_or_else(|| format!("no review {r}"))?;
        let mut read = Vec::with_capacity(entries.len());
        for entry in &entries {
            if entry.before.trim().is_empty() || entry.options.iter().all(|o| o.trim().is_empty()) {
                return Err("a suggestion needs a BEFORE and at least one AFTER".into());
            }
            // What the file will say, as a reader sees the block (newlines flattened…).
            let Some(e) = ReviewFile::parse(&append_block("", entry, "x")).entries.pop() else {
                return Err("the suggestion couldn't be written as a review block".into());
            };
            read.push(e);
        }
        let used: HashSet<ParaId> = review.items.iter().map(|it| it.para).collect();
        let texts: Vec<String> = doc.paragraphs().iter().map(|p| p.text()).collect();
        let live = paras(doc.paragraphs(), &texts, &is_caps);
        // New entries have no decisions yet.
        let matches = if para_only {
            match_para(&live, &live, &read, &mut used.clone(), |_, _| false)
        } else {
            match_live(&live, &live, &read, &used, |_, _| false, |_| None)
        };

        let mut file_text = review.text.clone();
        let mut results = Vec::with_capacity(read.len());
        // Paragraphs claimed so far: by existing items, then by this batch.
        let mut claimed: HashMap<ParaId, (usize, Option<usize>)> =
            review.items.iter().enumerate().map(|(i, it)| (it.para, (it.entry, Some(i)))).collect();
        for (mut e, m) in read.into_iter().zip(matches) {
            let index = review.file.entries.len();
            if let Some(m) = &m {
                // Written as matched, so reopening (PARA pass) finds the same line.
                e.para = Some(m.para);
                if normalize(&e.before) != normalize(&m.before) {
                    e.before = m.before.trim().to_string();
                }
            }
            let para = m.as_ref().map(|m| m.para);
            let (item, unplaced) = match place(index, &e, m) {
                Placed::Unmatched => {
                    let why = unplaced(&e, para_only, &live, &claimed);
                    results.push(AppendedEntry { item: None, entry: None, unplaced: Some(why) });
                    continue;
                }
                Placed::Invisible => {
                    review.invisible.push(index);
                    (None, Some(Unplaced::Invisible))
                }
                Placed::Item(item) => {
                    review.items.push(item);
                    (Some(review.items.len() - 1), None)
                }
            };
            if let Some(p) = para {
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

    /// Where item `i` of review `r` stands: its decision value under
    /// [`Self::key`] (see [`crate::meta`]), checked against the paragraph's
    /// current text (an accepted line edited since shows as rewritten). A
    /// gone line is [`Status::Removed`] if pending, else
    /// [`Status::Rewritten`].
    pub fn status(&self, doc: &Document, r: usize, i: usize) -> Status {
        let item = self.item(r, i);
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
    /// (by hand, or by another review), or was split, joined or retyped.
    pub fn is_outdated(&self, doc: &Document, r: usize, i: usize) -> bool {
        let item = self.item(r, i);
        self.status(doc, r, i) == Status::Pending
            && (Self::text(doc, item.para) != item.before || self.line_change(doc, r, i).is_some())
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
    pub fn line_change(&self, doc: &Document, r: usize, i: usize) -> Option<LineChange> {
        self.line_changes(doc).of(self.item(r, i).para)
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

    /// Whether `decision` can be made on item `i` of review `r` now, and if
    /// not, why: nothing on a gone line; no accepting on a split or joined
    /// line, nor keeping the original over a decision there (either would
    /// write a whole-line text into part of the line). [`Self::decide`]
    /// refuses the same. Hosts phrase it with [`Refusal::message`].
    pub fn check(&self, doc: &Document, r: usize, i: usize, decision: &Decision) -> Result<(), Refusal> {
        let item = self.item(r, i);
        let para = item.para;
        if let Decision::Accept(k) = decision
            && *k >= item.options.len()
        {
            return Err(Refusal::NoOption { k: *k, count: item.options.len() });
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

    /// [`Self::check`] with the neutral wording.
    pub fn can_apply(&self, doc: &Document, r: usize, i: usize, decision: &Decision) -> Result<(), String> {
        self.check(doc, r, i, decision).map_err(|e| e.to_string())
    }

    /// Record a decision: change the paragraph's text and the decision as one
    /// undo step. Returns the selection to show (the end of the change).
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

    /// [`Self::decide`], writing a value without the paragraph if `legacy`.
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
            Err(Refusal::LineRemoved { para }) => return Err(DocError::MissingParagraph(para)),
            Err(Refusal::LineChanged { .. }) => {
                return Err(DocError::Mismatch("the line was split or joined since the suggestion was written"));
            }
            Err(Refusal::NoOption { .. }) => return Err(DocError::Mismatch("no such option")),
        }
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
        };
        doc.replace_text(sel, item.para, &text, &[(&key, value.as_deref())])
    }

    /// After the writer has edited the paragraph themselves: record what the
    /// text now amounts to (one of the options, their original, or a rewrite).
    pub fn decide_from_text(
        &self,
        doc: &mut Document,
        sel: Selection,
        r: usize,
        i: usize,
    ) -> Result<Selection, DocError> {
        let item = self.item(r, i);
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
    /// The file is reserialized: comments and wrapping are not kept.
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
                            entry.mine = Some(doc.paragraph(item.para).map_or_else(|| "[cut]".into(), |p| p.text()));
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
