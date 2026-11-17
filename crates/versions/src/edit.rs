//! Changing a links file by hand, row by row (plan step B3): marking a row
//! in sync or not, and joining a row a merge left with nothing on one side
//! with a neighbour. The app (Edit › Mark in sync / Mark not in sync) and
//! the CLI (`scriptorium links <script> sync|unsync|join <row>`) call these
//! through `scriptorium_fsio::versions::edit_row`, which checks the hashes
//! (C6) and writes the result under the links lock.
//!
//! None of these is an undo step: the links file isn't part of a session.
//! Rows are indexed from 0 here; hosts number them from 1, as `rows` does.

use std::fmt;

use scriptorium_core::{ElementKind, ParaId, Script};

use crate::links::{Links, Stamp, stamp_text};

/// Why a row can't be changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// No such row (0-based index, and how many rows there are).
    NoRow { row: usize, rows: usize },
    /// The row's live paragraphs aren't the ones the links file lists (a
    /// line split off, typed in or removed since the version was written):
    /// marking it would name a session-only id, or stamp a line that's gone.
    Unlisted,
    /// One side of the row has no paragraphs: there's nothing to mark.
    OneSided,
    /// `join` on a row with paragraphs on both sides.
    NotOneSided,
    /// `join` without a direction, and both neighbours (0-based) have lines
    /// on the side the row lacks: which one is the writer's call.
    Ambiguous { above: usize, below: usize },
    /// `join` with no neighbour that has lines on the side the row lacks
    /// (the rows that are there, 0-based).
    NoNeighbour { above: Option<usize>, below: Option<usize> },
    /// `join` in a direction with no row there.
    NoRowThere(Toward),
    /// `join` in a direction whose row (0-based) lacks the same side: the
    /// joined row would still have nothing there.
    StillOneSided(usize),
}

/// Which neighbour `join` joins a row with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toward {
    Up,
    Down,
}

impl fmt::Display for Toward {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Toward::Up => "above",
            Toward::Down => "below",
        })
    }
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::NoRow { row, rows } => write!(f, "there is no row {} (the pair has {rows})", row + 1),
            EditError::Unlisted => f.write_str(
                "it has lines the links file doesn't list yet (split off, typed in or removed here): Write .fdx \
                 first, then mark it",
            ),
            EditError::OneSided => f.write_str("it has nothing on one side, so there's nothing to mark"),
            EditError::NotOneSided => {
                f.write_str("it has lines on both sides; join is for a row left with nothing on one side")
            }
            EditError::Ambiguous { above, below } => {
                write!(f, "it could join row {} above or row {} below; say which (up or down)", above + 1, below + 1)
            }
            EditError::NoNeighbour { above, below } => {
                let rows: Vec<String> =
                    [above.map(|r| format!("row {} above", r + 1)), below.map(|r| format!("row {} below", r + 1))]
                        .into_iter()
                        .flatten()
                        .collect();
                if rows.is_empty() {
                    f.write_str("it is the only row")
                } else {
                    let has = if rows.len() > 1 { "have" } else { "has" };
                    write!(f, "{} {has} nothing on the side it lacks either", rows.join(" and "))
                }
            }
            EditError::NoRowThere(t) => write!(f, "there is no row {t} it"),
            EditError::StillOneSided(r) => {
                write!(
                    f,
                    "row {} has nothing on the side it lacks either, so the joined row would still be one-sided",
                    r + 1
                )
            }
        }
    }
}

impl std::error::Error for EditError {}

fn check_row(links: &Links, row: usize) -> Result<(), EditError> {
    if row >= links.rows.len() {
        return Err(EditError::NoRow { row, rows: links.rows.len() });
    }
    Ok(())
}

