//! Re-find: when a member's file and the links file disagree (Final Draft
//! saved it, or a write was interrupted), find each listed paragraph again
//! in the new file. Rules: `docs/plans/versions.md`, "Re-find".
//!
//! 1. Exact stamp text, in order (LCS on kind + stamp text). Blank stamps,
//!    missing stamps and blank paragraphs never count as equal here, so a
//!    run of blank lines can't be paired out of order (C3).
//! 2. Inside each changed stretch, same-kind lines in order: as many pairs
//!    as the kinds allow (LCS on kinds), and among those pairings the one
//!    whose texts are most alike (`core::text::similarity`), so a reworded
//!    line with a same-kind line inserted next to it keeps its row. Equal
//!    runs pair by position. Blanks pair here, and so does a fully
//!    rewritten line between found neighbours. A line split in two (or
//!    more): when the joined halves read more like the old line than any
//!    half alone, the old line pairs with the first half and the rest are
//!    its split rest, kept in the same row.
//! 3. Left over: new paragraphs of the made member stay unlisted (D3); new
//!    paragraphs of any other member get a row each, empty on the other
//!    sides, stamped `=` with their text (C4); one that falls inside a
//!    multi-id cell joins that cell, so rows never cross, and a split rest
//!    joins its first half's cell. (The made member's split rest is
//!    unlisted and follows its first half by D3.) Old ids left over
//!    leave their rows; a row left with no ids at all is dropped.
//!
//! Re-find changes only ids (and the member's hash), never existing stamps.

use std::collections::{BTreeMap, HashMap, HashSet};

use scriptorium_core::diff::{Tag, opcodes};
use scriptorium_core::text::similarity;
use scriptorium_core::{ElementKind, ParaId, Script};

use crate::links::{Links, Para, Row, Stamp};

/// What re-find found: which old id is which new one, and what's left over.
///
/// The shell's message ("PURPLE LIST.fdx changed outside Scriptorium: X
/// lines reworded, Y new, Z removed; …") reads: reworded =
/// [`Alignment::changed`], new = `new.len()`, removed = `gone.len()`; the
/// rest (`exact`) are where they were. For the made member, `new` are the lines left unlisted
/// (D3), not new rows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Alignment {
    /// (old id, new id), ascending on both sides.
    pub pairs: Vec<(ParaId, ParaId)>,
    /// How many of `pairs` matched on exact text (step 1); the rest paired
    /// by kind and likeness (step 2) and count as changed.
    pub exact: usize,
    /// New paragraphs nothing old paired with, in file order. For the
    /// made member these stay unlisted; for another member each gets a row.
    pub new: Vec<ParaId>,
    /// Old ids nothing new paired with, ascending.
    pub gone: Vec<ParaId>,
    /// (first half, rest): new lines in `new` that are the rest of a line
    /// split in the file, with the new id of its first half (paired). For
    /// a member other than the made one, `refind` puts them in the first
    /// half's cell rather than rows of their own.
    pub split: Vec<(ParaId, ParaId)>,
}

impl Alignment {
    /// Pairs found by position rather than exact text (step 2).
    pub fn changed(&self) -> usize {
        self.pairs.len() - self.exact
    }
}

/// A comparison key for step 1: kind plus stamp text, or a key equal to
/// nothing (a blank or a paragraph with no stamp).
#[derive(Debug, PartialEq)]
enum Key<'a> {
    Text(&'a ElementKind, String),
    /// Never equal across sides: (side, index).
    Unique(bool, usize),
}

fn key<'a>(side: bool, index: usize, kind: &'a ElementKind, text: Option<String>) -> Key<'a> {
    match text {
        Some(t) if !t.is_empty() => Key::Text(kind, t),
        _ => Key::Unique(side, index),
    }
}

/// The stamp text of a paragraph of `kind` (see `links::stamp_text`).
fn stamp_text(kind: &ElementKind, text: &str, is_caps: &dyn Fn(&ElementKind) -> bool) -> String {
    crate::links::stamp_text(text, is_caps(kind))
}

