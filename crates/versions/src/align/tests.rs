use std::collections::BTreeMap;

use scriptorium_core::rules::is_caps;
use scriptorium_core::{ElementKind, ParaId, Paragraph, Script};

use super::{Alignment, by_text, refind};
use crate::links::{self, Para, Stamp};

use ElementKind::{Action as A, Character as C, Dialogue as D, SceneHeading as H};

fn caps(kind: &ElementKind) -> bool {
    is_caps(kind)
}

/// Old side: ids from `first`, each `(kind, "=text" | "~text" | "")`
/// ("" = no stamp).
fn old(first: u64, items: &[(ElementKind, &str)]) -> BTreeMap<ParaId, Para> {
    items
        .iter()
        .enumerate()
        .map(|(i, (kind, s))| {
            let stamp = if let Some(t) = s.strip_prefix('=') {
                Some(Stamp::Synced(t.to_string()))
            } else {
                s.strip_prefix('~').map(|t| Stamp::Placeholder(t.to_string()))
            };
            (ParaId(first + i as u64), Para { kind: kind.clone(), stamp })
        })
        .collect()
}

fn script(items: &[(ElementKind, &str)]) -> Script {
    Script {
        paragraphs: items
            .iter()
            .enumerate()
            .map(|(i, (kind, text))| Paragraph::plain(ParaId(i as u64), kind.clone(), *text))
            .collect(),
        ..Default::default()
    }
}

fn ids(v: &[u64]) -> Vec<ParaId> {
    v.iter().map(|i| ParaId(*i)).collect()
}

fn pairs(v: &[(u64, u64)]) -> Vec<(ParaId, ParaId)> {
    v.iter().map(|(a, b)| (ParaId(*a), ParaId(*b))).collect()
}

fn align(o: &BTreeMap<ParaId, Para>, n: &Script) -> Alignment {
    by_text(o, n, &caps)
}

/// The plan's C3 case: old version `[H, A"", A"", A"", C, D"", H]`, new
/// `[H, A"x", A"", A"y", C, D"", H]`: H, C and the second H pair in step 1;
/// the three Actions and the Dialogue pair by position in step 2.
#[test]
fn c3_blanks_pair_by_position_only() {
    let o = old(
        0,
        &[(H, "~INT. FERRY - NIGHT"), (A, "~"), (A, "~"), (A, "~"), (C, "=TOMÁS"), (D, "~"), (H, "~EXT. DOCK - DAY")],
    );
    let n = script(&[
        (H, "INT. FERRY - NIGHT"),
        (A, "x"),
        (A, ""),
        (A, "y"),
        (C, "TOMÁS"),
        (D, ""),
        (H, "EXT. DOCK - DAY"),
    ]);
    let al = align(&o, &n);
    assert_eq!(al.pairs, pairs(&[(0, 0), (1, 1), (2, 2), (3, 3), (4, 4), (5, 5), (6, 6)]));
    assert_eq!(al.exact, 3, "H, C and the second H are exact; nothing blank is");
    assert_eq!(al.changed(), 4);
    assert!(al.new.is_empty() && al.gone.is_empty());
}

/// Blanks never match text-equal in step 1: a blank new line must not pair
/// with a later old blank across a found anchor.
#[test]
fn blank_never_exact_even_with_unstamped_old() {
    // Old blank (no stamp) and a new blank: equal text "", but never step 1.
    let o = old(0, &[(A, ""), (A, "=")]);
    let n = script(&[(A, ""), (A, "  ")]);
    let al = align(&o, &n);
    assert_eq!(al.exact, 0);
    assert_eq!(al.pairs, pairs(&[(0, 0), (1, 1)]));
}

#[test]
fn line_edited() {
    let o = old(0, &[(A, "=One."), (A, "=Two."), (A, "=Three.")]);
    let n = script(&[(A, "One."), (A, "Two, edited."), (A, "Three.")]);
    let al = align(&o, &n);
    assert_eq!(al.pairs, pairs(&[(0, 0), (1, 1), (2, 2)]));
    assert_eq!(al.exact, 2);
}

