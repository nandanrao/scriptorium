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
//! [`MetaValue`]). A value whose paragraph isn't the item's is ignored
//! (pending), so deleting a block mid-file by hand orphans the decisions that
//! shift onto other entries instead of moving them there. Values without `@`
//! (written before this, and in the rebuilt starting point of a version 1
//! session, whose saved events expect them) are read as they are.

use std::collections::{HashMap, HashSet};

use scriptorium_core::doc::DocError;
use scriptorium_core::text::{normalize, similarity};
use scriptorium_core::{Document, ElementKind, ParaId, Script, Selection};

use crate::file::{Entry, FileStatus, ReviewFile, append_block, insert_id};
use crate::matching::{FUZZY_THRESHOLD, Match, Para, match_live, match_para};

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

/// What a decision value records, without the paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stored {
    /// `accepted:<k>`: option `k` of the item.
    Accepted(usize),
    /// `rejected`: kept as it was.
    Rejected,
    /// `rewritten`, or anything else that isn't one of the above.
    Rewritten,
}

/// A decision as stored in document metadata under [`Session::key`]: the one
/// place decision values are built and parsed. `<decision>@<para>`, or
/// `<decision>` alone (`para: None`) in values written before paragraphs were
/// recorded, which apply to whatever paragraph the item is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MetaValue {
    decision: Stored,
    para: Option<ParaId>,
}

impl MetaValue {
    /// `None` for a value whose paragraph part isn't a paragraph id.
    fn parse(value: &str) -> Option<Self> {
        let (d, para) = match value.split_once('@') {
            None => (value, None),
            Some((d, p)) => (d, Some(ParaId(p.parse::<u64>().ok()?))),
        };
        let decision = match d {
            "rejected" => Stored::Rejected,
            d => match d.strip_prefix("accepted:").and_then(|k| k.parse::<usize>().ok()) {
                Some(k) => Stored::Accepted(k),
                None => Stored::Rewritten,
            },
        };
        Some(Self { decision, para })
    }

    /// The decision in `value` if it was made on `para` (or doesn't say).
    fn on(value: &str, para: ParaId) -> Option<Stored> {
        Self::parse(value).filter(|v| v.para.is_none_or(|p| p == para)).map(|v| v.decision)
    }

    fn to_value(self) -> String {
        let d = match self.decision {
            Stored::Accepted(k) => format!("accepted:{k}"),
            Stored::Rejected => "rejected".to_string(),
            Stored::Rewritten => "rewritten".to_string(),
        };
        match self.para {
            Some(p) => format!("{d}@{}", p.0),
            None => d,
        }
    }
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
        Self::build(name.into(), text, &paras, &paras, &|_, _| false, script_name)
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
        Self::build(name.into(), text, &original_paras, &live_paras, &has_meta, script_name)
    }

    fn build(
        name: String,
        text: &str,
        original: &[Para],
        live: &[Para],
        has_meta: &dyn Fn(usize, ParaId) -> bool,
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
        let matches = match_live(original, live, &file.entries, &HashSet::new(), has_meta);

        let (mut items, mut unmatched, mut invisible, mut restore) = (vec![], vec![], vec![], vec![]);
        for (entry, (e, m)) in file.entries.iter().zip(matches).enumerate() {
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
        items.sort_by_key(|it| it.index);
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
}

impl Progress {
    pub fn done(&self) -> usize {
        self.total - self.pending
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
        Self { reviews }
    }

    pub fn item(&self, r: usize, i: usize) -> &Item {
        &self.reviews[r].items[i]
    }

    /// The document metadata key holding the decision on item `i` of review
    /// `r`. Its values are built and read only through [`MetaValue`].
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
            match_live(&live, &live, &read, &used, |_, _| false)
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
    /// [`Self::key`] (see [`MetaValue`]), checked against the paragraph's
    /// current text (an accepted line edited since shows as rewritten).
    pub fn status(&self, doc: &Document, r: usize, i: usize) -> Status {
        let item = self.item(r, i);
        match doc.meta(&self.key(r, i)).and_then(|v| MetaValue::on(v, item.para)) {
            None => Status::Pending,
            Some(Stored::Rejected) => Status::Rejected,
            Some(Stored::Accepted(k)) if item.options.get(k).is_some_and(|o| *o == Self::text(doc, item.para)) => {
                Status::Accepted(k)
            }
            Some(_) => Status::Rewritten,
        }
    }

    /// Pending, but the paragraph has changed since the suggestion was written
    /// (by hand, or by another review).
    pub fn is_outdated(&self, doc: &Document, r: usize, i: usize) -> bool {
        let item = self.item(r, i);
        self.status(doc, r, i) == Status::Pending && Self::text(doc, item.para) != item.before
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
        let key = self.key(r, i);
        let current = Self::text(doc, item.para);
        let untouched = self.status(doc, r, i) == Status::Pending;
        let para = (!legacy).then_some(item.para);
        let value = |decision| Some(MetaValue { decision, para }.to_value());
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
        let value = MetaValue { decision, para: Some(item.para) }.to_value();
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
                        Status::Pending => {}
                        Status::Accepted(k) => {
                            entry.status = FileStatus::Accepted;
                            entry.chosen = Some(item.option_src[k]);
                        }
                        Status::Rejected => entry.status = FileStatus::Rejected,
                        Status::Rewritten => {
                            entry.status = FileStatus::Rewritten;
                            entry.mine = Some(Self::text(doc, item.para));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_values_round_trip_and_read_legacy_values() {
        for (v, decision, para) in [
            ("accepted:2@7", Stored::Accepted(2), Some(ParaId(7))),
            ("rejected@0", Stored::Rejected, Some(ParaId(0))),
            ("rewritten@31", Stored::Rewritten, Some(ParaId(31))),
            ("accepted:1", Stored::Accepted(1), None),
            ("rejected", Stored::Rejected, None),
        ] {
            let m = MetaValue::parse(v).unwrap();
            assert_eq!(m, MetaValue { decision, para });
            assert_eq!(m.to_value(), v);
        }
        assert_eq!(MetaValue::parse("accepted:x").unwrap().decision, Stored::Rewritten);
        assert_eq!(MetaValue::parse("rejected@nope"), None);
        assert_eq!(MetaValue::on("rejected@3", ParaId(3)), Some(Stored::Rejected));
        assert_eq!(MetaValue::on("rejected@3", ParaId(4)), None);
        assert_eq!(MetaValue::on("rejected", ParaId(4)), Some(Stored::Rejected));
    }
}
