//! Status, D3 and the resolver (`versions::rows`, `versions::resolve`):
//! `docs/plans/versions.md`, step A2.

use scriptorium_core::{Document, ElementKind, ParaId, Paragraph, Pos, Script, Selection};
use scriptorium_versions::links::parse;
use scriptorium_versions::{
    LaidRow, Links, NotFound, Note, Placed, PlacedState, Resolver, RowQuery, RowStatus, lay_out,
};

use ElementKind::*;

fn caps(k: &ElementKind) -> bool {
    matches!(k, SceneHeading | Character | Transition)
}

fn script(paras: &[(u64, ElementKind, &str)]) -> Script {
    let paragraphs = paras.iter().map(|(id, k, t)| Paragraph::plain(ParaId(*id), k.clone(), *t)).collect();
    Script { paragraphs, ..Script::default() }
}

/// A links file (`b` made by `version new`) with the given rows section.
fn links(rows: &str) -> Links {
    let text = format!(
        "SCRIPTORIUM LINKS v1\nId: lk-test\na: X.fdx\na language: en\na hash: 1111\nb: X.es.fdx\nb language: es\nb hash: 2222\nb created: version new\n---\n{rows}"
    );
    parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"))
}

fn ids(v: &[u64]) -> Vec<ParaId> {
    v.iter().map(|&n| ParaId(n)).collect()
}

fn lay(l: &Links, a: &Script, b: &Script, placed: &[Placed]) -> Vec<LaidRow> {
    lay_out(l, a, b, placed, &caps)
}

fn statuses(rows: &[LaidRow]) -> Vec<RowStatus> {
    rows.iter().map(|r| r.status).collect()
}

fn placed(cell: &[u64], state: PlacedState, row_text: &[&str]) -> Placed {
    Placed { cell: ids(cell), state, row_text: row_text.iter().map(|s| s.to_string()).collect() }
}

/// One row, `a1 | b1`, both dialogue; the version reads `b` now.
fn one_row(a_stamp: &str, b_stamp: &str, a_now: &str, b_now: &str) -> (Links, Script, Script) {
    let l = links(&format!("1 | 1\n  a1 dialogue {a_stamp}\n  b1 dialogue {b_stamp}\n"));
    (l, script(&[(1, Dialogue, a_now)]), script(&[(1, Dialogue, b_now)]))
}

fn status_of(a_stamp: &str, b_stamp: &str, a_now: &str, b_now: &str, placed: &[Placed]) -> LaidRow {
    let (l, a, b) = one_row(a_stamp, b_stamp, a_now, b_now);
    lay(&l, &a, &b, placed).remove(0)
}

#[test]
fn every_status() {
    use RowStatus::*;
    let row = |a_st, b_st, a, b| status_of(a_st, b_st, a, b, &[]);
    assert_eq!(row("= Watch me.", "= Mirá.", "Watch  me.", "Mirá.").status, InSync);
    // Smart quotes and spacing aren't a change.
    assert_eq!(row("= It's", "= Es", "It\u{2019}s ", "Es").status, InSync);
    let r = row("= Watch me.", "= Mirá.", "Watch me.", "Mirá cómo.");
    assert_eq!((r.status, r.needs), (ThisChanged, false));
    let r = row("= Watch me.", "= Mirá.", "Watch me now.", "Mirá.");
    assert_eq!((r.status, r.needs), (OtherChanged, true));
    let r = row("= Watch me.", "= Mirá.", "Watch me now.", "Mirá cómo.");
    assert_eq!((r.status, r.needs), (BothChanged, true));
    let r = row("= Watch me.", "~", "Watch me.", "");
    assert_eq!((r.status, r.needs), (Blank, true));
    // Never in sync (no stamps): both sides count as changed.
    assert_eq!(row("", "", "Watch me.", "Mirá.").status, BothChanged);
    // Both blank and stamped so: in sync, nothing to do.
    let r = row("=", "=", "", "");
    assert_eq!((r.status, r.needs), (InSync, false));
}

