//! A pair's rows as they read now: each row's live paragraphs (with the
//! version's unlisted lines placed by D3), its status and whether Claude
//! should work on it. Rules: `docs/plans/versions.md`, "Status (derived)"
//! and "D3".
//!
//! The original (the member Scriptorium didn't make) is read from its file
//! (C8), so its ids are the links file's ids. The version is its live
//! document: the ids it came from its file with, plus fresh ids for lines
//! added since, which the links file doesn't list (D3).
//!
//! The caller checks the hashes first (C6): `lay_out` trusts that the links
//! file describes the bytes both scripts came from.

use std::collections::{HashMap, HashSet};

use scriptorium_core::{ElementKind, ParaId, Paragraph, Script};

use crate::links::{Links, Stamp, stamp_text};

/// What a row reads now. "This" is always the version (the made member),
/// "other" the original, whichever script the rows are shown on: a host
/// showing rows on the original (`read_rows` called on the English) swaps
/// `ThisChanged` and `OtherChanged` for its reader and shows no row as
/// needing work (only the version is written).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    /// Both sides as they were when last in sync (or a decided suggestion
    /// says so and the original still reads what it was made for).
    InSync,
    /// The version changed since the row was last in sync.
    ThisChanged,
    /// The original changed since.
    OtherChanged,
    BothChanged,
    /// The version's side is blank (or still its placeholder text): not yet
    /// translated. Only ever the version's side.
    Blank,
    /// One side has no live paragraphs.
    OneSided,
}

/// The state of a suggestion placed on a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacedState {
    Pending,
    Accepted,
    Rewritten,
    /// Rejected: the version's line was kept as it was.
    Kept,
}

/// A `ROW:` suggestion of this pair as review placed it (the host builds
/// these from review's items, so this crate needn't know review).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    /// The version's paragraphs it was placed on (live ids).
    pub cell: Vec<ParaId>,
    pub state: PlacedState,
    /// Its `ROW TEXT:` lines: the original's side when it was made.
    pub row_text: Vec<String>,
}

/// Why a row is shown with a note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Note {
    /// A kind Scriptorium doesn't edit ("End of Act", "Lyrics"). Never needs.
    NotEditable,
    /// The links file lists nothing on the version's side of the row: a
    /// line new in the original (C4, re-find), or a version line the writer
    /// deleted or joined on purpose (after a write). Phase A can't add one.
    NothingOnVersionSide,
}

impl Note {
    pub fn text(&self) -> &'static str {
        match self {
            Note::NotEditable => "can't be translated here; change it in Final Draft",
            Note::NothingOnVersionSide => {
                "nothing on this side: if the line is new in the original, add an empty line of this type in Final Draft at this point, then reopen; if you removed it on purpose, leave it"
            }
        }
    }
}

/// One row as it reads now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaidRow {
    /// Per member (the links file's order): the live paragraphs, in script
    /// order. The version's cell includes its unlisted lines (D3).
    pub cells: Vec<Vec<ParaId>>,
    pub status: RowStatus,
    /// Claude should work on it: blank or changed in the original,
    /// editable, and no suggestion placed on it (of any state) whose
    /// `ROW TEXT` reads the original now.
    pub needs: bool,
    pub note: Option<Note>,
    /// Indices into `placed` of the suggestions placed on this row.
    pub suggestions: Vec<usize>,
}

/// Which member is the version (the one `created:` marks; `b` if none is
/// marked) and which the original: [`Links::sides`].
pub fn sides(links: &Links) -> (usize, usize) {
    links.sides()
}