/// Mark row `row` in sync: `=` stamps with each paragraph's text now, on
/// both sides. `live` is the row's live paragraphs per member as laid out
/// (`LaidRow::cells`); `scripts` each member's script as the rows read it
/// (the version's live script, the original's file, C8). Refused when the
/// live paragraphs aren't exactly the listed ones ([`EditError::Unlisted`]),
/// so the links file never names a session-only id, and for a row with
/// nothing on one side. Kinds stay as listed: they describe the files.
pub fn sync_row(
    links: &Links,
    row: usize,
    live: &[Vec<ParaId>],
    scripts: &[&Script],
    is_caps: &dyn Fn(&ElementKind) -> bool,
) -> Result<Links, EditError> {
    check_row(links, row)?;
    if live != links.rows[row].cells.as_slice() {
        return Err(EditError::Unlisted);
    }
    if live.iter().any(Vec::is_empty) {
        return Err(EditError::OneSided);
    }
    let mut out = links.clone();
    for (m, ids) in live.iter().enumerate() {
        for id in ids {
            let p = scripts[m].paragraphs.iter().find(|p| p.id == *id).ok_or(EditError::Unlisted)?;
            let para = out.paras[m].get_mut(id).ok_or(EditError::Unlisted)?;
            para.stamp = Some(Stamp::Synced(stamp_text(&p.text(), is_caps(&p.kind))));
        }
    }
    Ok(out)
}

/// Take row `row`'s mark back: the original's `=` stamps go, so the row
/// reads "the original changed" (and needs a suggestion again) until it is
/// marked in sync or a suggestion on it is decided. The version's stamps
/// stay as they are; a placeholder (`~`) that Mark in sync replaced isn't
/// put back, but a blank version line reads "not yet translated" whatever
/// its stamp (status rule 2). The row may still read in sync through a
/// decided suggestion, or when both sides are blank (rules 2 and 3).
pub fn unsync_row(links: &Links, row: usize) -> Result<Links, EditError> {
    check_row(links, row)?;
    let (o, _) = links.sides();
    let mut out = links.clone();
    for id in &links.rows[row].cells[o] {
        if let Some(para) = out.paras[o].get_mut(id)
            && matches!(para.stamp, Some(Stamp::Synced(_)))
        {
            para.stamp = None;
        }
    }
    Ok(out)
}