#[test]
fn placeholder_heading_still_copied_reads_blank() {
    let l = links("1 | 1\n  a1 heading = INT. FERRY - NIGHT\n  b1 heading ~ INT. FERRY - NIGHT\n");
    let a = script(&[(1, SceneHeading, "INT. FERRY - NIGHT")]);
    let b = script(&[(1, SceneHeading, "int. ferry - night")]);
    let r = lay(&l, &a, &b, &[]).remove(0);
    assert_eq!((r.status, r.needs), (RowStatus::Blank, true));
    // Translated: changed on this side.
    let b = script(&[(1, SceneHeading, "INT. TERMINAL DE FERRIS - NOCHE")]);
    assert_eq!(lay(&l, &a, &b, &[])[0].status, RowStatus::ThisChanged);
}

#[test]
fn one_sided_rows() {
    let l = links(
        "1 | 1\n  a1 dialogue = Hi.\n  b1 dialogue = Hola.\n2 |\n  a2 dialogue = Watch me.\n | 2\n  b2 action = Llueve.\n",
    );
    let a = script(&[(1, Dialogue, "Hi."), (2, Dialogue, "Watch me.")]);
    let b = script(&[(1, Dialogue, "Hola."), (2, Action, "Llueve.")]);
    let rows = lay(&l, &a, &b, &[]);
    assert_eq!(statuses(&rows), [RowStatus::InSync, RowStatus::OneSided, RowStatus::OneSided]);
    assert!(rows.iter().all(|r| !r.needs));
    // C4: a row of the original with nothing on the version's side.
    assert_eq!(rows[1].note, Some(Note::NothingOnVersionSide));
    assert_eq!(rows[2].note, None);
}

#[test]
fn blank_is_never_the_originals() {
    // The original's side blank, the version's with text: not `Blank`.
    let r = status_of("~", "=", "", "Hola.", &[]);
    assert_eq!(r.status, RowStatus::BothChanged);
    let r = status_of("=", "=", "", "Hola.", &[]);
    assert_eq!(r.status, RowStatus::ThisChanged);
}

#[test]
fn a_listed_paragraph_gone_changes_its_side() {
    let l = links("1 | 1 2\n  a1 action = Rain.\n  b1 action = La lluvia.\n  b2 action = Golpea.\n");
    let a = script(&[(1, Action, "Rain.")]);
    let b = script(&[(1, Action, "La lluvia.")]);
    assert_eq!(lay(&l, &a, &b, &[])[0].status, RowStatus::ThisChanged);
}

#[test]
fn accepted_line_identical_to_the_original_reads_in_sync() {
    // D1: "MONTAGE" stays "MONTAGE". Without the decision it reads as the
    // placeholder (step 4); the decision comes first.
    let l = links("1 | 1\n  a1 heading = MONTAGE\n  b1 heading ~ MONTAGE\n");
    let a = script(&[(1, SceneHeading, "MONTAGE")]);
    let b = script(&[(1, SceneHeading, "MONTAGE")]);
    assert_eq!(lay(&l, &a, &b, &[])[0].status, RowStatus::Blank);
    let p = [placed(&[1], PlacedState::Accepted, &["MONTAGE"])];
    let r = lay(&l, &a, &b, &p).remove(0);
    assert_eq!((r.status, r.needs, r.suggestions.clone()), (RowStatus::InSync, false, vec![0]));
    // `ROW TEXT` in lower case still reads a heading in capitals.
    let p = [placed(&[1], PlacedState::Accepted, &["montage"])];
    assert_eq!(lay(&l, &a, &b, &p)[0].status, RowStatus::InSync);
}

