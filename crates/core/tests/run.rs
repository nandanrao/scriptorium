//! `Edit::replace_run` / `Edit::restore_run`: block suggestions accepted and
//! undone. Each replace and restore is one undo step, replays from the log,
//! and a restore gives back the exact paragraphs and ids.

use scriptorium_core::doc::{DocError, Group};
use scriptorium_core::{Document, ElementKind, NewPara, ParaId, Paragraph, Pos, Run, Script, Selection, Span, Style};

fn script(paras: &[(&str, &str)]) -> Script {
    Script {
        paragraphs: paras
            .iter()
            .enumerate()
            .map(|(i, (k, t))| Paragraph::plain(ParaId(i as u64), ElementKind::from_name(k), *t))
            .collect(),
        ..Script::default()
    }
}

fn sample() -> Script {
    script(&[
        ("Scene Heading", "EXT. FARM HOUSE - NIGHT"),
        ("Action", "They are playing truco."),
        ("Character", "CESAR"),
        ("Dialogue", "I am go to the town tomorrow."),
        ("Action", "Laura smiles."),
    ])
}

/// 0 plain, 1..=2 dual dialogue group 0, 3..=4 group 1, 5 plain.
fn dual() -> Script {
    let mut s = script(&[
        ("Action", "Before."),
        ("Character", "ANA"),
        ("Dialogue", "Hi."),
        ("Character", "BEA"),
        ("Dialogue", "Hello."),
        ("Action", "After."),
    ]);
    s.fixed = [(ParaId(1), 0), (ParaId(2), 0), (ParaId(3), 1), (ParaId(4), 1)].into();
    s
}

fn ids(doc: &Document) -> Vec<u64> {
    doc.paragraphs().iter().map(|p| p.id.0).collect()
}

fn texts(doc: &Document) -> Vec<String> {
    doc.paragraphs().iter().map(|p| p.text()).collect()
}

fn np(kind: &str, text: &str) -> NewPara {
    NewPara::plain(ElementKind::from_name(kind), text)
}

fn pid(ids: &[u64]) -> Vec<ParaId> {
    ids.iter().map(|i| ParaId(*i)).collect()
}

fn c() -> Selection {
    Selection::cursor(Pos::new(ParaId(0), 0))
}

/// The run's paragraphs as `restore_run` takes them.
fn before_of(doc: &Document, run: &[u64]) -> Vec<(ParaId, NewPara)> {
    run.iter()
        .map(|i| {
            let p = doc.paragraph(ParaId(*i)).unwrap();
            (p.id, NewPara { kind: p.kind.clone(), spans: p.spans.clone() })
        })
        .collect()
}