/// Join row `row`, which lists nothing on one side (a merge left it so),
/// with a neighbour: `toward` it, or else the one neighbour that has lines
/// on the side `row` lacks (refused when both do, or neither). Refused when
/// the joined row would still have nothing on a side. Returns the new links
/// and the index of the joined row. Rows stay in order (they are
/// neighbours, so nothing crosses); every stamp stays as it was, so the
/// joined row reads changed where its lines moved on, and Mark in sync
/// confirms it.
///
/// Backspace at the start of a translated line under a blank one keeps the
/// lower line's id, so the row *above* is left one-sided and joins down.
pub fn join_rows(links: &Links, row: usize, toward: Option<Toward>) -> Result<(Links, usize), EditError> {
    check_row(links, row)?;
    let lacks: Vec<usize> = (0..links.members.len()).filter(|&m| links.rows[row].cells[m].is_empty()).collect();
    if lacks.is_empty() {
        return Err(EditError::NotOneSided);
    }
    let above = row.checked_sub(1);
    let below = Some(row + 1).filter(|&r| r < links.rows.len());
    let fills = |r: usize| lacks.iter().all(|&m| !links.rows[r].cells[m].is_empty());
    let other = match toward {
        Some(t) => {
            let r = match t {
                Toward::Up => above,
                Toward::Down => below,
            }
            .ok_or(EditError::NoRowThere(t))?;
            if !fills(r) {
                return Err(EditError::StillOneSided(r));
            }
            r
        }
        None => match (above.filter(|&r| fills(r)), below.filter(|&r| fills(r))) {
            (Some(a), Some(b)) => return Err(EditError::Ambiguous { above: a, below: b }),
            (Some(r), None) | (None, Some(r)) => r,
            (None, None) => return Err(EditError::NoNeighbour { above, below }),
        },
    };
    let (first, second) = (row.min(other), row.max(other));
    let mut out = links.clone();
    let taken = out.rows.remove(second);
    for (cell, more) in out.rows[first].cells.iter_mut().zip(taken.cells) {
        cell.extend(more);
        cell.sort();
    }
    Ok((out, first))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::links::{Member, Para, Row, parse, write};
    use scriptorium_core::Paragraph;
    use std::collections::BTreeMap;

    fn para(id: u64, kind: ElementKind, text: &str) -> Paragraph {
        Paragraph::plain(ParaId(id), kind, text)
    }

    /// Three rows: a heading (in sync), an action blank in the version
    /// (`~`), and a dialogue row with nothing on the version side.
    fn links() -> Links {
        let member = |file: &str, lang: &str, created: bool| Member {
            file: file.into(),
            language: Some(lang.into()),
            hash: "h".into(),
            created: created.then(|| "version new".into()),
        };
        let mut a = BTreeMap::new();
        let mut b = BTreeMap::new();
        let synced = |t: &str| Some(Stamp::Synced(t.into()));
        a.insert(ParaId(0), Para { kind: ElementKind::SceneHeading, stamp: synced("INT. A - DAY") });
        a.insert(ParaId(1), Para { kind: ElementKind::Action, stamp: synced("Rain.") });
        a.insert(ParaId(2), Para { kind: ElementKind::Dialogue, stamp: synced("Go.") });
        b.insert(ParaId(0), Para { kind: ElementKind::SceneHeading, stamp: synced("INT. A - DAY") });
        b.insert(ParaId(1), Para { kind: ElementKind::Action, stamp: Some(Stamp::Placeholder(String::new())) });
        let row = |a: &[u64], b: &[u64]| Row {
            cells: vec![a.iter().map(|&i| ParaId(i)).collect(), b.iter().map(|&i| ParaId(i)).collect()],
        };
        Links {
            id: "lk-test".into(),
            members: vec![member("film.fdx", "en", false), member("film.es.fdx", "es", true)],
            rows: vec![row(&[0], &[0]), row(&[1], &[1]), row(&[2], &[])],
            paras: vec![a, b],
            extra_headers: vec![],
        }
    }

    fn scripts() -> (Script, Script) {
        let en = Script {
            paragraphs: vec![
                para(0, ElementKind::SceneHeading, "INT. A - DAY"),
                para(1, ElementKind::Action, "Rain hammers  the glass."),
                para(2, ElementKind::Dialogue, "Go."),
            ],
            ..Script::default()
        };
        let es = Script {
            paragraphs: vec![
                para(0, ElementKind::SceneHeading, "INT. A - DÍA"),
                para(1, ElementKind::Action, "La lluvia golpea el cristal."),
            ],
            ..Script::default()
        };
        (en, es)
    }

    fn caps(k: &ElementKind) -> bool {
        *k == ElementKind::SceneHeading
    }

    fn ids(v: &[u64]) -> Vec<ParaId> {
        v.iter().map(|&i| ParaId(i)).collect()
    }

    #[test]
    fn marking_in_sync_stamps_both_sides_with_their_text_now() {
        let l = links();
        let (en, es) = scripts();
        let out = sync_row(&l, 1, &[ids(&[1]), ids(&[1])], &[&en, &es], &caps).unwrap();
        assert_eq!(out.paras[0][&ParaId(1)].stamp, Some(Stamp::Synced("Rain hammers the glass.".into())));
        assert_eq!(out.paras[1][&ParaId(1)].stamp, Some(Stamp::Synced("La lluvia golpea el cristal.".into())));
        // Caps kinds are stamped in capitals; other rows untouched.
        let out = sync_row(&out, 0, &[ids(&[0]), ids(&[0])], &[&en, &es], &caps).unwrap();
        assert_eq!(out.paras[1][&ParaId(0)].stamp, Some(Stamp::Synced("INT. A - DÍA".into())));
        assert_eq!(out.paras[0][&ParaId(2)], l.paras[0][&ParaId(2)]);
        // Written and read back the same.
        assert_eq!(parse(&write(&out).unwrap()).unwrap(), out);
    }

    #[test]
    fn marking_is_refused_for_unlisted_lines_and_one_sided_rows() {
        let l = links();
        let (en, es) = scripts();
        // A line split off in the session (fresh id 9) is in the row.
        assert_eq!(sync_row(&l, 1, &[ids(&[1]), ids(&[1, 9])], &[&en, &es], &caps), Err(EditError::Unlisted));
        // A listed line removed.
        assert_eq!(sync_row(&l, 1, &[ids(&[1]), ids(&[])], &[&en, &es], &caps), Err(EditError::Unlisted));
        assert_eq!(sync_row(&l, 2, &[ids(&[2]), ids(&[])], &[&en, &es], &caps), Err(EditError::OneSided));
        assert_eq!(sync_row(&l, 3, &[], &[&en, &es], &caps), Err(EditError::NoRow { row: 3, rows: 3 }));
        assert_eq!(EditError::NoRow { row: 3, rows: 3 }.to_string(), "there is no row 4 (the pair has 3)");
    }

    #[test]
    fn not_in_sync_drops_the_originals_stamps() {
        let l = links();
        let out = unsync_row(&l, 0).unwrap();
        assert_eq!(out.paras[0][&ParaId(0)].stamp, None);
        assert_eq!(out.paras[1][&ParaId(0)], l.paras[1][&ParaId(0)], "the version's stay");
        let out = unsync_row(&l, 1).unwrap();
        assert_eq!(out.paras[0][&ParaId(1)].stamp, None);
        assert_eq!(out.paras[1][&ParaId(1)].stamp, Some(Stamp::Placeholder(String::new())));
        assert!(unsync_row(&l, 7).is_err());
    }

    #[test]
    fn join_puts_a_one_sided_row_into_the_neighbour_that_fills_it() {
        let l = links();
        // Row 3 lacks the version side; row 2 above fills it, nothing is below.
        let (out, into) = join_rows(&l, 2, None).unwrap();
        assert_eq!(into, 1);
        assert_eq!(out.rows.len(), 2);
        assert_eq!(out.rows[1].cells, vec![ids(&[1, 2]), ids(&[1])]);
        assert_eq!(out.paras, l.paras, "stamps stay");
        assert_eq!(parse(&write(&out).unwrap()).unwrap(), out);
        assert_eq!(join_rows(&l, 2, Some(Toward::Up)).unwrap(), (out, 1));
        assert_eq!(join_rows(&l, 2, Some(Toward::Down)), Err(EditError::NoRowThere(Toward::Down)));
        assert_eq!(join_rows(&l, 1, None), Err(EditError::NotOneSided));

        // Backspace at the start of a translated line under a blank one: the
        // upper row is left one-sided and joins the row below.
        let mut upper = l.clone();
        upper.rows = vec![
            Row { cells: vec![ids(&[0]), ids(&[0])] },
            Row { cells: vec![ids(&[1]), ids(&[])] },
            Row { cells: vec![ids(&[2]), ids(&[1])] },
        ];
        upper.paras[0].insert(ParaId(2), Para { kind: ElementKind::Action, stamp: None });
        let e = join_rows(&upper, 1, None).unwrap_err();
        assert_eq!(e, EditError::Ambiguous { above: 0, below: 2 });
        assert_eq!(e.to_string(), "it could join row 1 above or row 3 below; say which (up or down)");
        let (out, into) = join_rows(&upper, 1, Some(Toward::Down)).unwrap();
        assert_eq!(into, 1);
        assert_eq!(out.rows[1].cells, vec![ids(&[1, 2]), ids(&[1])]);

        // A neighbour lacking the same side: refused, either way.
        let mut both = upper.clone();
        both.rows[2].cells[1].clear();
        both.paras[1].remove(&ParaId(1));
        both.rows[0].cells[1].clear();
        both.paras[1].remove(&ParaId(0));
        both.rows[0].cells[0] = ids(&[0]);
        assert_eq!(join_rows(&both, 1, Some(Toward::Down)), Err(EditError::StillOneSided(2)));
        let e = join_rows(&both, 1, None).unwrap_err();
        assert_eq!(e, EditError::NoNeighbour { above: Some(0), below: Some(2) });
        assert!(e.to_string().starts_with("row 1 above and row 3 below have nothing"), "{e}");
        let mut alone = l.clone();
        alone.rows.truncate(1);
        alone.rows[0].cells[1].clear();
        assert_eq!(join_rows(&alone, 0, None).unwrap_err().to_string(), "it is the only row");
    }
}
