//! Finding each review entry's paragraph.
//!
//! [`match_live`] is what reviews use: first each entry's `PARA:` paragraph, if
//! it still fits the entry, then [`match_entries`] for the rest.
//!
//! [`match_entries`] matches by `BEFORE` text, in script order: an exact
//! (normalized) match at or after the previous match wins, then an exact match
//! anywhere, then the most similar unused paragraph (fuzzy, flagged for the
//! UI). Ported from `script-review/src/core/match.js`.

use std::collections::{HashMap, HashSet};

pub use scriptorium_core::text::FUZZY_THRESHOLD;
use scriptorium_core::text::{keep_edge_whitespace, looks_same, normalize, similarity};
use scriptorium_core::{ElementKind, ParaId, Paragraph};

use crate::file::{Entry, Line};
use crate::meta::{MetaValue, Now};

/// How much the best fuzzy window of a block must beat every other by.
pub const FUZZY_MARGIN: f64 = 0.05;

/// Paragraphs ahead of the previous match that a fuzzy match prefers.
const FUZZY_WINDOW: usize = 400;

/// What matching needs to know about a paragraph.
#[derive(Debug, Clone)]
pub struct Para<'a> {
    pub id: ParaId,
    pub text: &'a str,
    /// Whether review suggestions may change it (false for e.g. "End of Act").
    pub editable: bool,
    /// Whether the script shows this element in capitals (case changes don't show).
    pub caps: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    /// Index of the matched paragraph in the slice given to [`match_entries`]
    /// (for [`match_live`]: in the live slice).
    pub index: usize,
    pub para: ParaId,
    pub fuzzy: bool,
    pub score: f64,
    /// The paragraph text the entry's `BEFORE` was matched against: what the
    /// suggestion is for, and what undoing a decision on it goes back to.
    pub before: String,
    /// The entry's options that would visibly change the paragraph, with the
    /// paragraph's own leading/trailing whitespace. Options that look the same
    /// as the original (e.g. only letter case in a character name) are left out.
    pub options: Vec<String>,
    /// `options[i]` came from the entry's option `option_src[i]`.
    pub option_src: Vec<usize>,
    /// The paragraph no longer exists live (removed, or joined into the line
    /// above, since the script was read): the entry is still placed on it,
    /// so its decision and its number survive, but it can't be decided.
    /// [`Match::index`] is then where the line would be (see [`match_live`]).
    pub gone: bool,
}

/// One result per entry, in order: `None` if the entry's paragraph wasn't found.
pub fn match_entries(paras: &[Para], entries: &[Entry]) -> Vec<Option<Match>> {
    let mut index: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, p) in paras.iter().enumerate() {
        let k = normalize(p.text);
        if p.editable && !k.is_empty() {
            index.entry(k).or_default().push(i);
        }
    }
    let mut used: HashSet<usize> = HashSet::new();
    let mut cursor = 0;
    entries
        .iter()
        .map(|e| {
            // Blocks are matched by B3; until then they find no paragraph.
            if !e.is_line() {
                return None;
            }
            let before = Line::join(&e.before);
            let key = normalize(&before);
            let exact: Vec<usize> =
                index.get(&key).map(|v| v.iter().copied().filter(|i| !used.contains(i)).collect()).unwrap_or_default();
            let (found, fuzzy, score) = if !exact.is_empty() {
                (Some(exact.iter().copied().find(|&i| i >= cursor).unwrap_or(exact[0])), false, 1.0)
            } else if !key.is_empty() {
                // (index, score, near the cursor)
                let mut best: Option<(usize, f64, bool)> = None;
                for (i, p) in paras.iter().enumerate() {
                    if !p.editable || used.contains(&i) || p.text.trim().is_empty() {
                        continue;
                    }
                    let s = similarity(&before, p.text);
                    if s < FUZZY_THRESHOLD {
                        continue;
                    }
                    let near = i >= cursor && i < cursor + FUZZY_WINDOW;
                    let better = match best {
                        None => true,
                        Some((_, bs, bn)) => near && !bn || (near == bn && s > bs),
                    };
                    if better {
                        best = Some((i, s, near));
                    }
                }
                match best {
                    Some((i, s, _)) => (Some(i), true, s),
                    None => (None, false, 0.0),
                }
            } else {
                (None, false, 0.0)
            };
            let i = found?;
            used.insert(i);
            cursor = i + 1;
            let p = &paras[i];
            Some(make_match(i, p, p.text.to_string(), fuzzy, score, e))
        })
        .collect()
}