#[test]
fn line_inserted() {
    let o = old(0, &[(A, "=One."), (A, "=Two.")]);
    let n = script(&[(A, "One."), (A, "New."), (A, "Two.")]);
    let al = align(&o, &n);
    assert_eq!(al.pairs, pairs(&[(0, 0), (1, 2)]));
    assert_eq!(al.new, ids(&[1]));
    assert!(al.gone.is_empty());
}

#[test]
fn line_removed() {
    let o = old(0, &[(A, "=One."), (A, "=Two."), (A, "=Three.")]);
    let n = script(&[(A, "One."), (A, "Three.")]);
    let al = align(&o, &n);
    assert_eq!(al.pairs, pairs(&[(0, 0), (2, 1)]));
    assert_eq!(al.gone, ids(&[1]));
    assert!(al.new.is_empty());
}

#[test]
fn whole_file_resaved_with_other_whitespace_and_quotes() {
    let o = old(0, &[(H, "=INT. ROOM - DAY"), (A, "=She says \"hi\" - then leaves..."), (C, "=ANA")]);
    let n = script(&[
        (H, "  int. room – day "),
        (A, "She  says \u{201C}hi\u{201D} \u{2014} then leaves\u{2026}"),
        (C, "Ana"),
    ]);
    let al = align(&o, &n);
    assert_eq!(al.exact, 3);
    assert!(al.new.is_empty() && al.gone.is_empty());
}

#[test]
fn translated_heading_saved_in_final_draft() {
    let o = old(0, &[(H, "~INT. FERRY TERMINAL - NIGHT"), (A, "=La lluvia."), (C, "=TOMÁS")]);
    let n = script(&[(H, "INT. TERMINAL DE FERRIS - NOCHE"), (A, "La lluvia."), (C, "TOMÁS")]);
    let al = align(&o, &n);
    assert_eq!(al.pairs, pairs(&[(0, 0), (1, 1), (2, 2)]));
    assert_eq!(al.exact, 2);
}

#[test]
fn fully_rewritten_line_between_found_neighbours() {
    let o = old(0, &[(C, "=ANA"), (D, "=I never said that."), (C, "=TOMÁS")]);
    let n = script(&[(C, "ANA"), (D, "Completely different words."), (C, "TOMÁS")]);
    let al = align(&o, &n);
    assert_eq!(al.pairs, pairs(&[(0, 0), (1, 1), (2, 2)]));
    assert_eq!(al.changed(), 1);
}

#[test]
fn kinds_must_agree_in_step_two() {
    // The dialogue was retyped as action in Final Draft: not the same line.
    let o = old(0, &[(C, "=ANA"), (D, "=Hello."), (C, "=TOMÁS")]);
    let n = script(&[(C, "ANA"), (A, "Hello."), (C, "TOMÁS")]);
    let al = align(&o, &n);
    assert_eq!(al.pairs, pairs(&[(0, 0), (2, 2)]));
    assert_eq!(al.new, ids(&[1]));
    assert_eq!(al.gone, ids(&[1]));
}

#[test]
fn final_draft_drops_empty_paragraphs() {
    let o = old(0, &[(H, "=INT. A - DAY"), (A, "="), (A, "=Text."), (A, "=")]);
    let n = script(&[(H, "INT. A - DAY"), (A, "Text.")]);
    let al = align(&o, &n);
    assert_eq!(al.pairs, pairs(&[(0, 0), (2, 1)]));
    assert_eq!(al.gone, ids(&[1, 3]));
}

#[test]
fn final_draft_keeps_empty_paragraphs() {
    let o = old(0, &[(H, "=INT. A - DAY"), (A, "="), (A, "=Text."), (A, "=")]);
    let n = script(&[(H, "INT. A - DAY"), (A, ""), (A, "Text."), (A, "")]);
    let al = align(&o, &n);
    assert_eq!(al.pairs, pairs(&[(0, 0), (1, 1), (2, 2), (3, 3)]));
    assert!(al.new.is_empty() && al.gone.is_empty());
}