#[test]
fn decided_suggestion_then_the_original_changes_then_undecide() {
    let accepted = [placed(&[1], PlacedState::Accepted, &["Watch me."])];
    let r = status_of("= Watch me.", "~", "Watch me.", "Mirá.", &accepted);
    assert_eq!(r.status, RowStatus::InSync);
    let r =
        status_of("= Watch me.", "~", "Watch  me.", "Mirá.", &[placed(&[1], PlacedState::Rewritten, &["Watch me."])]);
    assert_eq!(r.status, RowStatus::InSync);
    // The English changes after: the decision no longer counts, and the
    // row needs a new suggestion.
    let r = status_of("= Watch me.", "~", "Watch me now.", "Mirá.", &accepted);
    assert_eq!((r.status, r.needs), (RowStatus::OtherChanged, true));
    // ... unless a pending one was made for the English as it reads now.
    let new = [accepted[0].clone(), placed(&[1], PlacedState::Pending, &["Watch me now."])];
    let r = status_of("= Watch me.", "~", "Watch me now.", "Mirá.", &new);
    assert_eq!((r.status, r.needs), (RowStatus::OtherChanged, false));
    // The English changed and changed back to its `=` stamp: the stamps
    // decide (the accepted text isn't stamped, so this side changed).
    let old = [placed(&[1], PlacedState::Accepted, &["Watch it."])];
    let r = status_of("= Watch me.", "~", "Watch me.", "Mirá.", &old);
    assert_eq!((r.status, r.needs), (RowStatus::ThisChanged, false));
    // Several decided: any one reading the original now will do.
    let two = [placed(&[1], PlacedState::Accepted, &["Old."]), placed(&[1], PlacedState::Accepted, &["Watch me now."])];
    assert_eq!(status_of("= Watch me.", "~", "Watch me now.", "Mirá.", &two).status, RowStatus::InSync);
    // Undecide: the blank comes back, the suggestion is pending again.
    let pending = [placed(&[1], PlacedState::Pending, &["Watch me."])];
    let r = status_of("= Watch me.", "~", "Watch me.", "", &pending);
    assert_eq!((r.status, r.needs), (RowStatus::Blank, false));
    // And with no suggestion at all: back to the stamps.
    let r = status_of("= Watch me.", "~", "Watch me.", "", &[]);
    assert_eq!((r.status, r.needs), (RowStatus::Blank, true));
}

#[test]
fn kept_on_a_blank_is_blank_not_needs() {
    let kept = [placed(&[1], PlacedState::Kept, &["Watch me."])];
    let r = status_of("= Watch me.", "~", "Watch me.", "", &kept);
    assert_eq!((r.status, r.needs), (RowStatus::Blank, false));
    // Both sides blank: nothing to translate.
    let r = status_of("=", "~", "", "", &kept);
    assert_eq!((r.status, r.needs), (RowStatus::InSync, false));
    // Kept on a line with text counts.
    let r = status_of("= Watch me.", "= Mirá.", "Watch me.", "Mirá cómo.", &kept);
    assert_eq!(r.status, RowStatus::InSync);
}

#[test]
fn a_pending_suggestion_stops_needs() {
    let pending = [placed(&[1], PlacedState::Pending, &["Watch me now."])];
    let r = status_of("= Watch me.", "= Mirá.", "Watch me now.", "Mirá.", &pending);
    assert_eq!((r.status, r.needs, r.suggestions.clone()), (RowStatus::OtherChanged, false, vec![0]));
    // One made for an older English doesn't stop it.
    let stale = [placed(&[1], PlacedState::Pending, &["Watch me."])];
    let r = status_of("= Watch me.", "= Mirá.", "Watch me now.", "Mirá.", &stale);
    assert_eq!((r.status, r.needs), (RowStatus::OtherChanged, true));
    // Placed on a paragraph no row holds: ignored.
    let r = status_of("= Watch me.", "= Mirá.", "Watch me now.", "Mirá.", &[placed(&[99], PlacedState::Pending, &[])]);
    assert!(r.needs);
}

#[test]
fn non_editable_rows_never_need() {
    let l = links("1 | 1\n  a1 \"End of Act\" = END OF ACT ONE\n  b1 \"End of Act\"\n");
    let a = script(&[(1, Other("End of Act".into()), "END OF ACT TWO")]);
    let b = script(&[(1, Other("End of Act".into()), "")]);
    let r = lay(&l, &a, &b, &[]).remove(0);
    assert_eq!((r.status, r.needs, r.note), (RowStatus::Blank, false, Some(Note::NotEditable)));
    assert_eq!(Note::NotEditable.text(), "can't be translated here; change it in Final Draft");
}

/// Rows `16 | 17`, `17 | 18` (the plan's D3 example 1).
fn cue_and_line() -> (Links, Script, Script) {
    let l = links(
        "16 | 17\n  a16 character = TOMÁS\n  b17 character = TOMÁS\n17 | 18\n  a17 dialogue = You're not getting on that boat.\n  b18 dialogue = No vas a subir a ese barco.\n",
    );
    let a = script(&[(16, Character, "TOMÁS"), (17, Dialogue, "You're not getting on that boat.")]);
    let b = script(&[(17, Character, "TOMÁS"), (18, Dialogue, "No vas a subir a ese barco.")]);
    (l, a, b)
}