/// A match on paragraph `p` (at `index`), with the entry's options filtered
/// and edge-whitespaced against `before`.
fn make_match(index: usize, p: &Para, before: String, fuzzy: bool, score: f64, e: &Entry) -> Match {
    let (mut options, mut option_src) = (vec![], vec![]);
    for (k, o) in e.options.iter().map(|o| Line::join(o)).enumerate() {
        if !looks_same(&o, &before, p.caps) {
            options.push(keep_edge_whitespace(&before, &o));
            option_src.push(k);
        }
    }
    Match { index, para: p.id, fuzzy, score, before, options, option_src, gone: false }
}

/// Where a paragraph gone from `live` would be: `gone_at(id)` if it knows,
/// else the live position of the next paragraph after it in `original` that
/// still exists, else the live length.
fn gone_index(
    original: &[Para],
    live_at: &HashMap<ParaId, usize>,
    live_len: usize,
    gone_at: &dyn Fn(ParaId) -> Option<usize>,
    id: ParaId,
) -> usize {
    gone_at(id).unwrap_or_else(|| {
        let from = original.iter().position(|p| p.id == id).map_or(original.len(), |i| i + 1);
        original[from..].iter().find_map(|p| live_at.get(&p.id).copied()).unwrap_or(live_len)
    })
}

/// Match `entries` against a script that may have been edited since it was
/// read: `original` is the script as read from its file, `live` as it is now
/// (the same slice twice when nothing has changed, or when only the live
/// script is known). Paragraphs in `used` (and each paragraph matched here)
/// take no further entry. One result per entry, in order; [`Match::index`] is
/// the paragraph's position in `live`.
///
/// 1. **`PARA:` pass.** An entry's `PARA:` paragraph is taken when it exists
///    live, is editable, and the entry fits it: `BEFORE` (normalized) equals
///    its original or its live text, or the entry was already decided (the
///    live text equals one of its `AFTER`s or its `MINE`, or
///    `has_meta(entry, para)` says the document holds a decision for it
///    made on that paragraph).
/// 2. **`BEFORE` pass** for every other entry: [`match_entries`] against
///    `original`. There is no second pass against the live text: pasted files
///    are written against the file, and tools always write `PARA:`. A match
///    whose paragraph has since been deleted is no match.
///
/// [`Match::before`] is the original text, else the live text, whichever
/// `BEFORE` equals; for a decided entry that equals neither, the `BEFORE`
/// text itself (with the live paragraph's edge whitespace), never the
/// post-decision text.
///
/// **Gone lines.** An entry whose paragraph has since been removed (or
/// joined away) keeps it, as a [`Match::gone`] match, so a reopened session
/// shows the same suggestions under the same numbers as before it was saved:
/// in the `PARA:` pass when the id isn't live but is in `original` with a
/// text that fits (or the entry was decided on it), or isn't in `original`
/// but `gone_at(id)` knows it (a line added since the script was read, then
/// removed); in the `BEFORE` pass when the original paragraph matched isn't
/// live. `gone_at(id)` is where a paragraph that existed but isn't live would
/// be now (a live position: that of the next surviving line), `None` if it
/// never existed; [`Match::index`] of a gone match is that (for an original
/// paragraph it doesn't know: the next surviving line of `original`).
pub fn match_live(
    original: &[Para],
    live: &[Para],
    entries: &[Entry],
    used: &HashSet<ParaId>,
    has_meta: impl Fn(usize, ParaId) -> bool,
    gone_at: impl Fn(ParaId) -> Option<usize>,
) -> Vec<Option<Match>> {
    let mut used = used.clone();
    let mut out = match_para_gone(original, live, entries, &mut used, has_meta, Some(&gone_at));
    before_pass(original, live, entries, &used, &gone_at, &mut out);
    out
}

