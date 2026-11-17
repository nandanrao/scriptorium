//! The links file after a write in place (`versions::written::after_write`):
//! `docs/plans/versions.md`, step A7a.

use scriptorium_core::{Document, ElementKind, ParaId, Paragraph, Pos, Script, Selection};
use scriptorium_versions::links::{parse, validate, write};
use scriptorium_versions::{Links, Placed, PlacedState, Row, RowStatus, Stamp, after_write, lay_out};

use ElementKind::*;

fn caps(k: &ElementKind) -> bool {
    matches!(k, SceneHeading | Character | Transition)
}

fn script(paras: &[(u64, ElementKind, &str)]) -> Script {
    let paragraphs = paras.iter().map(|(id, k, t)| Paragraph::plain(ParaId(*id), k.clone(), *t)).collect();
    Script { paragraphs, ..Script::default() }
}

fn links(rows: &str) -> Links {
    let text = format!(
        "SCRIPTORIUM LINKS v1\nId: lk-test\na: X.fdx\na language: en\na hash: 1111\nb: X.es.fdx\nb language: es\nb hash: 2222\nb created: version new\n---\n{rows}"
    );
    parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"))
}

fn ids(v: &[u64]) -> Vec<ParaId> {
    v.iter().map(|&n| ParaId(n)).collect()
}

fn row(a: &[u64], b: &[u64]) -> Row {
    Row { cells: vec![ids(a), ids(b)] }
}

/// `after_write`, checked to be a valid links file that writes and parses
/// back to itself.
fn after(l: &Links, a: &Script, b: &Script, placed: &[Placed]) -> Links {
    let out = after_write(l, a, b, placed, "3333", &caps);
    validate(&out).unwrap_or_else(|e| panic!("{e}\n{out:?}"));
    let text = write(&out).unwrap();
    assert_eq!(parse(&text).unwrap(), out);
    assert_eq!(out.members[1].hash, "3333");
    assert_eq!(out.members[0], l.members[0], "the original's member is untouched");
    out
}

fn stamp(l: &Links, member: usize, id: u64) -> Option<Stamp> {
    l.paras[member][&ParaId(id)].stamp.clone()
}

fn synced(t: &str) -> Option<Stamp> {
    Some(Stamp::Synced(t.to_string()))
}

#[test]
fn ids_become_positions_with_a_split_line() {
    let l = links(
        "16 | 17\n  a16 character = TOMÁS\n  b17 character = TOMÁS\n17 | 18\n  a17 dialogue = You're not getting on that boat.\n  b18 dialogue = No vas a subir a ese barco.\n",
    );
    let a = script(&[(16, Character, "TOMÁS"), (17, Dialogue, "You're not getting on that boat.")]);
    let b = script(&[(17, Character, "TOMÁS"), (18, Dialogue, "No vas a subir a ese barco.")]);
    let mut doc = Document::new(b);
    doc.enter(Selection::cursor(Pos::new(ParaId(18), 7))).unwrap();
    let b = doc.script().clone();
    assert_eq!(b.paragraphs.len(), 3);

    let out = after(&l, &a, &b, &[]);
    assert_eq!(out.rows, vec![row(&[16], &[0]), row(&[17], &[1, 2])]);
    assert_eq!(stamp(&out, 1, 0), synced("TOMÁS"), "in sync: re-stamped at its new position");
    // The split row reads ThisChanged: the head keeps its old stamp; the
    // tail was never in sync.
    assert_eq!(stamp(&out, 1, 1), synced("No vas a subir a ese barco."));
    assert_eq!(stamp(&out, 1, 2), None);
    assert_eq!(out.paras[1][&ParaId(2)].kind, Dialogue);
    assert_eq!(out.paras[0], l.paras[0]);
}

