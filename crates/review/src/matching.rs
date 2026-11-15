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

use scriptorium_core::ParaId;
use scriptorium_core::text::{keep_edge_whitespace, looks_same, normalize, similarity};

use crate::file::Entry;

pub const FUZZY_THRESHOLD: f64 = 0.85;
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
            let key = normalize(&e.before);
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
                    let s = similarity(&e.before, p.text);
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
    for (k, o) in e.options.iter().enumerate() {
        if !looks_same(o, &before, p.caps) {
            options.push(keep_edge_whitespace(&before, o));
            option_src.push(k);
        }
    }
    Match { index, para: p.id, fuzzy, score, before, options, option_src }
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
pub fn match_live(
    original: &[Para],
    live: &[Para],
    entries: &[Entry],
    used: &HashSet<ParaId>,
    has_meta: impl Fn(usize, ParaId) -> bool,
) -> Vec<Option<Match>> {
    let mut used = used.clone();
    let mut out = match_para(original, live, entries, &mut used, has_meta);
    let rest: Vec<usize> = (0..entries.len()).filter(|&n| out[n].is_none()).collect();
    if rest.is_empty() {
        return out;
    }
    let live_at: HashMap<ParaId, usize> = live.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
    let paras: Vec<Para> =
        original.iter().map(|p| Para { editable: p.editable && !used.contains(&p.id), ..p.clone() }).collect();
    let rest_entries: Vec<Entry> = rest.iter().map(|&n| entries[n].clone()).collect();
    for (n, m) in rest.into_iter().zip(match_entries(&paras, &rest_entries)) {
        if let Some(m) = m
            && let Some(&li) = live_at.get(&m.para)
        {
            out[n] = Some(Match { index: li, ..m });
        }
    }
    out
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
    let live_at: HashMap<ParaId, usize> = live.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
    let original_at: HashMap<ParaId, usize> = original.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
    let mut out: Vec<Option<Match>> = vec![None; entries.len()];
    for (n, e) in entries.iter().enumerate() {
        let Some(id) = e.para else { continue };
        let key = normalize(&e.before);
        let Some(&li) = live_at.get(&id) else { continue };
        let lp = &live[li];
        if key.is_empty() || used.contains(&id) || !lp.editable {
            continue;
        }
        let orig = original_at.get(&id).map(|&i| original[i].text);
        let same = |t: &str| normalize(t) == normalize(lp.text);
        let decided = || e.options.iter().any(|o| same(o)) || e.mine.as_deref().is_some_and(same) || has_meta(n, id);
        let before = match orig {
            Some(t) if normalize(t) == key => t.to_string(),
            _ if normalize(lp.text) == key => lp.text.to_string(),
            _ if decided() => keep_edge_whitespace(lp.text, &e.before),
            _ => continue,
        };
        used.insert(id);
        out[n] = Some(make_match(li, lp, before, false, 1.0, e));
    }
    out
}