/// Every row's live paragraphs per member: the listed ids still in each
/// script, and the version's unlisted paragraphs placed by D3. Indexed
/// `[row][member]`; members other than the two are left empty.
pub(crate) fn live_cells(links: &Links, original: &Script, version: &Script) -> Vec<Vec<Vec<ParaId>>> {
    let (o, v) = sides(links);
    let n = links.members.len();
    let mut out: Vec<Vec<Vec<ParaId>>> = vec![vec![Vec::new(); n]; links.rows.len()];
    if links.rows.is_empty() {
        return out;
    }
    let in_original: HashSet<ParaId> = original.paragraphs.iter().map(|p| p.id).collect();
    for (r, row) in links.rows.iter().enumerate() {
        out[r][o] = row.cells[o].iter().copied().filter(|id| in_original.contains(id)).collect();
    }

    let row_of: HashMap<ParaId, usize> =
        links.rows.iter().enumerate().flat_map(|(r, row)| row.cells[v].iter().map(move |&id| (id, r))).collect();
    let paras = &version.paragraphs;
    // For each position: the row of the next listed paragraph after it.
    let mut next_row = vec![None; paras.len()];
    let mut next = None;
    for i in (0..paras.len()).rev() {
        next_row[i] = next;
        if let Some(&r) = row_of.get(&paras[i].id) {
            next = Some(r);
        }
    }
    // D3, walking the version in order. `cur` is the row of the nearest
    // placed paragraph above (listed, or unlisted and placed already), so a
    // run of unlisted lines keeps its order across rows.
    let mut cur: Option<usize> = None;
    for (i, p) in paras.iter().enumerate() {
        if let Some(&r) = row_of.get(&p.id) {
            out[r][v].push(p.id);
            cur = Some(r);
            continue;
        }
        let start = cur.map_or(0, |r| r + 1);
        let end = next_row[i].unwrap_or(links.rows.len());
        let empty = (start..end.max(start)).find(|&r| out[r][v].is_empty());
        let Some(target) = empty.or(cur).or(next_row[i]) else { continue };
        out[target][v].push(p.id);
        cur = Some(target);
    }
    out
}

/// The pair's rows as they read now. `original` is the original's file
/// script; `version` the version's live script; `placed` the pair's `ROW:`
/// suggestions as review placed them; `is_caps` whether a kind is shown in
/// capitals (one function for both members for now: the version's file
/// was copied from the original's, so it has the same caps styles).
pub fn lay_out(
    links: &Links,
    original: &Script,
    version: &Script,
    placed: &[Placed],
    is_caps: &dyn Fn(&ElementKind) -> bool,
) -> Vec<LaidRow> {
    let (o, v) = sides(links);
    let cells = live_cells(links, original, version);
    let orig: HashMap<ParaId, &Paragraph> = original.paragraphs.iter().map(|p| (p.id, p)).collect();
    let ver: HashMap<ParaId, &Paragraph> = version.paragraphs.iter().map(|p| (p.id, p)).collect();

    let row_of_ver: HashMap<ParaId, usize> =
        cells.iter().enumerate().flat_map(|(r, c)| c[v].iter().map(move |&id| (id, r))).collect();
    let mut on_row: Vec<Vec<usize>> = vec![Vec::new(); links.rows.len()];
    for (n, p) in placed.iter().enumerate() {
        if let Some(&r) = p.cell.iter().find_map(|id| row_of_ver.get(id)) {
            on_row[r].push(n);
        }
    }

    cells
        .into_iter()
        .enumerate()
        .map(|(r, cells)| {
            let listed = &links.rows[r].cells;
            let side = |member: usize, by_id: &HashMap<ParaId, &Paragraph>| -> Side {
                let paras: Vec<Line> = cells[member]
                    .iter()
                    .map(|id| {
                        let p = by_id[id];
                        let caps = is_caps(&p.kind);
                        let text = p.text();
                        Line {
                            blank: text.trim().is_empty(),
                            now: stamp_text(&text, caps),
                            caps,
                            stamp: links.paras[member].get(id).and_then(|s| s.stamp.clone()),
                            editable: p.kind.is_editable(),
                        }
                    })
                    .collect();
                // A listed paragraph that's gone changes the side.
                let all_live = cells[member].iter().filter(|id| links.paras[member].contains_key(id)).count()
                    == listed[member].len();
                Side { lines: paras, all_live }
            };
            let o_side = side(o, &orig);
            let v_side = side(v, &ver);
            let row_placed: Vec<&Placed> = on_row[r].iter().map(|&n| &placed[n]).collect();
            let status = status(&o_side, &v_side, &row_placed);

            let editable = o_side.lines.iter().chain(&v_side.lines).all(|l| l.editable)
                && listed
                    .iter()
                    .enumerate()
                    .all(|(m, ids)| ids.iter().all(|id| links.paras[m].get(id).is_none_or(|p| p.kind.is_editable())));
            let note = if !editable {
                Some(Note::NotEditable)
            } else if listed[v].is_empty() && v_side.lines.is_empty() && !o_side.lines.is_empty() {
                Some(Note::NothingOnVersionSide)
            } else {
                None
            };
            let needs = matches!(status, RowStatus::Blank | RowStatus::OtherChanged | RowStatus::BothChanged)
                && !row_placed.iter().any(|p| o_side.reads(&p.row_text))
                && editable;
            LaidRow { cells, status, needs, note, suggestions: on_row[r].clone() }
        })
        .collect()
}

