//! Finding each review entry's paragraph.
//!
//! Entries are matched by their `BEFORE` text, in script order: an exact
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
    /// Index of the matched paragraph in the slice given to [`match_entries`].
    pub index: usize,
    pub para: ParaId,
    pub fuzzy: bool,
    pub score: f64,
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
            let (mut options, mut option_src) = (vec![], vec![]);
            for (k, o) in e.options.iter().enumerate() {
                if !looks_same(o, p.text, p.caps) {
                    options.push(keep_edge_whitespace(p.text, o));
                    option_src.push(k);
                }
            }
            Some(Match { index: i, para: p.id, fuzzy, score, options, option_src })
        })
        .collect()
}