/// Re-find the paragraphs `old` lists (one member's `Links::paras`: ids in
/// file order, each with its kind and stamp) in `new`, that member's file
/// as it reads now. `is_caps` says which kinds are shown in capitals
/// (`FdxFile::is_caps`).
///
/// Precondition: `new` is the file as parsed (`FdxFile::to_script`), its
/// ids `0..n` in file order.
pub fn by_text(old: &BTreeMap<ParaId, Para>, new: &Script, is_caps: &dyn Fn(&ElementKind) -> bool) -> Alignment {
    debug_assert!(as_parsed(new), "re-find needs the file as parsed: ids 0..n in order");
    let old: Vec<(ParaId, &Para)> = old.iter().map(|(id, p)| (*id, p)).collect();
    let new_paras = &new.paragraphs;

    let a: Vec<Key> = old
        .iter()
        .enumerate()
        .map(|(i, (_, p))| {
            let text = p.stamp.as_ref().map(|s| stamp_text(&p.kind, s.text(), is_caps));
            key(false, i, &p.kind, text)
        })
        .collect();
    let b: Vec<Key> = new_paras
        .iter()
        .enumerate()
        .map(|(j, p)| key(true, j, &p.kind, Some(stamp_text(&p.kind, &p.text(), is_caps))))
        .collect();

    let mut pairs = Vec::new();
    let mut split = Vec::new();
    let mut exact = 0;
    for op in opcodes(&a, &b) {
        match op.tag {
            Tag::Equal => {
                for k in 0..op.i2 - op.i1 {
                    pairs.push((old[op.i1 + k].0, new_paras[op.j1 + k].id));
                }
                exact += op.i2 - op.i1;
            }
            Tag::Replace => {
                // Step 2: same-kind lines in order.
                let olds: Vec<(&ElementKind, &str)> = old[op.i1..op.i2]
                    .iter()
                    .map(|(_, p)| (&p.kind, p.stamp.as_ref().map_or("", |s| s.text())))
                    .collect();
                let news: Vec<(&ElementKind, String)> =
                    new_paras[op.j1..op.j2].iter().map(|p| (&p.kind, p.text())).collect();
                let found = pair_stretch(&olds, &news);
                let (found, rests) = find_splits(&olds, &news, found);
                for (i, j) in found {
                    pairs.push((old[op.i1 + i].0, new_paras[op.j1 + j].id));
                }
                split.extend(rests.into_iter().map(|(h, r)| (new_paras[op.j1 + h].id, new_paras[op.j1 + r].id)));
            }
            Tag::Insert | Tag::Delete => {}
        }
    }

    let paired_old: HashSet<ParaId> = pairs.iter().map(|(o, _)| *o).collect();
    let paired_new: HashSet<ParaId> = pairs.iter().map(|(_, n)| *n).collect();
    Alignment {
        exact,
        new: new_paras.iter().map(|p| p.id).filter(|id| !paired_new.contains(id)).collect(),
        gone: old.iter().map(|(id, _)| *id).filter(|id| !paired_old.contains(id)).collect(),
        pairs,
        split,
    }
}

/// Whether `script` has ids `0..n` in order, as `FdxFile::to_script` gives.
fn as_parsed(script: &Script) -> bool {
    script.paragraphs.iter().enumerate().all(|(i, p)| p.id.0 == i as u64)
}

/// Above this many (old × new) cells, step 2 skips the similarity pass and
/// pairs same-kind lines prefix-first (a stretch that big is a rewrite).
const SIMILARITY_CELLS: usize = 40_000;

