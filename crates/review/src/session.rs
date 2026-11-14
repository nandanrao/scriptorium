//! Reviews opened on a script, and decisions made through a [`Document`].
//!
//! A decision is stored as document metadata under a key per suggestion
//! (`review/<review id>/<entry>`), set in the same transaction as the text
//! change. The review id is a hash of the review file's contents, so a saved
//! session still applies when review files are added, removed or reordered.
//! The status shown to the user is derived from that value and the paragraph's
//! current text, so a later hand edit to an accepted line shows as a rewrite.

use std::collections::HashMap;

use scriptorium_core::doc::DocError;
use scriptorium_core::text::normalize;
use scriptorium_core::{Document, ElementKind, ParaId, Script, Selection};

use crate::file::{Entry, FileStatus, ReviewFile};
use crate::matching::{Para, match_entries};

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
    /// The paragraph's position in the script it was matched against.
    pub index: usize,
    /// Which entry of the review file it came from.
    pub entry: usize,
    /// The paragraph's text when the review was opened (what the suggestion is for).
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
    /// Identifies the review file by its contents (see [`crate::sessionfile::hash`]).
    pub id: String,
    pub name: String,
    pub file: ReviewFile,
    /// In script order.
    pub items: Vec<Item>,
    pub unmatched: Vec<Unmatched>,
    /// Entries whose every option only changes what the script doesn't show
    /// (e.g. letter case in a character name). Not reviewed.
    pub invisible: Vec<usize>,
    pub warnings: Vec<String>,
    /// Matched paragraph position per entry (for writing progress back in order).
    entry_index: Vec<Option<usize>>,
    /// Decisions saved in the file, per item.
    restore: Vec<(usize, Decision)>,
}

impl Review {
    /// Read a review file and match it against `script`. `script_name` is the
    /// file name the script was opened from, to check the file's `Script:` header.
    pub fn open(
        name: impl Into<String>,
        text: &str,
        script: &Script,
        is_caps: impl Fn(&ElementKind) -> bool,
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
        let texts: Vec<String> = script.paragraphs.iter().map(|p| p.text()).collect();
        let paras: Vec<Para> = script
            .paragraphs
            .iter()
            .zip(&texts)
            .map(|(p, t)| Para { id: p.id, text: t, editable: p.kind.is_editable(), caps: is_caps(&p.kind) })
            .collect();
        let matches = match_entries(&paras, &file.entries);

        let (mut items, mut unmatched, mut invisible, mut restore) = (vec![], vec![], vec![], vec![]);
        let mut entry_index = vec![];
        for (entry, (e, m)) in file.entries.iter().zip(matches).enumerate() {
            let Some(m) = m else {
                unmatched.push(Unmatched { entry, before: e.before.clone() });
                entry_index.push(None);
                continue;
            };
            entry_index.push(Some(m.index));
            if m.options.is_empty() {
                invisible.push(entry);
                continue;
            }
            let decision = match (e.status, &e.mine) {
                (FileStatus::Rewritten, Some(mine)) => Some(Decision::Rewrite(mine.clone())),
                (FileStatus::Rejected, _) => Some(Decision::KeepMine),
                (FileStatus::Accepted, _) => {
                    Some(Decision::Accept(m.option_src.iter().position(|&s| s == e.chosen.unwrap_or(0)).unwrap_or(0)))
                }
                _ => None,
            };
            items.push((
                Item {
                    para: m.para,
                    index: m.index,
                    entry,
                    before: texts[m.index].clone(),
                    options: m.options,
                    option_src: m.option_src,
                    why: e.why.clone(),
                    fuzzy: m.fuzzy,
                },
                decision,
            ));
        }
        items.sort_by_key(|(it, _)| it.index);
        let items: Vec<Item> = items
            .into_iter()
            .enumerate()
            .map(|(i, (it, d))| {
                if let Some(d) = d {
                    restore.push((i, d));
                }
                it
            })
            .collect();
        let id = crate::sessionfile::hash(text)[..12].to_string();
        Self { id, name: name.into(), file, items, unmatched, invisible, warnings, entry_index, restore }
    }

    /// How many of the file's suggestions had decisions saved in it.
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

    /// The document metadata key holding the decision on item `i` of review `r`.
    pub fn key(&self, r: usize, i: usize) -> String {
        format!("review/{}/{}", self.reviews[r].id, self.item(r, i).entry)
    }

    /// Start a document for `script` with the decisions saved in the review
    /// files already applied. They are the starting point, so undo stops there.
    pub fn open_document(&self, script: Script) -> Document {
        let mut doc = Document::new(script);
        let sel = Selection::cursor(doc.start());
        for (r, review) in self.reviews.iter().enumerate() {
            for (i, d) in &review.restore {
                // A restore that doesn't apply (paragraph missing) is skipped.
                let _ = self.decide(&mut doc, sel, r, *i, d.clone());
            }
        }
        doc.rebase()
    }

    fn text(doc: &Document, para: ParaId) -> String {
        doc.paragraph(para).map(|p| p.text()).unwrap_or_default()
    }

    pub fn status(&self, doc: &Document, r: usize, i: usize) -> Status {
        let item = self.item(r, i);
        match doc.meta(&self.key(r, i)) {
            None => Status::Pending,
            Some("rejected") => Status::Rejected,
            Some(v) => match v.strip_prefix("accepted:").and_then(|k| k.parse::<usize>().ok()) {
                Some(k) if item.options.get(k).is_some_and(|o| *o == Self::text(doc, item.para)) => Status::Accepted(k),
                _ => Status::Rewritten,
            },
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
        let item = self.item(r, i);
        let key = self.key(r, i);
        let current = Self::text(doc, item.para);
        let untouched = self.status(doc, r, i) == Status::Pending;
        let (text, value) = match decision {
            Decision::Accept(k) => (item.options[k].clone(), Some(format!("accepted:{k}"))),
            Decision::KeepMine => (if untouched { current } else { item.before.clone() }, Some("rejected".into())),
            Decision::Rewrite(t) => (t, Some("rewritten".into())),
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
        let value = match item.options.iter().position(|o| *o == text) {
            Some(k) => format!("accepted:{k}"),
            None if text == item.before => "rejected".into(),
            None => "rewritten".into(),
        };
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
    pub fn save_progress(&self, doc: &Document, r: usize) -> String {
        let review = &self.reviews[r];
        let item_of: HashMap<usize, usize> = review.items.iter().enumerate().map(|(i, it)| (it.entry, i)).collect();
        let mut blocks: Vec<(usize, Entry)> = review
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
                } else if review.entry_index[e].is_none() {
                    // Unmatched: kept as written, at the end.
                    return (usize::MAX, review.file.entries[e].clone());
                }
                (review.entry_index[e].unwrap_or(usize::MAX), entry)
            })
            .collect();
        blocks.sort_by_key(|(pos, _)| *pos);
        ReviewFile {
            script: review.file.script.clone(),
            entries: blocks.into_iter().map(|(_, e)| e).collect(),
            warnings: vec![],
        }
        .serialize()
    }
}