// ---- refind: the links file rewritten ----

const LINKS: &str = "\
SCRIPTORIUM LINKS v1
Id: lk-1
a: PURPLE LIST.fdx
a language: en
a hash: aaaa
b: PURPLE LIST.es.fdx
b language: es
b hash: bbbb
b created: version new
---
0 | 0
  a0 heading = INT. FERRY TERMINAL - NIGHT
  b0 heading ~ INT. FERRY TERMINAL - NIGHT
1 | 1
  a1 action = Rain hammers the glass.
  b1 action = La lluvia golpea el cristal.
2 | 2
  a2 character = TOMÁS
  b2 character = TOMÁS
3 | 3
  a3 dialogue = You're not getting on that boat.
  b3 dialogue = No vas a subir a ese barco.
4 | 4
  a4 dialogue = Watch me.
  b4 dialogue ~
";

/// `refind`, checking the result is a valid links file.
fn re(l: &links::Links, member: usize, n: &Script, hash: &str) -> (links::Links, Alignment) {
    let (out, al) = refind(l, member, n, hash, &caps);
    links::validate(&out).unwrap();
    (out, al)
}

fn rows(l: &links::Links) -> Vec<(Vec<u64>, Vec<u64>)> {
    l.rows
        .iter()
        .map(|r| (r.cells[0].iter().map(|i| i.0).collect(), r.cells[1].iter().map(|i| i.0).collect()))
        .collect()
}

fn r(a: &[u64], b: &[u64]) -> (Vec<u64>, Vec<u64>) {
    (a.to_vec(), b.to_vec())
}

#[test]
fn scene_added_to_the_original_gets_rows_of_its_own() {
    let l = links::parse(LINKS).unwrap();
    let n = script(&[
        (H, "INT. FERRY TERMINAL - NIGHT"),
        (A, "Rain hammers the glass."),
        (H, "EXT. DOCK - NIGHT"),
        (A, "Waves."),
        (C, "TOMÁS"),
        (D, "You're not getting on that boat."),
        (D, "Watch me."),
    ]);
    let (out, al) = re(&l, 0, &n, "a2a2");
    assert_eq!(al.new, ids(&[2, 3]));
    assert_eq!(out.members[0].hash, "a2a2");
    assert_eq!(out.members[1].hash, "bbbb");
    assert_eq!(
        rows(&out),
        vec![r(&[0], &[0]), r(&[1], &[1]), r(&[2], &[]), r(&[3], &[]), r(&[4], &[2]), r(&[5], &[3]), r(&[6], &[4])]
    );
    // New lines are stamped with their text; their kind is the new file's.
    assert_eq!(out.paras[0][&ParaId(2)], Para { kind: H, stamp: Some(Stamp::Synced("EXT. DOCK - NIGHT".into())) });
    // Stamps move with their ids, unchanged.
    assert_eq!(out.paras[0][&ParaId(6)].stamp, Some(Stamp::Synced("Watch me.".into())));
    assert_eq!(out.paras[1], l.paras[1]);
    let text = links::write(&out).unwrap();
    assert_eq!(links::parse(&text).unwrap(), out);
}

#[test]
fn new_line_goes_after_nearest_listed_above_before_one_sided_rows() {
    let text = "\
SCRIPTORIUM LINKS v1
Id: lk-1
a: A.fdx
a hash: aaaa
b: B.fdx
b hash: bbbb
b created: version new
---
0 | 0
  a0 action = One.
  b0 action = Uno.
 | 1
  b1 action = Extra.
1 | 2
  a1 action = Two.
  b2 action = Dos.
";
    let l = links::parse(text).unwrap();
    let n = script(&[(A, "One."), (A, "New."), (A, "Two.")]);
    let (out, _) = re(&l, 0, &n, "x");
    assert_eq!(rows(&out), vec![r(&[0], &[0]), r(&[1], &[]), r(&[], &[1]), r(&[2], &[2])]);
}