#[test]
fn d3_enter_in_the_middle_joins_its_row() {
    let (l, a, b) = cue_and_line();
    let mut doc = Document::new(b);
    doc.enter(Selection::cursor(Pos::new(ParaId(18), 7))).unwrap();
    let b = doc.script().clone();
    assert_eq!(b.paragraphs.len(), 3);
    let tail = b.paragraphs[2].id;
    let rows = lay(&l, &a, &b, &[]);
    assert_eq!(rows[1].cells[1], vec![ParaId(18), tail]);
    assert_eq!(statuses(&rows), [RowStatus::InSync, RowStatus::ThisChanged]);
}

#[test]
fn d3_new_line_goes_into_the_empty_row_after() {
    // The plan's D3 example 2: rows `18 | 20`, `19 |`, `20 | 21`; an empty
    // Dialogue added after b20 in Final Draft (unlisted id 22).
    let l = links(
        "18 | 20\n  a18 dialogue = Watch me.\n  b20 dialogue = Mirá.\n19 |\n  a19 dialogue = Now.\n20 | 21\n  a20 action = She goes.\n  b21 action = Se va.\n",
    );
    let a = script(&[(18, Dialogue, "Watch me."), (19, Dialogue, "Now."), (20, Action, "She goes.")]);
    let b = script(&[(20, Dialogue, "Mirá."), (22, Dialogue, ""), (21, Action, "Se va.")]);
    let rows = lay(&l, &a, &b, &[]);
    assert_eq!(rows[1].cells[1], ids(&[22]));
    assert_eq!(statuses(&rows), [RowStatus::InSync, RowStatus::Blank, RowStatus::InSync]);
    assert!(rows[1].needs);
    assert_eq!(rows[1].note, None);

    // Two new lines and one empty row: the first takes the row, the second
    // follows it there (order kept), not back into the row above.
    let b = script(&[(20, Dialogue, "Mirá."), (22, Dialogue, ""), (23, Dialogue, ""), (21, Action, "Se va.")]);
    let rows = lay(&l, &a, &b, &[]);
    assert_eq!(rows[0].cells[1], ids(&[20]));
    assert_eq!(rows[1].cells[1], ids(&[22, 23]));

    // A new line before every listed one goes into the first empty row
    // before the next listed paragraph, or else that paragraph's row.
    let b = script(&[(9, Action, "x"), (20, Dialogue, "Mirá."), (21, Action, "Se va.")]);
    let rows = lay(&l, &a, &b, &[]);
    assert_eq!(rows[0].cells[1], ids(&[9, 20]));
    // A new line at the end with no listed paragraph after it.
    let b = script(&[(20, Dialogue, "Mirá."), (21, Action, "Se va."), (30, Action, "y")]);
    assert_eq!(lay(&l, &a, &b, &[])[2].cells[1], ids(&[21, 30]));
}

#[test]
fn both_sides_blank_is_in_sync() {
    // A blank line new in the original (C4) and a blank version line placed
    // there by D3: nothing to translate.
    let l = links("18 | 20\n  a18 dialogue = Watch me.\n  b20 dialogue = Mirá.\n19 |\n  a19 action =\n");
    let a = script(&[(18, Dialogue, "Watch me."), (19, Action, "")]);
    let b = script(&[(20, Dialogue, "Mirá."), (22, Action, "")]);
    let rows = lay(&l, &a, &b, &[]);
    assert_eq!(rows[1].cells[1], ids(&[22]));
    assert_eq!((rows[1].status, rows[1].needs), (RowStatus::InSync, false));
}

#[test]
fn a_decided_row_split_since_reads_this_changed_once_not_placed() {
    // Accepted, then split by Enter in the middle: the resolver no longer
    // places the entry (the version reads two lines), so the stamps decide.
    let (l, a, b) = cue_and_line();
    let mut doc = Document::new(b);
    doc.enter(Selection::cursor(Pos::new(ParaId(18), 7))).unwrap();
    let b = doc.script().clone();
    let row_text = strings(&["You're not getting on that boat."]);
    let chosen = texts(&[&["No vas a subir a ese barco."]]);
    let r = Resolver::new(&l, &a, &b, &caps);
    assert_eq!(r.candidates(&query(&row_text, &chosen, &[])), Err(NotFound::VersionReadsNone));
    let rows = lay(&l, &a, &b, &[]);
    assert_eq!(rows[1].status, RowStatus::ThisChanged);
    // Were it still placed (on the first half), it would read in sync.
    let p = [placed(&[18], PlacedState::Accepted, &["You're not getting on that boat."])];
    assert_eq!(lay(&l, &a, &b, &p)[1].status, RowStatus::InSync);
}