/// Run `f` as one transaction; check it is one undo step that undoes and
/// redoes exactly, and that replaying the saved log reproduces it.
fn one_step<T>(doc: &mut Document, f: impl FnOnce(&mut scriptorium_core::Edit<'_>) -> Result<T, DocError>) -> T {
    let start = doc.initial().clone();
    let before = doc.script().clone();
    let depth = doc.undo_depth();
    let v = doc.edit(c(), Group::Other, |e| f(e).map(|v| (c(), v))).unwrap();
    let after = doc.script().clone();
    assert_eq!(doc.undo_depth(), depth + 1, "one undo step");
    doc.undo().unwrap();
    assert_eq!(doc.script(), &before, "undo restores paragraphs and ids");
    doc.redo().unwrap();
    assert_eq!(doc.script(), &after, "redo restores the result and ids");
    let json = serde_json::to_string(doc.log()).unwrap();
    let log: Vec<scriptorium_core::doc::Event> = serde_json::from_str(&json).unwrap();
    let replayed = Document::replay(start, &log).unwrap();
    assert_eq!(replayed.script(), &after, "replay from the saved log");
    assert_eq!(replayed.next_id(), doc.next_id());
    v
}

/// Replace `run` with `new`, then restore it and check the document is
/// byte-identical to where it started (paragraphs, kinds, spans, ids).
/// Returns the ids `replace_run` gave and the document after the restore.
fn replace_then_restore(start: Script, run: &[u64], new: &[NewPara]) -> (Vec<ParaId>, Document, Document) {
    let mut doc = Document::new(start);
    let original = doc.script().clone();
    let before = before_of(&doc, run);
    let anchor = doc.index_of(ParaId(run[0])).unwrap().checked_sub(1).map(|i| doc.paragraphs()[i].id);
    let got = one_step(&mut doc, |e| e.replace_run(Run::Ids(pid(run)), new));
    assert_eq!(got.after, anchor, "the run's place is reported");
    let got = got.ids;
    let replaced = doc.clone();
    // A cut is restored from the anchor replace_run reported.
    let current = if got.is_empty() { Run::Empty { after: anchor } } else { Run::Ids(got.clone()) };
    one_step(&mut doc, |e| e.restore_run(current, &before));
    assert_eq!(doc.script(), &original, "restore is byte-identical");
    (got, replaced, doc)
}

#[test]
fn one_to_one_same_kind_similar_keeps_the_id() {
    let (got, replaced, _) =
        replace_then_restore(sample(), &[3], &[np("Dialogue", "I am going to the town tomorrow.")]);
    assert_eq!(got, pid(&[3]));
    assert_eq!(ids(&replaced), [0, 1, 2, 3, 4]);
    assert_eq!(texts(&replaced)[3], "I am going to the town tomorrow.");
    assert_eq!(replaced.next_id(), 5, "no id spent");
}

#[test]
fn one_to_one_same_kind_dissimilar_gets_a_new_id() {
    let (got, replaced, _) = replace_then_restore(sample(), &[4], &[np("Action", "The truck drives off.")]);
    assert_eq!(got, pid(&[5]));
    assert_eq!(ids(&replaced), [0, 1, 2, 3, 5]);
}

#[test]
fn one_to_one_different_kind_is_remove_and_insert() {
    let (got, replaced, _) = replace_then_restore(sample(), &[4], &[np("Dialogue", "Laura smiles.")]);
    assert_eq!(got, pid(&[5]));
    assert_eq!(ids(&replaced), [0, 1, 2, 3, 5]);
    assert_eq!(replaced.paragraphs()[4].kind, ElementKind::Dialogue);
}

#[test]
fn n_to_zero_cut_returns_no_ids_and_restores_after_the_anchor() {
    let (got, replaced, _) = replace_then_restore(sample(), &[2, 3], &[]);
    assert!(got.is_empty());
    assert_eq!(ids(&replaced), [0, 1, 4]);
}

#[test]
fn addition_keeps_the_anchor_and_adds_lines_below() {
    let new = [np("Action", "Laura smiles."), np("Action", "Cesar laughs."), np("Transition", "CUT TO:")];
    let (got, replaced, _) = replace_then_restore(sample(), &[4], &new);
    assert_eq!(got, pid(&[4, 5, 6]));
    assert_eq!(ids(&replaced), [0, 1, 2, 3, 4, 5, 6]);
    assert_eq!(texts(&replaced)[6], "CUT TO:");
}

#[test]
fn n_to_m_pairs_similar_same_kind_lines_and_keeps_their_styles() {
    let mut s = sample();
    let bold = Style { bold: true, ..Style::default() };
    s.paragraphs[3].spans = vec![
        Span { text: "I am".into(), style: bold },
        Span { text: " go to the town tomorrow.".into(), style: Style::default() },
    ];
    let new = [
        np("Character", "CESAR"),
        np("Dialogue", "I am going to the town tomorrow."),
        np("Action", "The truck drives off into the night."),
    ];
    let (got, replaced, _) = replace_then_restore(s, &[2, 3, 4], &new);
    assert_eq!(got, pid(&[2, 3, 5]), "equal and similar lines keep ids; the dissimilar one is new");
    assert_eq!(ids(&replaced), [0, 1, 2, 3, 5]);
    let p = replaced.paragraph(ParaId(3)).unwrap();
    assert_eq!(p.text(), "I am going to the town tomorrow.");
    assert_eq!(p.spans[0], Span { text: "I am".into(), style: bold }, "styles on the unchanged start survive");
}

#[test]
fn n_to_m_pairs_in_order_and_never_across_kinds() {
    // "Laura smiles." as Dialogue is not paired with the Action; the new
    // Action line between is dissimilar to everything.
    let new = [
        np("Character", "CESAR"),
        np("Action", "A dog barks somewhere far away."),
        np("Dialogue", "I am go to the town tomorrow!"),
        np("Dialogue", "Laura smiles."),
    ];
    let (got, replaced, _) = replace_then_restore(sample(), &[2, 3, 4], &new);
    assert_eq!(got, pid(&[2, 5, 3, 6]));
    assert_eq!(ids(&replaced), [0, 1, 2, 5, 3, 6]);
    assert_eq!(texts(&replaced)[4], "I am go to the town tomorrow!");
}

#[test]
fn full_replace_of_the_whole_document() {
    let new = [np("Scene Heading", "INT. KITCHEN - DAY"), np("Action", "Nobody is here.")];
    let (got, replaced, _) = replace_then_restore(sample(), &[0, 1, 2, 3, 4], &new);
    assert_eq!(got, pid(&[5, 6]));
    assert_eq!(ids(&replaced), [5, 6]);

    let (got, replaced, _) =
        replace_then_restore(script(&[("Action", "Only.")]), &[0], &[np("Dialogue", "Something else.")]);
    assert_eq!(got, pid(&[1]));
    assert_eq!(ids(&replaced), [1]);
}

#[test]
fn cutting_the_whole_document_is_refused() {
    let mut doc = Document::new(sample());
    let r = doc.edit(c(), Group::Other, |e| e.replace_run(Run::Ids(pid(&[0, 1, 2, 3, 4])), &[]).map(|v| (c(), v)));
    assert_eq!(r, Err(DocError::LastParagraph));
    assert_eq!(doc.script(), Document::new(sample()).script());
    assert!(doc.log().is_empty());
}

#[test]
fn run_is_its_live_span_and_must_be_non_empty() {
    let mut doc = Document::new(sample());
    let r = doc.edit(c(), Group::Other, |e| e.replace_run(Run::Ids(vec![]), &[np("Action", "x")]).map(|v| (c(), v)));
    assert!(matches!(r, Err(DocError::Mismatch(_))));
    assert!(doc.log().is_empty());
    assert_eq!(doc.next_id(), 5, "no id spent by a refused replace");

    // A decided run whose ids changed (some gone, some out of order, lines
    // between): the live first..last span is replaced.
    for (run, want) in [(&[1, 3][..], &[0, 5, 4][..]), (&[3, 2], &[0, 1, 5, 4]), (&[4, 9], &[0, 1, 2, 3, 5])] {
        let mut doc = Document::new(sample());
        let got = one_step(&mut doc, |e| e.replace_run(Run::Ids(pid(run)), &[np("Action", "Something new.")]));
        assert_eq!(got.ids, pid(&[5]));
        assert_eq!(ids(&doc), want, "run {run:?}");
    }
}

#[test]
fn dual_dialogue_lines_are_refused_structural_changes() {
    let mut s = dual();
    s.paragraphs[4] = Paragraph::plain(ParaId(4), ElementKind::Dialogue, "Hello there.");
    let mut doc = Document::new(s);
    let start = doc.script().clone();
    let refuse = |doc: &mut Document, run: &[u64], new: &[NewPara]| {
        let r = doc.edit(c(), Group::Other, |e| e.replace_run(Run::Ids(pid(run)), new).map(|v| (c(), v)));
        assert!(matches!(r, Err(DocError::Fixed(_))), "{r:?}");
    };
    refuse(&mut doc, &[1, 2], &[np("Action", "Something else entirely.")]);
    refuse(&mut doc, &[2], &[]);
    refuse(&mut doc, &[1], &[np("Character", "ANA"), np("Action", "Inside the block.")]);
    refuse(&mut doc, &[2], &[np("Action", "Hi.")]);
    assert_eq!(doc.script(), &start);
    assert!(doc.log().is_empty());

    // A fixed line takes any text by position with a same-kind line, even a
    // dissimilar one (D2 allows text splices).
    let got = one_step(&mut doc, |e| {
        e.replace_run(Run::Ids(pid(&[3, 4])), &[np("Character", "BEATRIZ"), np("Dialogue", "Something else entirely.")])
    });
    assert_eq!(got.ids, pid(&[3, 4]));
    assert_eq!(texts(&doc)[3..5], ["BEATRIZ", "Something else entirely."]);
    doc.undo().unwrap();

    // Text edits of a paired line are allowed, as are lines after the block.
    let got = one_step(&mut doc, |e| {
        e.replace_run(
            Run::Ids(pid(&[4, 5])),
            &[np("Dialogue", "Hello there!"), np("Action", "After."), np("Action", "New.")],
        )
    });
    assert_eq!(got.ids, pid(&[4, 5, 6]));
    assert_eq!(texts(&doc)[4], "Hello there!");
}

#[test]
fn replace_after_a_cut_inserts_at_the_cut() {
    let mut doc = Document::new(sample());
    one_step(&mut doc, |e| e.replace_run(Run::Ids(pid(&[2, 3])), &[]));
    let new = [np("Character", "LAURA"), np("Dialogue", "Go then.")];
    let got = one_step(&mut doc, |e| e.replace_run(Run::Empty { after: Some(ParaId(1)) }, &new));
    assert_eq!(got.ids, pid(&[5, 6]));
    assert_eq!(got.after, Some(ParaId(1)));
    assert_eq!(ids(&doc), [0, 1, 5, 6, 4]);
    let got = one_step(&mut doc, |e| e.replace_run(Run::Empty { after: None }, &[np("Action", "First.")]));
    assert_eq!(got.ids, pid(&[7]));
    assert_eq!(ids(&doc), [7, 0, 1, 5, 6, 4]);
}

#[test]
fn restore_covers_lines_added_inside_and_skips_gone_after_ids() {
    let mut doc = Document::new(sample());
    let original = doc.script().clone();
    let before = before_of(&doc, &[4]);
    let new = [np("Action", "Laura smiles."), np("Action", "Cesar laughs."), np("Transition", "CUT TO:")];
    let after = one_step(&mut doc, |e| e.replace_run(Run::Ids(pid(&[4])), &new)).ids;
    assert_eq!(after, pid(&[4, 5, 6]));
    // By hand: remove the anchor, add a line between the other two.
    doc.edit(c(), Group::Other, |e| {
        e.remove_para(ParaId(4))?;
        e.insert_para(5, np("Action", "By hand."))?;
        Ok((c(), ()))
    })
    .unwrap();
    assert_eq!(ids(&doc), [0, 1, 2, 3, 5, 7, 6]);
    one_step(&mut doc, |e| e.restore_run(Run::Ids(after.clone()), &before));
    assert_eq!(doc.script(), &original);
}

#[test]
fn restore_of_a_cut_with_a_gone_anchor_uses_the_nearest_live_predecessor() {
    let mut doc = Document::new(sample());
    let before = before_of(&doc, &[2, 3]);
    one_step(&mut doc, |e| e.replace_run(Run::Ids(pid(&[2, 3])), &[]));
    doc.edit(c(), Group::Other, |e| e.remove_para(ParaId(1)).map(|v| (c(), v))).unwrap();
    one_step(&mut doc, |e| e.restore_run(Run::Empty { after: Some(ParaId(1)) }, &before));
    assert_eq!(ids(&doc), [0, 2, 3, 4]);
    assert_eq!(texts(&doc)[2], "I am go to the town tomorrow.");
}

#[test]
fn restore_when_no_after_id_is_live_goes_where_the_lines_were() {
    let mut doc = Document::new(sample());
    let original = doc.script().clone();
    let before = before_of(&doc, &[3]);
    let after =
        one_step(&mut doc, |e| e.replace_run(Run::Ids(pid(&[3])), &[np("Dialogue", "Nothing alike here.")])).ids;
    doc.edit(c(), Group::Other, |e| e.remove_para(after[0]).map(|v| (c(), v))).unwrap();
    one_step(&mut doc, |e| e.restore_run(Run::Ids(after.clone()), &before));
    assert_eq!(doc.script(), &original);
}

#[test]
fn restore_refuses_a_line_live_outside_the_run() {
    let mut doc = Document::new(sample());
    let before = before_of(&doc, &[2, 3]);
    let r = doc.edit(c(), Group::Other, |e| e.restore_run(Run::Ids(pid(&[3])), &before).map(|v| (c(), v)));
    assert!(matches!(r, Err(DocError::Mismatch(_))));
}

#[test]
fn ids_are_never_reused_except_by_restore() {
    let mut doc = Document::new(sample());
    let before = before_of(&doc, &[4]);
    let got = one_step(&mut doc, |e| e.replace_run(Run::Ids(pid(&[4])), &[np("Action", "Totally new line.")])).ids;
    assert_eq!(got, pid(&[5]));
    // Undo, then a different replace: the undone id 5 is not handed out again.
    doc.undo().unwrap();
    let got = one_step(&mut doc, |e| e.replace_run(Run::Ids(pid(&[4])), &[np("Action", "Another new line.")])).ids;
    assert_eq!(got, pid(&[6]));
    // Restore brings 4 back (its own id), and removes 6.
    one_step(&mut doc, |e| e.restore_run(Run::Ids(got.clone()), &before));
    assert_eq!(ids(&doc), [0, 1, 2, 3, 4]);
    // After a rebase too, fresh ids stay above everything used.
    let mut doc = doc.rebase();
    let got = one_step(&mut doc, |e| e.replace_run(Run::Ids(pid(&[4])), &[np("Action", "Yet another.")])).ids;
    assert_eq!(got, pid(&[7]));
}