#[test]
fn new_line_at_the_start_and_end_of_the_original() {
    let l = links::parse(LINKS).unwrap();
    let n = script(&[
        (A, "FADE IN:"),
        (H, "INT. FERRY TERMINAL - NIGHT"),
        (A, "Rain hammers the glass."),
        (C, "TOMÁS"),
        (D, "You're not getting on that boat."),
        (D, "Watch me."),
        (A, "FADE OUT."),
    ]);
    let (out, al) = re(&l, 0, &n, "x");
    assert_eq!(al.new, ids(&[0, 6]));
    assert_eq!(
        rows(&out),
        vec![r(&[0], &[]), r(&[1], &[0]), r(&[2], &[1]), r(&[3], &[2]), r(&[4], &[3]), r(&[5], &[4]), r(&[6], &[])]
    );
}

/// D3: a Spanish line split in two in Final Draft; the new half stays
/// unlisted (the made member's new lines get no rows).
#[test]
fn version_line_split_new_half_unlisted() {
    let l = links::parse(LINKS).unwrap();
    let n = script(&[
        (H, "INT. FERRY TERMINAL - NIGHT"),
        (A, "La lluvia golpea"),
        (A, "el cristal."),
        (C, "TOMÁS"),
        (D, "No vas a subir a ese barco."),
        (D, ""),
    ]);
    let (out, al) = re(&l, 1, &n, "b2");
    assert_eq!(al.new, ids(&[2]));
    assert_eq!(rows(&out), vec![r(&[0], &[0]), r(&[1], &[1]), r(&[2], &[3]), r(&[3], &[4]), r(&[4], &[5])]);
    assert!(!out.paras[1].contains_key(&ParaId(2)));
    assert_eq!(out.members[1].hash, "b2");
}

/// Two Spanish lines in adjacent rows merged: the second row keeps its
/// English side and reads "nothing on this side".
#[test]
fn version_lines_merged_second_row_one_sided() {
    let l = links::parse(LINKS).unwrap();
    let n = script(&[
        (H, "INT. FERRY TERMINAL - NIGHT"),
        (A, "La lluvia golpea el cristal."),
        (C, "TOMÁS"),
        (D, "No vas a subir a ese barco. Mirá cómo lo hago."),
    ]);
    let (out, al) = re(&l, 1, &n, "b2");
    assert_eq!(al.gone, ids(&[4]));
    assert_eq!(rows(&out), vec![r(&[0], &[0]), r(&[1], &[1]), r(&[2], &[2]), r(&[3], &[3]), r(&[4], &[])]);
    assert!(!out.paras[1].contains_key(&ParaId(4)));
    links::validate(&out).unwrap();
}

/// A row whose ids all disappear is dropped.
#[test]
fn row_left_with_no_ids_is_dropped() {
    let text = "\
SCRIPTORIUM LINKS v1
Id: lk-1
a: A.fdx
a hash: aaaa
b: B.fdx
b hash: bbbb
b created: version new
---
0 | 0
  a0 action = One.
  b0 action = Uno.
 | 1
  b1 action = Extra.
1 | 2
  a1 action = Two.
  b2 action = Dos.
";
    let l = links::parse(text).unwrap();
    let n = script(&[(A, "Uno."), (A, "Dos.")]);
    let (out, al) = re(&l, 1, &n, "x");
    assert_eq!(al.gone, ids(&[1]));
    assert_eq!(rows(&out), vec![r(&[0], &[0]), r(&[1], &[1])]);
    links::validate(&out).unwrap();
}

/// Nothing changed but the hash: same rows, same stamps.
#[test]
fn unchanged_file_keeps_everything() {
    let l = links::parse(LINKS).unwrap();
    let n = script(&[
        (H, "INT. FERRY TERMINAL - NIGHT"),
        (A, "La lluvia golpea el cristal."),
        (C, "TOMÁS"),
        (D, "No vas a subir a ese barco."),
        (D, ""),
    ]);
    let (out, al) = re(&l, 1, &n, "bbbb");
    assert_eq!(out, l);
    assert_eq!(al.exact, 4);
    assert_eq!(al.changed(), 1);
}