#[test]
fn enter_at_the_start_joins_the_row_above() {
    // core: Enter at the start of a non-empty line inserts an empty
    // paragraph above it, and the text keeps its id.
    let (l, a, b) = cue_and_line();
    let mut doc = Document::new(b);
    doc.enter(Selection::cursor(Pos::new(ParaId(18), 0))).unwrap();
    let b = doc.script().clone();
    let new = b.paragraphs[1].id;
    assert!(b.paragraphs[1].text().is_empty());
    let rows = lay(&l, &a, &b, &[]);
    assert_eq!(rows[0].cells[1], vec![ParaId(17), new]);
    assert_eq!(rows[1].cells[1], ids(&[18]));
    assert_eq!(statuses(&rows), [RowStatus::ThisChanged, RowStatus::InSync]);
}

#[test]
fn backspace_under_a_blank_leaves_the_row_above_one_sided() {
    // b18 blank, b19 translated; Backspace at the start of b19 removes the
    // blank, and b19 keeps its id.
    let l = links(
        "17 | 18\n  a17 dialogue = Hi.\n  b18 dialogue ~\n18 | 19\n  a18 dialogue = Watch me.\n  b19 dialogue = Mirá.\n",
    );
    let a = script(&[(17, Dialogue, "Hi."), (18, Dialogue, "Watch me.")]);
    let b = script(&[(18, Dialogue, ""), (19, Dialogue, "Mirá.")]);
    let mut doc = Document::new(b);
    doc.delete_backward(Selection::cursor(Pos::new(ParaId(19), 0)), false).unwrap();
    let b = doc.script().clone();
    assert_eq!(b.paragraphs.iter().map(|p| p.id).collect::<Vec<_>>(), ids(&[19]));
    let rows = lay(&l, &a, &b, &[]);
    assert_eq!(statuses(&rows), [RowStatus::OneSided, RowStatus::InSync]);
    assert!(rows[0].cells[1].is_empty());
    assert_eq!(rows[0].note, None, "not new in the original: the version's line was removed");
}

// The resolver.

fn texts(t: &[&[&str]]) -> Vec<Vec<String>> {
    t.iter().map(|lines| lines.iter().map(|s| s.to_string()).collect()).collect()
}

fn strings(t: &[&str]) -> Vec<String> {
    t.iter().map(|s| s.to_string()).collect()
}

fn query<'a>(row_text: &'a [String], texts: &'a [Vec<String>], para: &'a [ParaId]) -> RowQuery<'a> {
    RowQuery { links_id: "lk-test", row_text, texts, para }
}

#[test]
fn resolver_finds_the_row_by_its_texts() {
    let l = links("1 | 1\n  a1 dialogue = Hi.\n  b1 dialogue ~\n2 | 2\n  a2 dialogue = Watch me.\n  b2 dialogue ~\n");
    let a = script(&[(1, Dialogue, "Hi."), (2, Dialogue, "Watch me.")]);
    let b = script(&[(1, Dialogue, ""), (2, Dialogue, "")]);
    let r = Resolver::new(&l, &a, &b, &caps);
    let row_text = strings(&["Watch me."]);
    let t = texts(&[&[""], &["Mirá."]]);
    assert_eq!(r.candidates(&query(&row_text, &t, &[])), Ok(vec![ids(&[2])]));
    // Another pair's entry.
    let other = r.candidates(&RowQuery { links_id: "lk-other", row_text: &row_text, texts: &t, para: &[] });
    assert_eq!(other, Err(NotFound::OtherPair));
    // The English reworded since.
    let a2 = script(&[(1, Dialogue, "Hi."), (2, Dialogue, "Watch me go.")]);
    let r = Resolver::new(&l, &a2, &b, &caps);
    let e = r.candidates(&query(&row_text, &t, &[])).unwrap_err();
    assert_eq!(e, NotFound::OriginalChanged { label: "English".into() });
    assert_eq!(e.text(), "the English line changed since this suggestion");
}