/// One live paragraph of a row, as the status rules read it.
struct Line {
    blank: bool,
    /// `stamp_text` of its text now.
    now: String,
    caps: bool,
    /// Its stamp in the links file (`None`: unlisted, or never in sync).
    stamp: Option<Stamp>,
    editable: bool,
}

struct Side {
    lines: Vec<Line>,
    /// Every paragraph the links file lists on this side is still there.
    all_live: bool,
}

impl Side {
    /// Every paragraph reads its `=` stamp (`synced`) or its `~` stamp, and
    /// none the links file lists is gone.
    fn reads_stamps(&self, synced: bool) -> bool {
        self.all_live
            && self.lines.iter().all(|l| match (&l.stamp, synced) {
                (Some(Stamp::Synced(t)), true) | (Some(Stamp::Placeholder(t)), false) => *t == l.now,
                _ => false,
            })
    }

    /// Whether `texts` (a `ROW TEXT`) reads what this side reads now.
    fn reads(&self, texts: &[String]) -> bool {
        texts.len() == self.lines.len() && texts.iter().zip(&self.lines).all(|(t, l)| stamp_text(t, l.caps) == l.now)
    }
}

/// The status rules, in order.
fn status(o: &Side, v: &Side, placed: &[&Placed]) -> RowStatus {
    // 1.
    if o.lines.is_empty() || v.lines.is_empty() {
        return RowStatus::OneSided;
    }
    // 2. Both sides blank: nothing to translate.
    let v_blank = v.lines.iter().all(|l| l.blank);
    let o_blank = o.lines.iter().all(|l| l.blank);
    if v_blank && o_blank {
        return RowStatus::InSync;
    }
    if v_blank {
        return RowStatus::Blank;
    }
    // 3. Decided suggestions: in sync while one's `ROW TEXT` reads the
    // original now. If none does and the original moved off its `=`
    // stamps too, the original changed; otherwise the stamps decide.
    let decided: Vec<&&Placed> = placed
        .iter()
        .filter(|p| matches!(p.state, PlacedState::Accepted | PlacedState::Rewritten | PlacedState::Kept))
        .collect();
    if decided.iter().any(|p| o.reads(&p.row_text)) {
        return RowStatus::InSync;
    }
    if !decided.is_empty() && !o.reads_stamps(true) {
        return RowStatus::OtherChanged;
    }
    // 4.
    if v.reads_stamps(false) {
        return RowStatus::Blank;
    }
    // 5.
    match (!v.reads_stamps(true), !o.reads_stamps(true)) {
        (false, false) => RowStatus::InSync,
        (true, false) => RowStatus::ThisChanged,
        (false, true) => RowStatus::OtherChanged,
        (true, true) => RowStatus::BothChanged,
    }
}
