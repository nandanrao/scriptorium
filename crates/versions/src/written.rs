//! The links file after the version (the made member) is written in place:
//! `docs/plans/versions.md`, "Writing a version in place", step 5.
//!
//! Paragraph *i* of the written file is the version's live paragraph *i*,
//! so the version's ids become positions. Its unlisted lines (split off,
//! typed in) are listed in the rows D3 put them in. Rows that read
//! `InSync` get `=` stamps with their current texts on both sides; every
//! other row keeps its stamps (a `Blank` row its `~`, a changed row its old
//! `=`), except that a row which lost a listed version line loses the
//! stamps of its remaining version lines too: the stamps describe the old
//! cell, and once the gone line is no longer listed they could otherwise
//! read as in sync (or as untouched placeholders). The original isn't
//! written, so its ids and the rows' original side stay as they are. Rows
//! left with no ids on every side are dropped.
//!
//! A row in sync only through a decided suggestion (status rule 3) is
//! stamped too, so it stays in sync even if the review file is lost later.
//! Undeciding that suggestion after the write changes only the row's status
//! label (the blank comes back: "not yet translated"), never `needs`: the
//! suggestion, pending again, still reads the original, so the row doesn't
//! need a new one.

use std::collections::{BTreeMap, HashMap};

use scriptorium_core::{ElementKind, ParaId, Paragraph, Script};

use crate::links::{Links, Para, Row, Stamp, stamp_text};
use crate::rows::{Placed, RowStatus, lay_out, sides};

/// The links file for the version's new bytes. `links` describes the bytes
/// both scripts came from (the caller checked the hashes, C6); `original`
/// is the original's file script; `version` the version's live script, the
/// one just written; `placed` the pair's `ROW:` suggestions as review
/// placed them (live ids), as for [`lay_out`]; `hash` the written file's
/// `sessionfile::hash`.
pub fn after_write(
    links: &Links,
    original: &Script,
    version: &Script,
    placed: &[Placed],
    hash: &str,
    is_caps: &dyn Fn(&ElementKind) -> bool,
) -> Links {
    let (o, v) = sides(links);
    let laid = lay_out(links, original, version, placed, is_caps);

    // Live id -> position in the written file, and the live paragraphs.
    let position: HashMap<ParaId, ParaId> =
        version.paragraphs.iter().enumerate().map(|(i, p)| (p.id, ParaId(i as u64))).collect();
    let live: HashMap<ParaId, &Paragraph> = version.paragraphs.iter().map(|p| (p.id, p)).collect();
    let orig: HashMap<ParaId, &Paragraph> = original.paragraphs.iter().map(|p| (p.id, p)).collect();
    let synced = |p: &Paragraph| Stamp::Synced(stamp_text(&p.text(), is_caps(&p.kind)));

    let mut out = links.clone();
    out.members[v].hash = hash.to_string();
    let mut v_paras: BTreeMap<ParaId, Para> = BTreeMap::new();
    let mut rows = Vec::with_capacity(laid.len());
    for (r, row) in laid.iter().enumerate() {
        let in_sync = row.status == RowStatus::InSync;
        // A listed version line is gone: the old stamps no longer describe
        // this cell (see the module doc).
        let lost = links.rows[r].cells[v].iter().any(|id| !live.contains_key(id));
        let mut cells = links.rows[r].cells.clone();
        cells[v] = Vec::with_capacity(row.cells[v].len());
        for id in &row.cells[v] {
            let (Some(&new_id), Some(p)) = (position.get(id), live.get(id)) else { continue };
            let stamp = if in_sync {
                Some(synced(p))
            } else if lost {
                None
            } else {
                links.paras[v].get(id).and_then(|old| old.stamp.clone())
            };
            v_paras.insert(new_id, Para { kind: p.kind.clone(), stamp });
            cells[v].push(new_id);
        }
        if in_sync {
            for id in &row.cells[o] {
                if let (Some(p), Some(para)) = (orig.get(id), out.paras[o].get_mut(id)) {
                    para.stamp = Some(synced(p));
                }
            }
        }
        if cells.iter().any(|c| !c.is_empty()) {
            rows.push(Row { cells });
        }
    }
    out.rows = rows;
    out.paras[v] = v_paras;
    out
}