#[test]
fn resolver_survives_shifted_ids() {
    // A line inserted above in both files (a Final Draft save renumbers):
    // the same rows are found under their new ids.
    let l =
        links("1 | 1\n  a1 action = Rain.\n  b1 action = Lluvia.\n2 | 2\n  a2 dialogue = Watch me.\n  b2 dialogue ~\n");
    let a = script(&[(1, Action, "Rain."), (2, Dialogue, "Watch me.")]);
    let b = script(&[(1, Action, "Lluvia."), (2, Dialogue, "")]);
    let row_text = strings(&["Watch me."]);
    let t = texts(&[&[""]]);
    assert_eq!(Resolver::new(&l, &a, &b, &caps).candidates(&query(&row_text, &t, &[])), Ok(vec![ids(&[2])]));
    let l2 = links(
        "1 | 1\n  a1 action = New.\n  b1 action = Nuevo.\n2 | 2\n  a2 action = Rain.\n  b2 action = Lluvia.\n3 | 3\n  a3 dialogue = Watch me.\n  b3 dialogue ~\n",
    );
    let a2 = script(&[(1, Action, "New."), (2, Action, "Rain."), (3, Dialogue, "Watch me.")]);
    let b2 = script(&[(1, Action, "Nuevo."), (2, Action, "Lluvia."), (3, Dialogue, "")]);
    // `PARA: 2` is stale now; it names a row that doesn't fit and is ignored.
    assert_eq!(Resolver::new(&l2, &a2, &b2, &caps).candidates(&query(&row_text, &t, &ids(&[2]))), Ok(vec![ids(&[3])]));
}

#[test]
fn resolver_repeated_cue_decided_stays_on_its_row() {
    let l = links(
        "1 | 1\n  a1 character = WOMAN\n  b1 character = WOMAN\n2 | 2\n  a2 dialogue = Hi.\n  b2 dialogue = Hola.\n3 | 3\n  a3 character = WOMAN\n  b3 character = WOMAN\n",
    );
    let a = script(&[(1, Character, "WOMAN"), (2, Dialogue, "Hi."), (3, Character, "WOMAN")]);
    // The first accepted ("MUJER"), the second still pending.
    let b = script(&[(1, Character, "MUJER"), (2, Dialogue, "Hola."), (3, Character, "WOMAN")]);
    let r = Resolver::new(&l, &a, &b, &caps);
    let row_text = strings(&["WOMAN"]);
    // Pending, placed first: BEFORE and the options. Both rows read one of
    // them, but the row still reading BEFORE comes first, even with a stale
    // `PARA` naming the accepted row.
    let pending = texts(&[&["WOMAN"], &["MUJER"]]);
    assert_eq!(r.candidates(&query(&row_text, &pending, &[])), Ok(vec![ids(&[3]), ids(&[1])]));
    assert_eq!(r.candidates(&query(&row_text, &pending, &ids(&[1]))), Ok(vec![ids(&[3]), ids(&[1])]));
    // Decided: review passes the option it chose.
    let decided = texts(&[&["Mujer"]]);
    assert_eq!(r.candidates(&query(&row_text, &decided, &[])), Ok(vec![ids(&[1])]));
}

#[test]
fn resolver_kept_then_hand_typed_is_not_found() {
    let l = links("1 | 1\n  a1 dialogue = Hi.\n  b1 dialogue = Hola.\n");
    let a = script(&[(1, Dialogue, "Hi.")]);
    let row_text = strings(&["Hi."]);
    let kept = texts(&[&["Hola."]]);
    let b = script(&[(1, Dialogue, "Hola.")]);
    assert_eq!(Resolver::new(&l, &a, &b, &caps).candidates(&query(&row_text, &kept, &[])), Ok(vec![ids(&[1])]));
    let b = script(&[(1, Dialogue, "Hola, amigo.")]);
    let e = Resolver::new(&l, &a, &b, &caps).candidates(&query(&row_text, &kept, &[]));
    assert_eq!(e, Err(NotFound::VersionReadsNone));
}

