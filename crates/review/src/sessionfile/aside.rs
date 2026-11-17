//! Whether a saved session that no longer fits its script (the script's
//! bytes changed: Final Draft saved it) can be set aside without losing
//! anything: it holds nothing the files don't. The CLI then starts fresh
//! from the script and the review files instead of refusing
//! (`docs/plans/versions.md`, "Opening a version from the CLI after a Final
//! Draft save").
//!
//! The saved events can't be replayed (their paragraph ids belong to the old
//! bytes), so this reads only what the log leaves behind
//! ([`scriptorium_core::doc::undo_groups`]): the decision metadata, which
//! paragraphs each step changed, and the text a rewrite left.

use std::collections::{BTreeMap, HashMap, HashSet};

use scriptorium_core::doc::{Op, Transaction, undo_groups};
use scriptorium_core::{Document, ParaId, spans};

use super::SessionFile;
use crate::file::{Entry, FileStatus, Line};
use crate::meta::{MetaValue, Now, Stored};
use crate::{Item, Session};

/// Decision metadata keys (`review/<review id>/<entry>`, see `session.rs`).
const PREFIX: &str = "review/";

/// Why a saved session can't be set aside quietly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kept {
    /// A step still applied isn't a decision on its own lines: typing, a
    /// line split or joined by hand, an edit elsewhere merged into a
    /// rewrite.
    Typing,
    /// A decision differs from what the review files say now (one side has
    /// a decision the other hasn't, another option, or a rewrite whose text
    /// isn't the file's `MINE`).
    Decisions,
    /// A version 1 session with edits: its starting point isn't saved, so
    /// what it holds can't be told apart from the files.
    Old,
}

impl Kept {
    /// What the session holds that the files don't, for a refusal.
    pub fn what(&self) -> &'static str {
        match self {
            Kept::Typing => "typing that isn't in any file",
            Kept::Decisions => "decisions that aren't saved in a review file",
            Kept::Old => "work saved by an older Scriptorium, too old to check against the files",
        }
    }
}