/// The `BEFORE` pass of [`match_live`]: every line entry without a result
/// yet, matched by [`match_entries`] against `original` minus `used`.
fn before_pass(
    original: &[Para],
    live: &[Para],
    entries: &[Entry],
    used: &HashSet<ParaId>,
    gone_at: &dyn Fn(ParaId) -> Option<usize>,
    out: &mut [Option<Match>],
) {
    let rest: Vec<usize> = (0..entries.len()).filter(|&n| out[n].is_none() && entries[n].is_line()).collect();
    if rest.is_empty() {
        return;
    }
    let live_at: HashMap<ParaId, usize> = live.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
    let paras: Vec<Para> =
        original.iter().map(|p| Para { editable: p.editable && !used.contains(&p.id), ..p.clone() }).collect();
    let rest_entries: Vec<Entry> = rest.iter().map(|&n| entries[n].clone()).collect();
    for (n, m) in rest.into_iter().zip(match_entries(&paras, &rest_entries)) {
        out[n] = m.map(|m| match live_at.get(&m.para) {
            Some(&li) => Match { index: li, ..m },
            None => Match { index: gone_index(original, &live_at, live.len(), gone_at, m.para), gone: true, ..m },
        });
    }
}

/// The `PARA:` pass of [`match_live`] alone: an entry whose `PARA:`
/// paragraph doesn't fit it (or that has no `PARA:`) gets `None`, never a
/// match by `BEFORE`, which could land on another line with the same text.
/// Each paragraph matched is added to `used`.
pub fn match_para(
    original: &[Para],
    live: &[Para],
    entries: &[Entry],
    used: &mut HashSet<ParaId>,
    has_meta: impl Fn(usize, ParaId) -> bool,
) -> Vec<Option<Match>> {
    match_para_gone(original, live, entries, used, has_meta, None)
}

/// [`match_para`], also placing entries on gone lines (see [`match_live`])
/// when `gone_at` is given.
fn match_para_gone(
    original: &[Para],
    live: &[Para],
    entries: &[Entry],
    used: &mut HashSet<ParaId>,
    has_meta: impl Fn(usize, ParaId) -> bool,
    gone_at: Option<&dyn Fn(ParaId) -> Option<usize>>,
) -> Vec<Option<Match>> {
    let live_at: HashMap<ParaId, usize> = live.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
    let original_at: HashMap<ParaId, usize> = original.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
    let mut out: Vec<Option<Match>> = vec![None; entries.len()];
    for (n, e) in entries.iter().enumerate() {
        // Blocks are matched by B3; until then they find no paragraph.
        if !e.is_line() {
            continue;
        }
        let Some(&id) = e.para.first() else { continue };
        let e_before = Line::join(&e.before);
        let key = normalize(&e_before);
        if key.is_empty() || used.contains(&id) {
            continue;
        }
        let Some(&li) = live_at.get(&id) else {
            let Some(gone_at) = gone_at else { continue };
            let m = match original_at.get(&id).map(|&i| &original[i]) {
                Some(op) if op.editable && (normalize(op.text) == key || has_meta(n, id)) => {
                    let before = if normalize(op.text) == key {
                        op.text.to_string()
                    } else {
                        keep_edge_whitespace(op.text, &e_before)
                    };
                    make_match(0, op, before, false, 1.0, e)
                }
                Some(_) => continue,
                None if gone_at(id).is_some() => {
                    let p = Para { id, text: &e_before, editable: true, caps: false };
                    make_match(0, &p, e_before.clone(), false, 1.0, e)
                }
                None => continue,
            };
            used.insert(id);
            out[n] = Some(Match { index: gone_index(original, &live_at, live.len(), gone_at, id), gone: true, ..m });
            continue;
        };
        let lp = &live[li];
        if !lp.editable {
            continue;
        }
        let orig = original_at.get(&id).map(|&i| original[i].text);
        let same = |t: &str| normalize(t) == normalize(lp.text);
        let mine = e.mine.as_deref().filter(|m| !m.is_empty()).map(Line::join);
        let decided =
            || e.options.iter().any(|o| same(&Line::join(o))) || mine.as_deref().is_some_and(same) || has_meta(n, id);
        let before = match orig {
            Some(t) if normalize(t) == key => t.to_string(),
            _ if normalize(lp.text) == key => lp.text.to_string(),
            _ if decided() => keep_edge_whitespace(lp.text, &e_before),
            _ => continue,
        };
        used.insert(id);
        out[n] = Some(make_match(li, lp, before, false, 1.0, e));
    }
    out
}