#[test]
fn in_sync_rows_get_current_text_blank_keeps_tilde_changed_keeps_old() {
    let l = links(concat!(
        "1 | 1\n  a1 heading = INT. FERRY - NIGHT\n  b1 heading = INT. FERRY - NIGHT\n",
        "2 | 2\n  a2 dialogue = Watch me.\n  b2 dialogue ~\n",
        "3 | 3\n  a3 dialogue = Go.\n  b3 dialogue = Ve.\n",
        "4 | 4\n  a4 dialogue = Stay.\n  b4 dialogue = Quedate.\n",
        "5 | 5\n  a5 dialogue = Run.\n  b5 dialogue ~\n",
    ));
    let a = script(&[
        (1, SceneHeading, "int. ferry - night"),
        (2, Dialogue, "Watch me."),
        (3, Dialogue, "Go now."),
        (4, Dialogue, "Stay."),
        (5, Dialogue, "Run."),
    ]);
    let b = script(&[
        (1, SceneHeading, "INT. FERRY - NIGHT"),
        (2, Dialogue, ""),
        (3, Dialogue, "Ve."),
        (4, Dialogue, "Quédate aquí."),
        // Accepted below: in sync through the decided suggestion.
        (5, Dialogue, "Corré."),
    ]);
    let placed = [Placed { cell: ids(&[5]), state: PlacedState::Accepted, row_text: vec!["Run.".into()] }];
    let st: Vec<RowStatus> = lay_out(&l, &a, &b, &placed, &caps).iter().map(|r| r.status).collect();
    assert_eq!(
        st,
        [RowStatus::InSync, RowStatus::Blank, RowStatus::OtherChanged, RowStatus::ThisChanged, RowStatus::InSync]
    );

    let out = after(&l, &a, &b, &placed);
    assert_eq!(out.rows, (1..=5).map(|n| row(&[n], &[n - 1])).collect::<Vec<_>>());
    // In sync: both sides stamped with what they read now (caps applied).
    assert_eq!(stamp(&out, 0, 1), synced("INT. FERRY - NIGHT"));
    assert_eq!(stamp(&out, 1, 0), synced("INT. FERRY - NIGHT"));
    assert_eq!(stamp(&out, 0, 5), synced("Run."));
    assert_eq!(stamp(&out, 1, 4), synced("Corré."));
    // Blank keeps `~`.
    assert_eq!(stamp(&out, 1, 1), Some(Stamp::Placeholder(String::new())));
    // Changed rows keep their old stamps on both sides.
    assert_eq!(stamp(&out, 0, 3), synced("Go."));
    assert_eq!(stamp(&out, 1, 2), synced("Ve."));
    assert_eq!(stamp(&out, 0, 4), synced("Stay."));
    assert_eq!(stamp(&out, 1, 3), synced("Quedate."));

    // Written again with nothing changed, the rows read the same.
    let again: Vec<RowStatus> = lay_out(&out, &a, &renumber(&b), &[], &caps).iter().map(|r| r.status).collect();
    assert_eq!(
        again,
        [RowStatus::InSync, RowStatus::Blank, RowStatus::OtherChanged, RowStatus::ThisChanged, RowStatus::InSync]
    );
}

/// The written file as it parses: ids are positions.
fn renumber(s: &Script) -> Script {
    let mut s = s.clone();
    for (i, p) in s.paragraphs.iter_mut().enumerate() {
        p.id = ParaId(i as u64);
    }
    s
}

#[test]
fn rows_left_with_no_ids_are_dropped() {
    // `| 2` is the version's own line; deleted in the session, its row has
    // nothing left on either side.
    let l = links(concat!(
        "1 | 1\n  a1 dialogue = Go.\n  b1 dialogue = Ve.\n",
        "| 2\n  b2 action = Llueve.\n",
        "2 | 3\n  a2 dialogue = Stay.\n  b3 dialogue = Quedate.\n",
    ));
    let a = script(&[(1, Dialogue, "Go."), (2, Dialogue, "Stay.")]);
    let b = script(&[(1, Dialogue, "Ve."), (3, Dialogue, "Quedate.")]);
    let out = after(&l, &a, &b, &[]);
    assert_eq!(out.rows, vec![row(&[1], &[0]), row(&[2], &[1])]);
}

#[test]
fn a_new_line_fills_a_row_new_in_the_original() {
    // D3 example 2: rows `18 | 20`, `19 |`, `20 | 21`; an empty Dialogue
    // added after b20 joins `19 |`, which now lists it.
    let l = links(
        "18 | 20\n  a18 dialogue = Watch me.\n  b20 dialogue = Mirá.\n19 |\n  a19 dialogue = Now.\n20 | 21\n  a20 action = She goes.\n  b21 action = Se va.\n",
    );
    let a = script(&[(18, Dialogue, "Watch me."), (19, Dialogue, "Now."), (20, Action, "She goes.")]);
    let b = script(&[(20, Dialogue, "Mirá."), (22, Dialogue, ""), (21, Action, "Se va.")]);
    let out = after(&l, &a, &b, &[]);
    assert_eq!(out.rows, vec![row(&[18], &[0]), row(&[19], &[1]), row(&[20], &[2])]);
    assert_eq!(stamp(&out, 1, 1), None, "Blank, never in sync");
    assert_eq!(stamp(&out, 0, 19), synced("Now."));
}

#[test]
fn a_deleted_line_leaves_a_one_sided_row_and_a_new_kind_is_kept() {
    let l = links(concat!(
        "1 | 1\n  a1 dialogue = Go.\n  b1 dialogue = Ve.\n",
        "2 | 2\n  a2 dialogue = Stay.\n  b2 dialogue = Quedate.\n",
    ));
    let a = script(&[(1, Dialogue, "Go."), (2, Dialogue, "Stay.")]);
    // b2 deleted; b1 retyped as Action.
    let b = script(&[(1, Action, "Ve.")]);
    let out = after(&l, &a, &b, &[]);
    assert_eq!(out.rows, vec![row(&[1], &[0]), row(&[2], &[])]);
    assert_eq!(out.paras[1][&ParaId(0)].kind, Action);
    assert_eq!(out.paras[0], l.paras[0], "one-sided: the original keeps its stamps");
}