/// `Ok` if `saved` holds nothing the files don't:
/// - every step its log leaves applied is a decision, and changes only the
///   lines that decision names (a rewrite's typing is merged into its
///   decision's undo step, so it counts; a split, join, added or removed
///   line counts only inside a block decision's own lines);
/// - its decisions, per key, equal the review files' saved progress
///   (`STATUS`/`CHOSEN` as written in `session`'s files, whether or not the
///   entry is placed in `fresh` now: Final Draft may have changed its line),
///   compared by what was decided, never by the paragraph ids after `@`
///   (they're the old file's);
/// - every line the session's edits touched ends as the files say: taking
///   those edits back, last first, from the line's text in `fresh` (the
///   script opened fresh from its file with that progress) must meet
///   each one where it was made and end on the line's text when the session
///   began (the suggestion's `BEFORE`, or the option its earlier decision
///   accepted). A line rewritten before the session began (its old `MINE` is
///   gone) or a block decided in it can't be checked that way and counts as
///   not saved.
///
/// A session with no events of its own (only the starting point it took
/// from the review files, as a first open or a write in place leaves it)
/// holds nothing beyond them.
pub fn only_decisions(saved: &SessionFile, fresh: &Document, session: &Session) -> Result<(), Kept> {
    if saved.events.is_empty() {
        return Ok(());
    }
    let Some(base) = &saved.base else { return Err(Kept::Old) };
    let events = undo_groups(&saved.events);
    if !events.iter().all(|group| own_lines_only(group)) {
        return Err(Kept::Typing);
    }
    let mut start: BTreeMap<&str, Option<&str>> = BTreeMap::new();
    for t in undo_groups(base).into_iter().flatten() {
        for (key, to) in decision_ops(t) {
            start.insert(key, to);
        }
    }
    let mut meta = start.clone();
    for t in events.iter().flatten() {
        for (key, to) in decision_ops(t) {
            meta.insert(key, to);
        }
    }
    let was = decisions(meta.iter().filter_map(|(k, v)| Some((*k, (*v)?))));
    let files = saved_progress(session);
    if was.len() != files.len() || !was.iter().all(|(k, v)| v.as_ref().ok() == files.get(*k)) {
        return Err(Kept::Decisions);
    }
    let applied: Vec<&Op> = events.iter().flatten().flat_map(|t| &t.ops).collect();
    let lines = line_keys(&events).ok_or(Kept::Decisions)?;
    let mut touched: Vec<ParaId> = vec![];
    for op in &applied {
        match op {
            Op::SetMeta { .. } => {}
            Op::Splice { para, .. } => {
                if !touched.contains(para) {
                    touched.push(*para);
                }
            }
            // A changed kind, or lines split, joined, added or removed: a
            // block decision, which this can't check.
            _ => return Err(Kept::Decisions),
        }
    }
    for para in touched {
        let key = lines.get(&para).ok_or(Kept::Decisions)?;
        let (entry, item) = entry_of(session, key).ok_or(Kept::Decisions)?;
        // An item placed now reads its texts from the line it's on; one that
        // isn't (Final Draft changed its line) from its file entry, one line.
        let one = |lines: Option<&Vec<Line>>| match lines.map(Vec::as_slice) {
            Some([line]) => Ok(line.text.clone()),
            _ => Err(Kept::Decisions),
        };
        let before = || item.map_or_else(|| one(Some(&entry.before)), |it| Ok(it.before.clone()));
        let option = |k: usize| match item {
            Some(it) => it.options.get(k).cloned().ok_or(Kept::Decisions),
            None => one(entry.options.get(k)),
        };
        let began = match start.get(key).copied().flatten().map(MetaValue::parse) {
            None | Some(Some(MetaValue { decision: Stored::Rejected, .. })) => before()?,
            Some(Some(MetaValue { decision: Stored::Accepted(k), .. })) => option(k)?,
            Some(_) => return Err(Kept::Decisions),
        };
        let ends = match item {
            Some(it) => fresh.paragraph(it.para).map(|p| p.text()).ok_or(Kept::Decisions)?,
            None => match entry.status {
                FileStatus::Pending | FileStatus::Rejected => one(Some(&entry.before))?,
                FileStatus::Accepted => one(entry.options.get(entry.chosen.unwrap_or(0)))?,
                FileStatus::Rewritten => one(entry.mine.as_ref())?,
            },
        };
        if taken_back(ends, para, &applied).as_deref() != Some(began.as_str()) {
            return Err(Kept::Decisions);
        }
    }
    Ok(())
}

/// The decision key each line a session decision names belongs to (from the
/// values its steps set or took back); `None` if a line belongs to two.
fn line_keys<'a>(events: &[Vec<&'a Transaction>]) -> Option<HashMap<ParaId, &'a str>> {
    let mut out: HashMap<ParaId, &str> = HashMap::new();
    for op in events.iter().flatten().flat_map(|t| &t.ops) {
        let Op::SetMeta { key, from, to } = op else { continue };
        if !key.starts_with(PREFIX) {
            continue;
        }
        for v in [from, to].into_iter().flatten().filter_map(|v| MetaValue::parse(v)) {
            for p in v.paras.into_iter().flatten() {
                if out.insert(p, key.as_str()).is_some_and(|k| k != key) {
                    return None;
                }
            }
        }
    }
    Some(out)
}

/// The file entry a decision key (`review/<review id>/<entry>`) is for, and
/// its item in the fresh session if it is placed there; `None` for a block.
fn entry_of<'s>(session: &'s Session, key: &str) -> Option<(&'s Entry, Option<&'s Item>)> {
    let (id, entry) = key.strip_prefix(PREFIX)?.rsplit_once('/')?;
    let e: usize = entry.parse().ok()?;
    let review = session.reviews.iter().find(|r| r.id == id)?;
    let item = review.items.iter().find(|it| it.entry == e);
    if item.is_some_and(|it| it.block) {
        return None;
    }
    Some((review.file.entries.get(e)?, item))
}