// ---- blocks (B3) ------------------------------------------------------------

/// A paragraph for block matching: what [`Para`] says, plus the paragraph
/// itself (its kind, and its spans, to restore it from).
#[derive(Debug, Clone)]
pub struct Full<'a> {
    pub para: Para<'a>,
    pub source: &'a Paragraph,
}

/// Where a block entry was placed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BlockMatch {
    /// The ids it is placed on: while pending, the run it was written for;
    /// placed from a decision, the lines the decision left (empty: a cut).
    pub paras: Vec<ParaId>,
    /// The live position of the first live of `paras`, or where they (or
    /// the cut) would be.
    pub index: usize,
    pub fuzzy: bool,
    /// What the suggestion was made against, with the run's ids (C2): each
    /// `BEFORE` line as the paragraph read when it matched (live, else
    /// original), else as written; untyped lines take the paragraph's kind.
    pub before: Vec<Paragraph>,
    /// The options (every line typed) that would visibly change the run.
    pub options: Vec<Vec<Line>>,
    /// `options[i]` is the entry's option `option_src[i]`.
    pub option_src: Vec<usize>,
    /// None of `paras` is live (a cut, or lines removed since).
    pub gone: bool,
}

/// Why a block entry has no place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Miss {
    /// One of its lines already has an item of the review.
    Taken(ParaId),
    /// Matching by `PARA:` only, and it has none.
    NoPara,
    /// No line of its `PARA:` is live, or nothing fits its `BEFORE`.
    Missing,
    NotEditable,
    /// Its `PARA:` lines don't read as its `BEFORE` (or don't number as many).
    Mismatch,
    /// Several windows fit its `BEFORE` equally well.
    Ambiguous,
    /// Its lines were split, partly removed or moved apart.
    Restructured,
}

/// What [`match_review`] found for one entry.
#[derive(Debug, Clone)]
pub(crate) enum Found {
    Line(Match),
    Block(BlockMatch),
    /// No place; why, for blocks.
    None(Option<Miss>),
}

/// Whether `line` reads as paragraph `f`: same normalized text, same kind if typed.
fn fits(line: &Line, f: &Full) -> bool {
    line.kind.as_ref().is_none_or(|k| *k == f.source.kind) && normalize(&line.text) == normalize(f.para.text)
}

/// The kind an untyped line `j` of an option takes: that of the `BEFORE`
/// paragraph in its place, else of the last one.
pub(crate) fn kind_at(before: &[Paragraph], j: usize) -> ElementKind {
    before.get(j).or(before.last()).map_or(ElementKind::Action, |p| p.kind.clone())
}