/// Step 2 for one changed stretch: index pairs `(old, new)`, ascending,
/// same kind only. The most pairs the kinds allow; among pairings with that
/// many, the greatest total similarity; ties go to the earliest pairs.
/// Blanks are alike to nothing.
fn pair_stretch(old: &[(&ElementKind, &str)], new: &[(&ElementKind, String)]) -> Vec<(usize, usize)> {
    let (n, m) = (old.len(), new.len());
    if n * m > SIMILARITY_CELLS {
        let ka: Vec<&ElementKind> = old.iter().map(|(k, _)| *k).collect();
        let kb: Vec<&ElementKind> = new.iter().map(|(k, _)| *k).collect();
        return opcodes(&ka, &kb)
            .into_iter()
            .filter(|op| op.tag == Tag::Equal)
            .flat_map(|op| (0..op.i2 - op.i1).map(move |k| (op.i1 + k, op.j1 + k)))
            .collect();
    }
    // A pair is worth more than any total of similarities (each ≤ 1000).
    let unit = 1000 * (n.min(m) as u64 + 1);
    let weight = |i: usize, j: usize| -> Option<u64> {
        let ((ka, a), (kb, b)) = (&old[i], &new[j]);
        if ka != kb {
            return None;
        }
        let alike = if a.trim().is_empty() || b.trim().is_empty() { 0.0 } else { similarity(a, b) };
        Some(unit + (alike * 1000.0).round() as u64)
    };
    // best[i][j]: best score pairing old[i..] with new[j..].
    let w = m + 1;
    let mut best = vec![0u64; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let take = weight(i, j).map_or(0, |s| s + best[(i + 1) * w + j + 1]);
            best[i * w + j] = take.max(best[(i + 1) * w + j]).max(best[i * w + j + 1]);
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if let Some(s) = weight(i, j)
            && s + best[(i + 1) * w + j + 1] == best[i * w + j]
        {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if best[(i + 1) * w + j] == best[i * w + j] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// How alike (`similarity`) a split's joined halves must be to the old
/// line, and by how much more than any one half: a reworded line next to
/// an added one is not a split.
const SPLIT_ALIKE: f64 = 0.6;
const SPLIT_MARGIN: f64 = 0.1;
/// Joined halves this alike are the old line itself (a split and nothing
/// else), so any gain over the best half will do.
const SPLIT_SAME: f64 = 0.95;

/// (old or first-half index, new index) within one stretch.
type IndexPair = (usize, usize);

/// How far either way a split is looked for around a paired line.
const SPLIT_REACH: usize = 8;

/// Splits in one stretch, after [`pair_stretch`]: for each pair (i, j),
/// the unpaired same-kind lines right before and after j may be halves of
/// old line i split in the file. The range around j whose joined text
/// (whitespace-joined) is most like old i is taken when it reads clearly
/// like it (`SPLIT_ALIKE`) and clearly more than any one line in it does
/// (`SPLIT_MARGIN`), or reads as the old line itself (`SPLIT_SAME`) and
/// more than any one line; then i pairs with the range's first line and the
/// others are its rest. Pairs claim in order; a line claimed once is never
/// claimed again. Returns the pairs and (first, rest)
/// index pairs. Blank old lines have nothing to compare, so never split.
fn find_splits(
    old: &[(&ElementKind, &str)],
    new: &[(&ElementKind, String)],
    pairs: Vec<(usize, usize)>,
) -> (Vec<IndexPair>, Vec<IndexPair>) {
    // Lines a pair holds or a split claimed; updated as splits claim more,
    // so no line is ever claimed twice.
    let mut taken: HashSet<usize> = pairs.iter().map(|(_, j)| *j).collect();
    let mut out = Vec::with_capacity(pairs.len());
    let mut rests = Vec::new();
    for (i, j) in pairs {
        let (kind, text) = old[i];
        if text.trim().is_empty() {
            out.push((i, j));
            continue;
        }
        let free = |k: usize| !taken.contains(&k) && new[k].0 == kind;
        let mut lo = j;
        while lo > 0 && j - lo < SPLIT_REACH && free(lo - 1) {
            lo -= 1;
        }
        let mut hi = j;
        while hi + 1 < new.len() && hi - j < SPLIT_REACH && free(hi + 1) {
            hi += 1;
        }
        let mut best: Option<(f64, usize, usize)> = None;
        for s in lo..=j {
            for e in j..=hi {
                if s == e {
                    continue;
                }
                let joined = new[s..=e].iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(" ");
                let alike = similarity(text, &joined);
                let single = new[s..=e].iter().map(|(_, t)| similarity(text, t)).fold(0.0, f64::max);
                let clear =
                    (alike >= SPLIT_ALIKE && alike >= single + SPLIT_MARGIN) || (alike >= SPLIT_SAME && alike > single);
                if clear && best.is_none_or(|(b, _, _)| alike > b) {
                    best = Some((alike, s, e));
                }
            }
        }
        match best {
            Some((_, s, e)) => {
                taken.extend(s..=e);
                out.push((i, s));
                rests.extend((s + 1..=e).map(|r| (s, r)));
            }
            None => out.push((i, j)),
        }
    }
    (out, rests)
}

/// Re-find member `member` of `links` in `new` (its file as it reads now,
/// whose bytes hash to `hash`) and return the links file rewritten for it,
/// with what was found. Ids are mapped; the member's hash becomes `hash`;
/// existing stamps never change. New paragraphs of a member that isn't the
/// made one (`created:`) get a row of their own, stamped `=` with their
/// text so the next re-find finds them exactly, right after the row of the
/// nearest listed paragraph above (C4); one that falls between the first
/// and last id of a cell joins that cell, and the rest of a split line
/// joins its first half's cell (`Alignment::split`), stamped likewise. The
/// made member's new paragraphs stay unlisted (D3). Rows left with no ids
/// are dropped.
///
/// Precondition: as for [`by_text`], `new` is the file as parsed (ids
/// `0..n` in file order).
pub fn refind(
    links: &Links,
    member: usize,
    new: &Script,
    hash: &str,
    is_caps: &dyn Fn(&ElementKind) -> bool,
) -> (Links, Alignment) {
    let alignment = by_text(&links.paras[member], new, is_caps);
    let map: HashMap<ParaId, ParaId> = alignment.pairs.iter().copied().collect();

    let mut out = links.clone();
    out.members[member].hash = hash.to_string();
    let mut paras: BTreeMap<ParaId, Para> =
        links.paras[member].iter().filter_map(|(id, p)| map.get(id).map(|n| (*n, p.clone()))).collect();

    // The rest of a split line follows its first half into its cell; only
    // listed for a member other than the made one.
    let listing = links.made() != Some(member);
    let mut rest_of: HashMap<ParaId, Vec<ParaId>> = HashMap::new();
    if listing {
        for (head, rest) in &alignment.split {
            rest_of.entry(*head).or_default().push(*rest);
        }
    }

    // Mapped rows; the map is monotone and a rest comes right after its
    // first half, so cells stay ascending.
    let members = links.members.len();
    let mapped = links.rows.iter().map(|row| {
        let mut row = row.clone();
        row.cells[member] = row.cells[member]
            .iter()
            .filter_map(|id| map.get(id).copied())
            .flat_map(|id| std::iter::once(id).chain(rest_of.get(&id).into_iter().flatten().copied()))
            .collect();
        row
    });

    // New rows for the original's new paragraphs (C4); split rests are
    // stamped too but stay in their first half's cell.
    let rests: HashSet<ParaId> = alignment.split.iter().map(|(_, r)| *r).collect();
    let mut fresh: Vec<ParaId> = Vec::new();
    if listing {
        for id in &alignment.new {
            if let Some(p) = new.paragraphs.get(id.0 as usize) {
                let stamp = Stamp::Synced(stamp_text(&p.kind, &p.text(), is_caps));
                paras.insert(*id, Para { kind: p.kind.clone(), stamp: Some(stamp) });
                if !rests.contains(id) {
                    fresh.push(*id);
                }
            }
        }
    }
    let own_row = |id: ParaId| {
        let mut cells = vec![Vec::new(); members];
        cells[member] = vec![id];
        Row { cells }
    };

    // Merge: a new paragraph goes right after the row holding the nearest
    // listed paragraph above it, before rows with nothing on this side; or
    // into the cell whose ids it falls between.
    let mut rows = Vec::with_capacity(links.rows.len() + fresh.len());
    let mut held: Vec<Row> = Vec::new();
    let mut fresh = fresh.into_iter().peekable();
    for row in mapped {
        if row.cells.iter().all(|c| c.is_empty()) {
            continue;
        }
        match (row.cells[member].first().copied(), row.cells[member].last().copied()) {
            (Some(first), Some(last)) => {
                while let Some(id) = fresh.next_if(|id| *id < first) {
                    rows.push(own_row(id));
                }
                rows.append(&mut held);
                let mut row = row;
                while let Some(id) = fresh.next_if(|id| *id < last) {
                    row.cells[member].push(id);
                }
                row.cells[member].sort();
                rows.push(row);
            }
            _ => held.push(row),
        }
    }
    rows.extend(fresh.map(own_row));
    rows.append(&mut held);

    out.rows = rows;
    out.paras[member] = paras;
    (out, alignment)
}

#[cfg(test)]
mod tests;