/// `text` with every edit `applied` made to `para` taken back, last first;
/// `None` if one doesn't find the text it inserted where it inserted it.
fn taken_back(mut text: String, para: ParaId, applied: &[&Op]) -> Option<String> {
    for op in applied.iter().rev() {
        if let Op::Splice { para: p, at, removed, inserted } = op
            && *p == para
        {
            let inserted = spans::text(inserted);
            if text.get(*at..at + inserted.len()) != Some(inserted.as_str()) {
                return None;
            }
            text.replace_range(*at..at + inserted.len(), &spans::text(removed));
        }
    }
    Some(text)
}

/// The decision keys a transaction sets, with their new values.
fn decision_ops(t: &Transaction) -> impl Iterator<Item = (&str, Option<&str>)> {
    t.ops.iter().filter_map(|op| match op {
        Op::SetMeta { key, to, .. } if key.starts_with(PREFIX) => Some((key.as_str(), to.as_deref())),
        _ => None,
    })
}

/// Whether an undo group is decisions only: it sets a decision, and its text
/// ops touch only the paragraphs its decisions (made or taken back) name. A
/// split, join, added or removed paragraph is allowed only in a block
/// decision (one whose value names several lines or what it left).
fn own_lines_only(group: &[&Transaction]) -> bool {
    let (mut own, mut block, mut any) = (HashSet::new(), false, false);
    for op in group.iter().flat_map(|t| &t.ops) {
        let Op::SetMeta { key, from, to } = op else { continue };
        if !key.starts_with(PREFIX) {
            continue;
        }
        any = true;
        for v in [from, to].into_iter().flatten().filter_map(|v| MetaValue::parse(v)) {
            block |= v.now.is_some() || v.paras.as_ref().is_some_and(|ps| ps.len() > 1);
            own.extend(v.paras.into_iter().flatten());
            match v.now {
                Some(Now::Ids(ids)) => own.extend(ids),
                Some(Now::After(_) | Now::Start) | None => {}
            }
        }
    }
    let mine = |p: &ParaId| own.contains(p);
    any && group.iter().flat_map(|t| &t.ops).all(|op| match op {
        Op::SetMeta { .. } => true,
        Op::Splice { para, .. } | Op::SetKind { para, .. } => mine(para),
        Op::Split { para, new_para: other, .. } | Op::Merge { para, next: other, .. } => {
            block && mine(para) && mine(other)
        }
        Op::InsertPara { para, .. } | Op::RemovePara { para, .. } => block && mine(&para.id),
    })
}

/// The decisions the review files hold, under the keys a session stores them
/// by: `accepted` with the option (the file's `CHOSEN` mapped to the item's
/// option index where the entry is placed now; where it isn't, the file's
/// own number, which is the same unless an option repeats the `BEFORE`
/// line), `rejected`, `rewritten`. Pending entries have none.
fn saved_progress(session: &Session) -> BTreeMap<String, Stored> {
    let mut out = BTreeMap::new();
    for review in &session.reviews {
        for (e, entry) in review.file.entries.iter().enumerate() {
            let decision = match entry.status {
                FileStatus::Pending => continue,
                FileStatus::Rejected => Stored::Rejected,
                FileStatus::Rewritten => Stored::Rewritten,
                FileStatus::Accepted => {
                    let chosen = entry.chosen.unwrap_or(0);
                    let item = review.items.iter().find(|it| it.entry == e);
                    let k = match item {
                        Some(it) => it.option_src.iter().position(|&src| src == chosen).unwrap_or(chosen),
                        None => chosen,
                    };
                    Stored::Accepted(k)
                }
            };
            out.insert(format!("{PREFIX}{}/{e}", review.id), decision);
        }
    }
    out
}

/// What was decided under each decision key: its decision, or the raw value
/// when it doesn't parse (compared as is).
fn decisions<'a>(meta: impl Iterator<Item = (&'a str, &'a str)>) -> BTreeMap<&'a str, Result<Stored, &'a str>> {
    meta.filter(|(k, _)| k.starts_with(PREFIX))
        .map(|(k, v)| (k, MetaValue::parse(v).map(|m| m.decision).ok_or(v)))
        .collect()
}