/// `lines` with every kind filled in (see [`kind_at`]).
pub(crate) fn typed(lines: &[Line], before: &[Paragraph]) -> Vec<Line> {
    lines
        .iter()
        .enumerate()
        .map(|(j, l)| Line { kind: Some(l.kind.clone().unwrap_or_else(|| kind_at(before, j))), text: l.text.clone() })
        .collect()
}

struct Ctx<'s, 'a> {
    original: &'s [Full<'a>],
    live: &'s [Full<'a>],
    live_at: HashMap<ParaId, usize>,
    original_at: HashMap<ParaId, usize>,
    gone_at: &'s dyn Fn(ParaId) -> Option<usize>,
}

impl<'s, 'a> Ctx<'s, 'a> {
    fn new(original: &'s [Full<'a>], live: &'s [Full<'a>], gone_at: &'s dyn Fn(ParaId) -> Option<usize>) -> Self {
        Ctx {
            original,
            live,
            live_at: live.iter().enumerate().map(|(i, p)| (p.para.id, i)).collect(),
            original_at: original.iter().enumerate().map(|(i, p)| (p.para.id, i)).collect(),
            gone_at,
        }
    }

    fn live(&self, id: ParaId) -> Option<&Full<'a>> {
        self.live_at.get(&id).map(|&i| &self.live[i])
    }

    fn orig(&self, id: ParaId) -> Option<&Full<'a>> {
        self.original_at.get(&id).map(|&i| &self.original[i])
    }

    /// As [`gone_index`].
    fn gone_index(&self, id: ParaId) -> usize {
        (self.gone_at)(id).unwrap_or_else(|| {
            let from = self.original_at.get(&id).map_or(self.original.len(), |i| i + 1);
            self.original[from..].iter().find_map(|p| self.live_at.get(&p.para.id).copied()).unwrap_or(self.live.len())
        })
    }

    /// The entry's `BEFORE` lines on `ids` (see [`BlockMatch::before`]).
    fn before_paras(&self, e: &Entry, ids: &[ParaId]) -> Vec<Paragraph> {
        e.before
            .iter()
            .zip(ids)
            .map(|(line, &id)| {
                let (lp, op) = (self.live(id), self.orig(id));
                if let Some(f) = lp.filter(|f| fits(line, f)).or(op.filter(|f| fits(line, f))) {
                    return f.source.clone();
                }
                let kind = line
                    .kind
                    .clone()
                    .or_else(|| lp.or(op).map(|f| f.source.kind.clone()))
                    .unwrap_or(ElementKind::Action);
                Paragraph::plain(id, kind, line.text.clone())
            })
            .collect()
    }

    fn caps(&self, id: ParaId) -> bool {
        self.live(id).or(self.orig(id)).is_some_and(|f| f.para.caps)
    }

    /// The entry's options that would visibly change `before`.
    fn options(&self, e: &Entry, before: &[Paragraph]) -> (Vec<Vec<Line>>, Vec<usize>) {
        let (mut options, mut src) = (vec![], vec![]);
        for (k, o) in e.options.iter().enumerate() {
            let lines = typed(o, before);
            let same = lines.len() == before.len()
                && lines
                    .iter()
                    .zip(before)
                    .all(|(l, p)| l.kind.as_ref() == Some(&p.kind) && looks_same(&l.text, &p.text(), self.caps(p.id)));
            if !same {
                options.push(lines);
                src.push(k);
            }
        }
        (options, src)
    }

    /// Placed from its stored decision `v`, made on `before_ids` (C1).
    fn decided(
        &self,
        e: &Entry,
        before_ids: &[ParaId],
        v: &MetaValue,
        used: &HashSet<ParaId>,
    ) -> Result<BlockMatch, Miss> {
        let now = match &v.now {
            Some(Now::Ids(ids)) => ids.clone(),
            None => before_ids.to_vec(),
            Some(Now::After(_) | Now::Start) => vec![],
        };
        if let Some(&t) = now.iter().find(|id| used.contains(id)) {
            return Err(Miss::Taken(t));
        }
        let first_live = now.iter().filter_map(|id| self.live_at.get(id).copied()).min();
        let (index, gone) = match first_live {
            Some(i) => (i, false),
            None => (self.gone_index(now.first().copied().unwrap_or(before_ids[0])), true),
        };
        let before = self.before_paras(e, before_ids);
        let (options, option_src) = self.options(e, &before);
        Ok(BlockMatch { paras: now, index, fuzzy: false, before, options, option_src, gone })
    }

    /// Placed on `ids` (a `PARA:` list or a `BEFORE` window): they must be
    /// live, consecutive and editable. `fuzzy`: the window only fitted
    /// approximately, so what it was made against is the original text.
    fn placed(&self, e: &Entry, ids: &[ParaId], fuzzy: bool) -> Result<BlockMatch, Miss> {
        let pos: Vec<usize> = ids.iter().filter_map(|id| self.live_at.get(id).copied()).collect();
        if pos.is_empty() {
            return Err(Miss::Missing);
        }
        if pos.len() != ids.len() || pos.windows(2).any(|w| w[1] != w[0] + 1) {
            return Err(Miss::Restructured);
        }
        if pos.iter().any(|&i| !self.live[i].para.editable) {
            return Err(Miss::NotEditable);
        }
        let before = if fuzzy {
            ids.iter().map(|&id| self.orig(id).expect("window from the original").source.clone()).collect()
        } else {
            self.before_paras(e, ids)
        };
        let (options, option_src) = self.options(e, &before);
        Ok(BlockMatch { paras: ids.to_vec(), index: pos[0], fuzzy, before, options, option_src, gone: false })
    }

    /// The `PARA:` pass for one block. With `allow_gone` (opening a
    /// session, not appending), a block some of whose lines are gone is
    /// placed too, as it stayed before the session was saved (see
    /// [`Self::partly_removed`]).
    fn by_para(&self, e: &Entry, used: &HashSet<ParaId>, allow_gone: bool) -> Result<BlockMatch, Miss> {
        let ids = &e.para;
        if ids.len() != e.before.len() {
            return Err(Miss::Mismatch);
        }
        if let Some(&t) = ids.iter().find(|id| used.contains(id)) {
            return Err(Miss::Taken(t));
        }
        let m = match self.placed(e, ids, false) {
            Err(Miss::Restructured) if allow_gone => self.partly_removed(e, ids)?,
            m => m?,
        };
        for (line, &id) in e.before.iter().zip(ids) {
            // A gone line can't be read; `partly_removed` checked it existed.
            let Some(lp) = self.live(id) else { continue };
            if !fits(line, lp) && !self.orig(id).is_some_and(|op| fits(line, op)) {
                return Err(Miss::Mismatch);
            }
        }
        Ok(m)
    }

    /// A pending block whose lines are partly gone (removed by hand, or by
    /// another review), placed on all of `ids` so it keeps its number and
    /// shows as outdated, as before reopening: the live ones editable and in
    /// order, every gone one a line the script had (in the original, or
    /// known to `gone_at`). Lines split or moved out of order stay
    /// [`Miss::Restructured`].
    fn partly_removed(&self, e: &Entry, ids: &[ParaId]) -> Result<BlockMatch, Miss> {
        let pos: Vec<usize> = ids.iter().filter_map(|id| self.live_at.get(id).copied()).collect();
        let known =
            |id: &ParaId| self.live_at.contains_key(id) || self.orig(*id).is_some() || (self.gone_at)(*id).is_some();
        if pos.is_empty() || pos.len() == ids.len() || pos.windows(2).any(|w| w[1] <= w[0]) || !ids.iter().all(known) {
            return Err(Miss::Restructured);
        }
        if pos.iter().any(|&i| !self.live[i].para.editable) {
            return Err(Miss::NotEditable);
        }
        let before = self.before_paras(e, ids);
        let (options, option_src) = self.options(e, &before);
        Ok(BlockMatch { paras: ids.to_vec(), index: pos[0], fuzzy: false, before, options, option_src, gone: false })
    }

    /// The `BEFORE` window for one block: its ids, and whether only fuzzily.
    /// `decided_on` (the ids its stored decision was made on) is tried
    /// first: where it was decided wins over any other window that reads
    /// the same.
    fn by_before(
        &self,
        e: &Entry,
        used: &HashSet<ParaId>,
        cursor: usize,
        decided_on: Option<&[ParaId]>,
    ) -> Result<(Vec<ParaId>, bool), Miss> {
        let n = e.before.len();
        if n == 0 || n > self.original.len() {
            return Err(Miss::Missing);
        }
        let free = |k: usize| self.original[k].para.editable && !used.contains(&self.original[k].para.id);
        if let Some(ids) = decided_on.filter(|ids| ids.len() == n)
            && let Some(&s) = self.original_at.get(&ids[0])
            && s + n <= self.original.len()
            && (s..s + n).all(free)
            && self.original[s..s + n].iter().zip(ids).all(|(f, id)| f.para.id == *id)
            && e.before.iter().zip(&self.original[s..s + n]).all(|(l, f)| fits(l, f))
        {
            return Ok((ids.to_vec(), false));
        }
        let (mut exact, mut fuzzy) = (vec![], vec![]);
        for s in 0..=self.original.len() - n {
            if !(s..s + n).all(free) {
                continue;
            }
            let w = &self.original[s..s + n];
            if e.before.iter().zip(w).all(|(l, f)| fits(l, f)) {
                exact.push(s);
                continue;
            }
            let mut sum = 0.0;
            let all = e.before.iter().zip(w).all(|(l, f)| {
                let kind_ok = l.kind.as_ref().is_none_or(|k| *k == f.source.kind);
                let s = if kind_ok && !f.para.text.trim().is_empty() { similarity(&l.text, f.para.text) } else { 0.0 };
                sum += s;
                s >= FUZZY_THRESHOLD
            });
            if all {
                fuzzy.push((s, sum / n as f64));
            }
        }
        let start = if !exact.is_empty() {
            if n >= 2 && exact.len() > 1 {
                return Err(Miss::Ambiguous);
            }
            (exact.iter().copied().find(|&s| s >= cursor).unwrap_or(exact[0]), false)
        } else if !fuzzy.is_empty() {
            fuzzy.sort_by(|a, b| b.1.total_cmp(&a.1));
            if n >= 2 && fuzzy.len() > 1 && fuzzy[0].1 - fuzzy[1].1 < FUZZY_MARGIN {
                return Err(Miss::Ambiguous);
            }
            (fuzzy[0].0, true)
        } else {
            return Err(Miss::Missing);
        };
        Ok((self.original[start.0..start.0 + n].iter().map(|f| f.para.id).collect(), start.1))
    }
}

