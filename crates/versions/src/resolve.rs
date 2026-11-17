//! The resolver: where a `ROW:` suggestion goes (C1, C7). By row content
//! alone: the rows whose original side reads the entry's `ROW TEXT` and
//! whose version side reads one of its texts (`BEFORE`, an option, `MINE`).
//! Never by `PARA` or by matching text elsewhere. Rows that still read
//! the entry's `BEFORE` come first (so a pending entry never takes a row
//! already accepted with its option), and `PARA` only orders candidates
//! within that. Rules: `docs/plans/versions.md`, "The resolver".
//!
//! Review knows nothing about links: the host wraps a [`Resolver`] in the
//! closure review's open path takes, turning an entry into a [`RowQuery`].

use std::collections::HashMap;
use std::fmt;

use scriptorium_core::{ElementKind, ParaId, Paragraph, Script};

use crate::links::{Links, stamp_text};
use crate::rows::{live_cells, sides};

/// What review passes about one `ROW:` entry.
#[derive(Debug, Clone, Copy)]
pub struct RowQuery<'a> {
    /// Its `ROW:`: the links file's `Id:`.
    pub links_id: &'a str,
    /// Its `ROW TEXT:` lines (empty: the original's side was empty).
    pub row_text: &'a [String],
    /// The texts the version's side may read, each as its lines
    /// (`BEFORE: [empty]` is one empty line). What the host passes depends
    /// on the entry's state:
    /// - pending: `BEFORE` first, then each option;
    /// - accepted or rewritten: only the text it put there (the chosen
    ///   option, or `MINE`);
    /// - kept (rejected): only `BEFORE`.
    ///
    /// The first text counts as `BEFORE`: rows reading it are ranked first.
    pub texts: &'a [Vec<String>],
    /// Its `PARA:` ids, if any: among equally ranked candidates, the one
    /// holding them comes first.
    pub para: &'a [ParaId],
}

/// Why a `ROW:` entry has no row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotFound {
    /// Its `ROW:` names another links file.
    OtherPair,
    /// No row's original side reads its `ROW TEXT` now. `label` names the
    /// original ("English", or its file stem).
    OriginalChanged { label: String },
    /// Rows read its `ROW TEXT`, but their version side reads none of its
    /// texts (typed into by hand since, or split or joined).
    VersionReadsNone,
}

impl NotFound {
    pub fn text(&self) -> String {
        match self {
            NotFound::OtherPair => "this suggestion is for another pair of versions".to_string(),
            NotFound::OriginalChanged { label } => format!("the {label} line changed since this suggestion"),
            NotFound::VersionReadsNone => "the line no longer reads what this suggestion was made for".to_string(),
        }
    }
}

impl fmt::Display for NotFound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text())
    }
}

struct Candidate {
    /// The version's live paragraphs.
    cell: Vec<ParaId>,
    /// `stamp_text` of each, and whether its kind is in capitals.
    now: Vec<(String, bool)>,
    /// The original's side now, `stamp_text`, with caps.
    original: Vec<(String, bool)>,
}

/// The pair's rows, read once, for placing every `ROW:` entry of a review.
pub struct Resolver {
    id: String,
    /// The original's label, for [`NotFound::OriginalChanged`].
    original_label: String,
    rows: Vec<Candidate>,
    /// Rows by their original side, case-folded (exact checks after).
    by_original: HashMap<Vec<String>, Vec<usize>>,
}

/// Case-folded key, so a `ROW TEXT` written in lower case still finds a
/// row of capitals; the exact check per kind comes after.
fn key(text: &str) -> String {
    stamp_text(text, true)
}

impl Resolver {
    /// `original` is the original's file script (C8), `version` the
    /// version's live script. The caller has checked both hashes (C6).
    pub fn new(links: &Links, original: &Script, version: &Script, is_caps: &dyn Fn(&ElementKind) -> bool) -> Self {
        let (o, v) = sides(links);
        let orig: HashMap<ParaId, &Paragraph> = original.paragraphs.iter().map(|p| (p.id, p)).collect();
        let ver: HashMap<ParaId, &Paragraph> = version.paragraphs.iter().map(|p| (p.id, p)).collect();
        let read = |ids: &[ParaId], paras: &HashMap<ParaId, &Paragraph>| -> Vec<(String, bool)> {
            ids.iter()
                .map(|id| {
                    let p = paras[id];
                    let caps = is_caps(&p.kind);
                    (stamp_text(&p.text(), caps), caps)
                })
                .collect()
        };
        let mut rows = Vec::new();
        let mut by_original: HashMap<Vec<String>, Vec<usize>> = HashMap::new();
        for cells in live_cells(links, original, version) {
            let original = read(&cells[o], &orig);
            let now = read(&cells[v], &ver);
            by_original.entry(original.iter().map(|(t, _)| key(t)).collect()).or_default().push(rows.len());
            rows.push(Candidate { cell: cells[v].clone(), now, original });
        }
        Resolver { id: links.id.clone(), original_label: links.members[o].label(), rows, by_original }
    }

    /// The version-side cells the entry may go on: first those whose
    /// version side reads `texts[0]` (`BEFORE`, or a decided entry's one
    /// text), then the rest; each group in row order, except that a cell
    /// holding the `PARA` ids comes first within its group. Never empty.
    pub fn candidates(&self, q: &RowQuery) -> Result<Vec<Vec<ParaId>>, NotFound> {
        if q.links_id != self.id {
            return Err(NotFound::OtherPair);
        }
        let changed = || NotFound::OriginalChanged { label: self.original_label.clone() };
        let k: Vec<String> = q.row_text.iter().map(|t| key(t)).collect();
        let rows = self.by_original.get(&k).ok_or_else(changed)?;
        let reads = |want: &[String], now: &[(String, bool)]| {
            want.len() == now.len() && want.iter().zip(now).all(|(w, (n, caps))| stamp_text(w, *caps) == *n)
        };
        let rows: Vec<&Candidate> =
            rows.iter().map(|&r| &self.rows[r]).filter(|c| reads(q.row_text, &c.original)).collect();
        if rows.is_empty() {
            return Err(changed());
        }
        let mut first = Vec::new();
        let mut rest = Vec::new();
        for c in rows.into_iter().filter(|c| !c.cell.is_empty()) {
            match q.texts.iter().position(|t| reads(t, &c.now)) {
                Some(0) => first.push(c.cell.clone()),
                Some(_) => rest.push(c.cell.clone()),
                None => {}
            }
        }
        for group in [&mut first, &mut rest] {
            if !q.para.is_empty()
                && let Some(i) = group.iter().position(|cell| q.para.iter().all(|id| cell.contains(id)))
            {
                let cell = group.remove(i);
                group.insert(0, cell);
            }
        }
        first.extend(rest);
        if first.is_empty() { Err(NotFound::VersionReadsNone) } else { Ok(first) }
    }
}