const MULTI: &str = "\
SCRIPTORIUM LINKS v1
Id: lk-1
a: A.fdx
a hash: aaaa
b: B.fdx
b hash: bbbb
b created: version new
---
0 | 0
  a0 action = One.
  b0 action = Uno.
1 2 | 1
  a1 action = Two.
  a2 action = Three.
  b1 action = Dos tres.
3 | 2
  a3 action = Four.
  b2 action = Cuatro.
";

/// A new original line between two ids of one cell joins that cell, so
/// rows never cross.
#[test]
fn new_line_inside_a_multi_id_cell_joins_it() {
    let l = links::parse(MULTI).unwrap();
    let n = script(&[(A, "One."), (A, "Two."), (A, "Inserted."), (A, "Three."), (A, "Four.")]);
    let (out, al) = re(&l, 0, &n, "x");
    assert_eq!(al.new, ids(&[2]));
    assert_eq!(rows(&out), vec![r(&[0], &[0]), r(&[1, 2, 3], &[1]), r(&[4], &[2])]);
}

/// A reworded line with a same-kind line inserted just before it keeps its
/// row; the inserted line is the new one.
#[test]
fn reworded_line_with_insert_before_pairs_by_likeness() {
    let o = old(0, &[(H, "=INT. HALL - DAY"), (A, "=The door opens."), (C, "=ANA")]);
    let n = script(&[(H, "INT. HALL - DAY"), (A, "A dog barks."), (A, "The door creaks open."), (C, "ANA")]);
    let al = align(&o, &n);
    assert_eq!(al.pairs, pairs(&[(0, 0), (1, 2), (2, 3)]));
    assert_eq!(al.new, ids(&[1]));
    assert_eq!(al.changed(), 1);
}

/// Equal-length runs still pair by position, however the texts compare.
#[test]
fn equal_runs_pair_by_position() {
    let o = old(0, &[(C, "=ANA"), (A, "=First line."), (A, "=Second line."), (C, "=BOB")]);
    let n = script(&[(C, "ANA"), (A, "Second line!"), (A, "First line!"), (C, "BOB")]);
    let al = align(&o, &n);
    assert_eq!(al.pairs, pairs(&[(0, 0), (1, 1), (2, 2), (3, 3)]));
}

/// A scene added to the original is stamped, so the next re-find finds it
/// exactly instead of pairing it by position again.
#[test]
fn new_original_lines_found_exactly_next_time() {
    let l = links::parse(LINKS).unwrap();
    let n = script(&[
        (H, "INT. FERRY TERMINAL - NIGHT"),
        (A, "Rain hammers the glass."),
        (H, "EXT. DOCK - NIGHT"),
        (A, "Waves."),
        (C, "TOMÁS"),
        (D, "You're not getting on that boat."),
        (D, "Watch me."),
    ]);
    let (once, _) = re(&l, 0, &n, "a2");
    let (twice, al) = re(&once, 0, &n, "a2");
    assert_eq!(twice, once);
    assert_eq!(al.exact, 7);
    assert_eq!(al.changed(), 0);
    assert!(al.new.is_empty() && al.gone.is_empty());
}