/// Whether an entry is matched as a block.
fn is_block(e: &Entry) -> bool {
    !e.is_line() && e.invalid.is_none() && !e.before.is_empty()
}

/// Every entry of a review matched against the script: line entries as
/// [`match_live`] (or [`match_para`] with `para_only`) does, blocks as the
/// module docs say. `decided(n)` is entry `n`'s stored decision, if any.
/// Paragraphs in `used` take no entry.
#[allow(clippy::too_many_arguments)]
pub(crate) fn match_review(
    original: &[Full],
    live: &[Full],
    entries: &[Entry],
    used: &HashSet<ParaId>,
    para_only: bool,
    has_meta: &dyn Fn(usize, ParaId) -> bool,
    decided: &dyn Fn(usize) -> Option<MetaValue>,
    gone_at: &dyn Fn(ParaId) -> Option<usize>,
) -> Vec<Found> {
    let ctx = Ctx::new(original, live, gone_at);
    let mut used = used.clone();
    let mut blocks: Vec<Option<Result<BlockMatch, Miss>>> = vec![None; entries.len()];
    type Slot = Option<Result<BlockMatch, Miss>>;
    let take = |blocks: &mut [Slot], n: usize, r: Result<BlockMatch, Miss>, used: &mut HashSet<ParaId>| {
        if let Ok(m) = &r {
            used.extend(m.paras.iter().copied());
        }
        blocks[n] = Some(r);
    };
    let named = |e: &Entry| is_block(e) && !e.para.is_empty();
    // 1. Decided blocks reserve the lines they left, and (from the `BEFORE`
    // pass only) the lines they were made on: those may be gone, or read as
    // they did, but no other entry was written for them. Repeated lines
    // elsewhere must not leave the `BEFORE` pass two windows to choose from.
    let mut made_on: HashMap<ParaId, usize> = HashMap::new();
    for n in (0..entries.len()).filter(|&n| is_block(&entries[n])) {
        for id in decided(n).and_then(|v| v.paras).unwrap_or_default() {
            made_on.entry(id).or_insert(n);
        }
    }
    for (n, e) in entries.iter().enumerate().filter(|(_, e)| named(e) && e.para.len() == e.before.len()) {
        if let Some(v) = decided(n).filter(|v| v.paras.as_deref() == Some(&e.para[..])) {
            take(&mut blocks, n, ctx.decided(e, &e.para, &v, &used), &mut used);
        }
    }
    // 2. `PARA:` pass: lines, then blocks.
    let lo: Vec<Para> = original.iter().map(|f| f.para.clone()).collect();
    let ll: Vec<Para> = live.iter().map(|f| f.para.clone()).collect();
    let mut lines = match_para_gone(&lo, &ll, entries, &mut used, has_meta, (!para_only).then_some(gone_at));
    for (n, e) in entries.iter().enumerate().filter(|(_, e)| named(e)) {
        if blocks[n].is_none() {
            take(&mut blocks, n, ctx.by_para(e, &used, !para_only), &mut used);
        }
    }
    // 3. `BEFORE` pass: lines, then blocks.
    if !para_only {
        let mut reserved = used.clone();
        reserved.extend(made_on.keys().copied());
        before_pass(&lo, &ll, entries, &reserved, gone_at, &mut lines);
        used.extend(lines.iter().flatten().map(|m| m.para));
        let mut cursor = 0;
        for (n, e) in entries.iter().enumerate().filter(|(_, e)| is_block(e) && e.para.is_empty()) {
            // Lines another decided entry was made on are not free; its own are.
            let mut free_of = used.clone();
            free_of.extend(made_on.iter().filter(|&(_, &m)| m != n).map(|(id, _)| *id));
            let on = decided(n).and_then(|v| v.paras);
            let r = ctx.by_before(e, &free_of, cursor, on.as_deref()).and_then(|(ids, fuzzy)| {
                cursor = ctx.original_at[ids.last().expect("a window")] + 1;
                match decided(n).filter(|v| v.paras.as_deref() == Some(&ids[..])) {
                    Some(v) => ctx.decided(e, &ids, &v, &used),
                    None => ctx.placed(e, &ids, fuzzy),
                }
            });
            take(&mut blocks, n, r, &mut used);
        }
    }
    lines
        .into_iter()
        .zip(blocks)
        .zip(entries)
        .map(|((l, b), e)| match (l, b) {
            (Some(m), _) => Found::Line(m),
            (None, Some(Ok(m))) => Found::Block(m),
            (None, Some(Err(miss))) => Found::None(Some(miss)),
            (None, None) if is_block(e) && para_only => Found::None(Some(Miss::NoPara)),
            (None, None) => Found::None(None),
        })
        .collect()
}