#[test]
fn resolver_para_picks_among_identical_rows() {
    let l = links("1 | 1\n  a1 dialogue = Yes.\n  b1 dialogue ~\n2 | 2\n  a2 dialogue = Yes.\n  b2 dialogue ~\n");
    let a = script(&[(1, Dialogue, "Yes."), (2, Dialogue, "Yes.")]);
    let b = script(&[(1, Dialogue, ""), (2, Dialogue, "")]);
    let r = Resolver::new(&l, &a, &b, &caps);
    let row_text = strings(&["Yes."]);
    let t = texts(&[&[""]]);
    assert_eq!(r.candidates(&query(&row_text, &t, &[])), Ok(vec![ids(&[1]), ids(&[2])]));
    assert_eq!(r.candidates(&query(&row_text, &t, &ids(&[2]))), Ok(vec![ids(&[2]), ids(&[1])]));

    // The first "Yes." accepted as "Sí."; a pending "Yes." entry (BEFORE
    // blank, option "Sí.") goes on the blank row, not the accepted one.
    let b = script(&[(1, Dialogue, "Sí."), (2, Dialogue, "")]);
    let r = Resolver::new(&l, &a, &b, &caps);
    let pending = texts(&[&[""], &["Sí."]]);
    assert_eq!(r.candidates(&query(&row_text, &pending, &ids(&[1]))), Ok(vec![ids(&[2]), ids(&[1])]));
}

#[test]
fn resolver_multi_paragraph_rows_and_one_sided() {
    // A 1:2 row: ROW TEXT has one line, the version side two.
    let l = links("1 | 1 2\n  a1 action = Rain. She counts.\n  b1 action ~\n  b2 action ~\n2 |\n  a2 action = Gone.\n");
    let a = script(&[(1, Action, "Rain. She counts."), (2, Action, "Gone.")]);
    let b = script(&[(1, Action, ""), (2, Action, "")]);
    let r = Resolver::new(&l, &a, &b, &caps);
    let row_text = strings(&["Rain. She counts."]);
    assert_eq!(r.candidates(&query(&row_text, &texts(&[&["", ""]]), &[])), Ok(vec![ids(&[1, 2])]));
    assert_eq!(r.candidates(&query(&row_text, &texts(&[&[""]]), &[])), Err(NotFound::VersionReadsNone));
    // A row with nothing on the version's side is never a candidate.
    let e = r.candidates(&query(&strings(&["Gone."]), &texts(&[&[]]), &[]));
    assert_eq!(e, Err(NotFound::VersionReadsNone));
}

#[test]
fn resolver_keeps_an_entry_on_its_own_row_when_its_line_changed() {
    // Two identical rows, both still blank in the original's terms: a
    // translation typed on each. The entry was made for row 1 (`PARA: 1`).
    let l =
        links("1 | 1\n  a1 action = Laura smiles.\n  b1 action ~\n2 | 2\n  a2 action = Laura smiles.\n  b2 action ~\n");
    let a = script(&[(1, Action, "Laura smiles."), (2, Action, "Laura smiles.")]);
    let row_text = strings(&["Laura smiles."]);
    let t = texts(&[&["Laura sonrie y se va."], &["Laura sonríe y se marcha."]]);
    // Split by Enter: still only its own row (review keeps it on the PARA line).
    // The tail is unlisted; D3 puts it in row 1.
    let b = script(&[(1, Action, "Laura sonrie"), (30, Action, " y se va."), (2, Action, "Laura sonrie y se va.")]);
    let r = Resolver::new(&l, &a, &b, &caps);
    assert_eq!(r.candidates(&query(&row_text, &t, &ids(&[1]))), Ok(vec![ids(&[1, 30])]));
    // Without PARA the identical row is found, as before.
    assert_eq!(r.candidates(&query(&row_text, &t, &[])), Ok(vec![ids(&[2])]));
    // Typed into (or joined): not found, never moved to row 2.
    let b = script(&[(1, Action, "Laura se va."), (2, Action, "Laura sonrie y se va.")]);
    let r = Resolver::new(&l, &a, &b, &caps);
    assert_eq!(r.candidates(&query(&row_text, &t, &ids(&[1]))), Err(NotFound::LineChanged));
    // Its own row reading an option (accepted by another entry): ranked as before.
    let b = script(&[(1, Action, "Laura sonríe y se marcha."), (2, Action, "Laura sonrie y se va.")]);
    let r = Resolver::new(&l, &a, &b, &caps);
    assert_eq!(r.candidates(&query(&row_text, &t, &ids(&[1]))), Ok(vec![ids(&[2]), ids(&[1])]));
}