/// A scene cut and a different one written at the same spot: every line
/// pairs by kind and position (the rows read "English changed"), nothing
/// is new or gone.
#[test]
fn scene_cut_and_another_written_in_its_place() {
    let text = "\
SCRIPTORIUM LINKS v1
Id: lk-1
a: A.fdx
a hash: aaaa
b: B.fdx
b hash: bbbb
b created: version new
---
0 | 0
  a0 heading = INT. KITCHEN - DAY
  b0 heading = INT. COCINA - DÍA
1 | 1
  a1 action = Pots boil.
  b1 action = Las ollas hierven.
2 | 2
  a2 character = ANA
  b2 character = ANA
3 | 3
  a3 dialogue = It's ready.
  b3 dialogue = Está listo.
";
    let l = links::parse(text).unwrap();
    let n = script(&[(H, "EXT. ROOF - NIGHT"), (A, "Wind."), (C, "BOB"), (D, "Cold up here.")]);
    let (out, al) = re(&l, 0, &n, "x");
    assert_eq!(al.exact, 0);
    assert_eq!(al.changed(), 4);
    assert!(al.new.is_empty() && al.gone.is_empty());
    assert_eq!(rows(&out), rows(&l));
    // Stamps are kept: every row's original side now differs from them.
    assert_eq!(out.paras[0], l.paras[0]);
}

const FARM: &str = "\
SCRIPTORIUM LINKS v1
Id: lk-1
a: film.fdx
a hash: aaaa
b: film.es.fdx
b hash: bbbb
b created: version new
---
0 | 0
  a0 heading = EXT. FARM HOUSE - NIGHT
  b0 heading = EXT. CASA DE CAMPO - NOCHE
1 | 1
  a1 action = Laura smiles.
  b1 action = Laura sonríe.
2 | 2
  a2 character = CESAR
  b2 character = CESAR
";

/// Final Draft splits the Spanish "Laura sonríe." into "Laura" + "sonríe."
/// (and the heading was retyped): the old line pairs with the first half,
/// so by D3 the unlisted "sonríe." follows it into the same row, not the
/// heading's.
#[test]
fn version_line_split_pairs_with_first_half() {
    let l = links::parse(FARM).unwrap();
    let n = script(&[(H, "EXT. CASA DE CAMPO - DE NOCHE"), (A, "Laura"), (A, "sonríe."), (C, "CESAR")]);
    let (out, al) = re(&l, 1, &n, "b2");
    assert_eq!(al.pairs, pairs(&[(0, 0), (1, 1), (2, 3)]));
    assert_eq!(al.new, ids(&[2]));
    assert_eq!(al.split, pairs(&[(1, 2)]));
    assert_eq!(rows(&out), vec![r(&[0], &[0]), r(&[1], &[1]), r(&[2], &[3])]);
}

/// The same split in the original: both halves land in the line's cell,
/// stamped, and the next re-find finds them exactly.
#[test]
fn original_line_split_keeps_both_halves_in_its_row() {
    let l = links::parse(FARM).unwrap();
    let n = script(&[(H, "EXT. FARM HOUSE - NIGHT"), (A, "Laura"), (A, "smiles."), (C, "CESAR")]);
    let (out, al) = re(&l, 0, &n, "a2");
    assert_eq!(al.split, pairs(&[(1, 2)]));
    assert_eq!(rows(&out), vec![r(&[0], &[0]), r(&[1, 2], &[1]), r(&[3], &[2])]);
    // The first half keeps the old stamp (so the row reads changed); the
    // rest is stamped with its own text.
    assert_eq!(out.paras[0][&ParaId(1)].stamp, Some(Stamp::Synced("Laura smiles.".into())));
    assert_eq!(out.paras[0][&ParaId(2)].stamp, Some(Stamp::Synced("smiles.".into())));
    let (again, al) = re(&out, 0, &n, "a2");
    assert_eq!(again, out);
    assert_eq!(al.changed(), 1, "only the first half, against its old stamp");
}

/// A line split in three, the old line most like the middle third.
#[test]
fn line_split_in_three() {
    let o = old(0, &[(C, "=ANA"), (D, "=I said no. I meant it. Go home."), (C, "=BOB")]);
    let n = script(&[(C, "ANA"), (D, "I said no."), (D, "I meant it."), (D, "Go home."), (C, "BOB")]);
    let al = align(&o, &n);
    assert_eq!(al.pairs, pairs(&[(0, 0), (1, 1), (2, 4)]));
    assert_eq!(al.split, pairs(&[(1, 2), (1, 3)]));
}