fn statuses(l: &Links, a: &Script, b: &Script, placed: &[Placed]) -> Vec<RowStatus> {
    lay_out(l, a, b, placed, &caps).iter().map(|r| r.status).collect()
}

#[test]
fn a_deleted_version_line_never_makes_a_row_read_in_sync_or_blank() {
    // `1 | 1 2` both `=`, b2 deleted; `2 | 3 4` both `~`, b4 deleted.
    let l = links(concat!(
        "1 | 1 2\n  a1 action = Rain.\n  b1 action = Lluvia.\n  b2 action = Mucha.\n",
        "2 | 3 4\n  a2 action = Wind.\n  b3 action ~ Wind.\n  b4 action ~ Cold.\n",
    ));
    let a = script(&[(1, Action, "Rain."), (2, Action, "Wind.")]);
    let b = script(&[(1, Action, "Lluvia."), (3, Action, "Wind.")]);
    let before = statuses(&l, &a, &b, &[]);
    assert_eq!(before, [RowStatus::ThisChanged, RowStatus::ThisChanged]);
    let out = after(&l, &a, &b, &[]);
    assert_eq!(out.rows, vec![row(&[1], &[0]), row(&[2], &[1])]);
    assert_eq!(stamp(&out, 1, 0), None);
    assert_eq!(stamp(&out, 1, 1), None);
    assert_eq!(statuses(&out, &a, &renumber(&b), &[]), before);
}

#[test]
fn an_unlisted_line_in_an_in_sync_row_gets_its_stamp() {
    // b9 was added (unlisted) and D3 put it in the empty-version row `2 |`;
    // a decided suggestion there makes the row in sync.
    let l = links(concat!("1 | 1\n  a1 dialogue = Go.\n  b1 dialogue = Ve.\n", "2 |\n  a2 dialogue = Now.\n",));
    let a = script(&[(1, Dialogue, "Go."), (2, Dialogue, "Now.")]);
    let b = script(&[(1, Dialogue, "Ve."), (9, Dialogue, "Ya.")]);
    let placed = [Placed { cell: ids(&[9]), state: PlacedState::Accepted, row_text: vec!["Now.".into()] }];
    assert_eq!(statuses(&l, &a, &b, &placed), [RowStatus::InSync, RowStatus::InSync]);
    let out = after(&l, &a, &b, &placed);
    assert_eq!(out.rows, vec![row(&[1], &[0]), row(&[2], &[1])]);
    assert_eq!(stamp(&out, 1, 1), synced("Ya."));
    assert_eq!(stamp(&out, 0, 2), synced("Now."));
    // In sync from the stamps alone now (the review file isn't needed).
    assert_eq!(statuses(&out, &a, &renumber(&b), &[]), [RowStatus::InSync, RowStatus::InSync]);
}

#[test]
fn a_split_blank_line_keeps_its_tilde_and_the_tail_has_none() {
    let l = links("1 | 1\n  a1 dialogue = Watch me.\n  b1 dialogue ~\n");
    let a = script(&[(1, Dialogue, "Watch me.")]);
    let mut doc = Document::new(script(&[(1, Dialogue, "")]));
    doc.enter(Selection::cursor(Pos::new(ParaId(1), 0))).unwrap();
    let b = doc.script().clone();
    assert_eq!(b.paragraphs.len(), 2);
    assert_eq!(statuses(&l, &a, &b, &[]), [RowStatus::Blank]);
    let out = after(&l, &a, &b, &[]);
    assert_eq!(out.rows, vec![row(&[1], &[0, 1])]);
    assert_eq!(stamp(&out, 1, 0), Some(Stamp::Placeholder(String::new())));
    assert_eq!(stamp(&out, 1, 1), None);
    assert_eq!(statuses(&out, &a, &renumber(&b), &[]), [RowStatus::Blank]);
}

#[test]
fn a_pending_suggestion_on_a_blank_row_stamps_nothing() {
    let l = links("1 | 1\n  a1 dialogue = Watch me.\n  b1 dialogue ~\n");
    let a = script(&[(1, Dialogue, "Watch me.")]);
    let b = script(&[(1, Dialogue, "")]);
    let placed = [Placed { cell: ids(&[1]), state: PlacedState::Pending, row_text: vec!["Watch me.".into()] }];
    assert_eq!(statuses(&l, &a, &b, &placed), [RowStatus::Blank]);
    let out = after(&l, &a, &b, &placed);
    assert_eq!(stamp(&out, 1, 0), Some(Stamp::Placeholder(String::new())));
    assert_eq!(stamp(&out, 0, 1), synced("Watch me."));
}
