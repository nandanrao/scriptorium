//! The `Edit` primitives: each inverts, replays from the log, keeps ids, and
//! respects fresh-id and fixed-paragraph rules.

use scriptorium_core::doc::{DocError, Group};
use scriptorium_core::{Document, ElementKind, NewPara, ParaId, Paragraph, Pos, Script, Selection, Span, Style};

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

/// Two adjacent dual dialogue blocks (groups 0 and 1) between plain lines:
/// 0 plain, 1..=2 group 0, 3..=4 group 1, 5 plain.
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

fn texts(doc: &Document) -> Vec<String> {
    doc.paragraphs().iter().map(|p| p.text()).collect()
}

fn ids(doc: &Document) -> Vec<u64> {
    doc.paragraphs().iter().map(|p| p.id.0).collect()
}

fn cur(para: u64, offset: usize) -> Selection {
    Selection::cursor(Pos::new(ParaId(para), offset))
}

fn plain(text: &str) -> Vec<Span> {
    vec![Span { text: text.into(), style: Style::default() }]
}

/// Run one transaction, check it undoes to the start, redoes to the result,
/// and that replaying the log reproduces the result.
fn check_inverts<T>(start: Script, f: impl FnOnce(&mut scriptorium_core::Edit<'_>) -> Result<T, DocError>) -> Document {
    let mut doc = Document::new(start.clone());
    let before = doc.script().clone();
    doc.edit(cur(0, 0), Group::Other, |e| f(e).map(|v| (cur(0, 0), v))).unwrap();
    let after = doc.script().clone();
    assert_ne!(after, before, "the edit changed something");
    doc.undo().unwrap();
    assert_eq!(doc.script(), &before, "undo restores the script and ids");
    doc.redo().unwrap();
    assert_eq!(doc.script(), &after, "redo restores the result and ids");
    // The log is saved as JSON in session files: no new op kinds.
    let json = serde_json::to_string(doc.log()).unwrap();
    let log: Vec<scriptorium_core::doc::Event> = serde_json::from_str(&json).unwrap();
    let replayed = Document::replay(start, &log).unwrap();
    assert_eq!(replayed.script(), &after, "replay from the saved log");
    assert_eq!(replayed.next_id(), doc.next_id());
    doc
}

#[test]
fn splice_replaces_text_and_inverts() {
    let doc = check_inverts(sample(), |e| e.splice(ParaId(3), 5..7, plain("going")));
    assert_eq!(texts(&doc)[3], "I am going to the town tomorrow.");
}

#[test]
fn splice_refuses_bad_ranges_and_records_no_op() {
    let mut doc = Document::new(script(&[("Action", "ñu")]));
    let r = doc.edit(cur(0, 0), Group::Other, |e| e.splice(ParaId(0), 1..2, vec![]).map(|v| (cur(0, 0), v)));
    assert!(matches!(r, Err(DocError::Mismatch(_))), "inside a multi-byte char");
    let r = doc.edit(cur(0, 0), Group::Other, |e| e.splice(ParaId(0), 0..9, vec![]).map(|v| (cur(0, 0), v)));
    assert!(matches!(r, Err(DocError::Mismatch(_))));
    doc.edit(cur(0, 0), Group::Other, |e| e.splice(ParaId(0), 0..2, plain("ñ")).map(|v| (cur(0, 0), v))).unwrap();
    assert!(doc.log().is_empty(), "a splice that changes nothing is not logged");
}

#[test]
fn insert_para_takes_a_fresh_id_and_inverts() {
    let mut new = None;
    let doc = check_inverts(sample(), |e| {
        new = Some(e.insert_para(2, NewPara::plain(ElementKind::Action, "Wind."))?);
        e.insert_para(6, NewPara::plain(ElementKind::Transition, "CUT TO:"))
    });
    assert_eq!(new, Some(ParaId(5)));
    assert_eq!(ids(&doc), [0, 1, 5, 2, 3, 4, 6]);
    assert_eq!(doc.paragraphs()[6].kind, ElementKind::Transition);
}

#[test]
fn insert_para_with_id_only_brings_back_absent_used_ids() {
    let mut doc = Document::new(sample());
    let s = cur(0, 0);
    doc.edit(s, Group::Other, |e| e.remove_para(ParaId(2)).map(|v| (s, v))).unwrap();
    let live = doc.edit(s, Group::Other, |e| {
        e.insert_para_with_id(0, ParaId(1), NewPara::plain(ElementKind::Action, "x")).map(|v| (s, v))
    });
    assert!(matches!(live, Err(DocError::Mismatch(_))), "id is live");
    let unused = doc.edit(s, Group::Other, |e| {
        e.insert_para_with_id(0, ParaId(5), NewPara::plain(ElementKind::Action, "x")).map(|v| (s, v))
    });
    assert!(matches!(unused, Err(DocError::Mismatch(_))), "id was never used");
    doc.edit(s, Group::Other, |e| {
        e.insert_para_with_id(2, ParaId(2), NewPara::plain(ElementKind::Character, "CESAR")).map(|v| (s, v))
    })
    .unwrap();
    assert_eq!(doc.script(), &Document::new(sample()).script().clone());
    doc.undo();
    assert_eq!(ids(&doc), [0, 1, 3, 4]);
    let replayed = Document::replay(sample(), doc.log()).unwrap();
    assert_eq!(replayed.script(), doc.script());
}

#[test]
fn remove_para_inverts_and_refuses_the_last() {
    let doc = check_inverts(sample(), |e| e.remove_para(ParaId(0)));
    assert_eq!(ids(&doc), [1, 2, 3, 4]);
    let mut one = Document::new(script(&[("Action", "Only.")]));
    let r = one.edit(cur(0, 0), Group::Other, |e| e.remove_para(ParaId(0)).map(|v| (cur(0, 0), v)));
    assert_eq!(r, Err(DocError::LastParagraph));
}

#[test]
fn replacing_everything_inserts_before_removing() {
    let mut doc = Document::new(script(&[("Action", "Old.")]));
    let new = doc
        .edit(cur(0, 0), Group::Other, |e| {
            let id = e.insert_para(1, NewPara::plain(ElementKind::Dialogue, "New."))?;
            e.remove_para(ParaId(0))?;
            Ok((Selection::cursor(Pos::new(id, 0)), id))
        })
        .unwrap();
    assert_eq!(ids(&doc), [new.0]);
    doc.undo();
    assert_eq!(texts(&doc), ["Old."]);
}

#[test]
fn set_kind_inverts() {
    let doc = check_inverts(sample(), |e| e.set_kind(ParaId(4), ElementKind::Dialogue));
    assert_eq!(doc.paragraphs()[4].kind, ElementKind::Dialogue);
}

#[test]
fn split_in_the_middle_keeps_the_head_on_the_original_id() {
    let mut new = ParaId(0);
    let doc = check_inverts(sample(), |e| {
        new = e.split(Pos::new(ParaId(1), 4))?;
        Ok(())
    });
    assert_eq!(new, ParaId(5));
    assert_eq!(ids(&doc), [0, 1, 5, 2, 3, 4]);
    assert_eq!(texts(&doc)[1..3], ["They", " are playing truco."]);
    assert_eq!(doc.paragraphs()[2].kind, ElementKind::Action);
}

#[test]
fn split_at_offset_zero_inserts_above_so_the_text_keeps_its_id() {
    let doc = check_inverts(sample(), |e| e.split(Pos::new(ParaId(3), 0)));
    assert_eq!(ids(&doc), [0, 1, 2, 5, 3, 4]);
    assert_eq!(texts(&doc)[3..5], ["", "I am go to the town tomorrow."]);
    assert_eq!(doc.paragraphs()[3].kind, ElementKind::Dialogue);
}

#[test]
fn split_at_the_end_adds_below_with_the_next_kind() {
    let doc = check_inverts(sample(), |e| e.split(Pos::new(ParaId(2), 5)));
    assert_eq!(ids(&doc), [0, 1, 2, 5, 3, 4]);
    assert_eq!(texts(&doc)[2..4], ["CESAR", ""]);
    assert_eq!(doc.paragraphs()[3].kind, ElementKind::Dialogue);
}

#[test]
fn join_merges_onto_the_first_and_inverts() {
    let mut at = None;
    let doc = check_inverts(sample(), |e| {
        at = Some(e.join(ParaId(3), ParaId(4))?);
        Ok(())
    });
    assert_eq!(at, Some(Pos::new(ParaId(3), "I am go to the town tomorrow.".len())));
    assert_eq!(ids(&doc), [0, 1, 2, 3]);
    let mut doc = Document::new(sample());
    let r = doc.edit(cur(0, 0), Group::Other, |e| e.join(ParaId(1), ParaId(3)).map(|v| (cur(0, 0), v)));
    assert!(matches!(r, Err(DocError::Mismatch(_))), "not adjacent");
}

#[test]
fn set_meta_inverts_with_text_in_one_step() {
    let doc = check_inverts(sample(), |e| {
        e.splice(ParaId(4), 6..12, plain("grins"))?;
        e.set_meta("review/0/1", Some("accepted:0"))
    });
    assert_eq!(doc.meta("review/0/1"), Some("accepted:0"));
    assert_eq!(texts(&doc)[4], "Laura grins.");
}

#[test]
fn a_failed_edit_rolls_back_and_spends_no_id() {
    let mut doc = Document::new(sample());
    let r = doc.edit(cur(0, 0), Group::Other, |e| {
        e.insert_para(0, NewPara::plain(ElementKind::Action, "x"))?;
        e.remove_para(ParaId(99)).map(|v| (cur(0, 0), v))
    });
    assert_eq!(r, Err(DocError::MissingParagraph(ParaId(99))));
    assert_eq!(doc.script(), Document::new(sample()).script());
    assert_eq!(doc.next_id(), 5);
    assert!(doc.log().is_empty());
}

// ---- id floor ------------------------------------------------------------

#[test]
fn cut_last_then_rebase_never_reuses_its_id() {
    let mut doc = Document::new(sample());
    let s = cur(0, 0);
    doc.edit(s, Group::Other, |e| e.remove_para(ParaId(4)).map(|v| (s, v))).unwrap();
    let mut doc = doc.rebase();
    assert_eq!(doc.initial().id_floor, 5);
    let id =
        doc.edit(s, Group::Other, |e| e.insert_para(4, NewPara::plain(ElementKind::Action, "New.")).map(|v| (s, v)));
    assert_eq!(id, Ok(ParaId(5)));
    // Rebuilding from the starting point (as sync's `rebuild` does) keeps the floor.
    let rebuilt = Document::with_meta(doc.initial().clone(), doc.initial_meta().clone());
    assert_eq!(rebuilt.next_id(), 5);
    let rebuilt = rebuilt.replay_onto(doc.log()).unwrap();
    assert_eq!(rebuilt.script(), doc.script());
    assert_eq!(rebuilt.next_id(), 6);
    // And a second rebase carries it on.
    assert_eq!(doc.rebase().next_id(), 6);
}

#[test]
fn id_floor_from_the_script_and_undone_inserts() {
    let mut s = sample();
    s.id_floor = 9;
    let mut doc = Document::new(s);
    assert_eq!(doc.next_id(), 9);
    let c = cur(0, 0);
    doc.edit(c, Group::Other, |e| e.insert_para(0, NewPara::plain(ElementKind::Action, "a")).map(|v| (c, v))).unwrap();
    doc.undo();
    let doc = doc.rebase();
    assert_eq!(doc.next_id(), 10, "an undone insert's id stays used");
    let mut empty = Script { id_floor: 4, ..Script::default() };
    assert_eq!(Document::new(empty.clone()).paragraphs()[0].id, ParaId(4));
    empty.id_floor = 0;
    assert_eq!(Document::new(empty).next_id(), 1);
}

#[test]
fn id_floor_serializes_only_when_set() {
    let json = serde_json::to_string(&sample()).unwrap();
    assert!(!json.contains("id_floor"));
    let back: Script = serde_json::from_str(&json).unwrap();
    assert_eq!(back.id_floor, 0);
    let mut s = sample();
    s.id_floor = 7;
    let back: Script = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(back.id_floor, 7);
}

// ---- fixed paragraphs ----------------------------------------------------

fn refused<T: std::fmt::Debug>(
    doc: &mut Document,
    f: impl FnOnce(&mut scriptorium_core::Edit<'_>) -> Result<T, DocError>,
) -> DocError {
    let before = doc.script().clone();
    let c = cur(0, 0);
    let err = doc.edit(c, Group::Other, |e| f(e).map(|v| (c, v))).unwrap_err();
    assert_eq!(doc.script(), &before);
    err
}

#[test]
fn fixed_paragraphs_refuse_structural_primitives() {
    let mut doc = Document::new(dual());
    let fixed = |e: DocError| assert!(matches!(e, DocError::Fixed(_)), "{e:?}");
    fixed(refused(&mut doc, |e| e.remove_para(ParaId(2))));
    fixed(refused(&mut doc, |e| e.set_kind(ParaId(2), ElementKind::Action)));
    fixed(refused(&mut doc, |e| e.split(Pos::new(ParaId(2), 1))));
    fixed(refused(&mut doc, |e| e.split(Pos::new(ParaId(2), 0))));
    fixed(refused(&mut doc, |e| e.split(Pos::new(ParaId(4), 6))));
    fixed(refused(&mut doc, |e| e.join(ParaId(1), ParaId(2))));
    fixed(refused(&mut doc, |e| e.join(ParaId(2), ParaId(3))));
    fixed(refused(&mut doc, |e| e.join(ParaId(4), ParaId(5))));
    fixed(refused(&mut doc, |e| e.join(ParaId(0), ParaId(1))));
    fixed(refused(&mut doc, |e| e.insert_para(2, NewPara::plain(ElementKind::Action, "x"))));
    fixed(refused(&mut doc, |e| e.insert_para(4, NewPara::plain(ElementKind::Action, "x"))));
    assert!(doc.log().is_empty());
}

#[test]
fn fixed_paragraphs_take_text_edits_and_same_kind() {
    let mut doc = Document::new(dual());
    let c = cur(0, 0);
    doc.edit(c, Group::Other, |e| {
        e.splice(ParaId(2), 0..2, plain("Hey"))?;
        e.set_kind(ParaId(2), ElementKind::Dialogue)?;
        Ok((c, ()))
    })
    .unwrap();
    assert_eq!(texts(&doc)[2], "Hey.");
    doc.replace_text(c, ParaId(4), "Hello there.", &[]).unwrap();
    doc.insert_text(cur(3, 3), "TRIZ").unwrap();
    assert_eq!(texts(&doc)[3], "BEATRIZ");
}

#[test]
fn inserting_between_two_wrappers_or_beside_one_is_allowed() {
    let mut doc = Document::new(dual());
    let c = cur(0, 0);
    let ids_new = doc
        .edit(c, Group::Other, |e| {
            let between = e.insert_para(3, NewPara::plain(ElementKind::Action, "Between."))?;
            let before = e.insert_para(1, NewPara::plain(ElementKind::Action, "Above."))?;
            let after = e.insert_para(e.doc().paragraphs().len() - 1, NewPara::plain(ElementKind::Action, "Below."))?;
            Ok((c, [between, before, after]))
        })
        .unwrap();
    assert_eq!(ids(&doc), [0, ids_new[1].0, 1, 2, ids_new[0].0, 3, 4, ids_new[2].0, 5]);
    // Enter at the start of the plain line after a block adds a line between too.
    doc.enter(cur(5, 0)).unwrap();
    // An empty plain line right after a fixed one can still be removed by
    // Backspace (the fixed line is untouched).
    let c = cur(0, 0);
    let empty = doc
        .edit(c, Group::Other, |e| {
            let i = e.doc().index_of(ParaId(4)).unwrap() + 1;
            Ok((c, e.insert_para(i, NewPara { kind: ElementKind::Action, spans: vec![] })?))
        })
        .unwrap();
    doc.delete_backward(Selection::cursor(Pos::new(empty, 0)), false).unwrap();
    assert!(doc.paragraph(empty).is_none());
    assert_eq!(texts(&doc)[doc.index_of(ParaId(4)).unwrap()], "Hello.");
}

#[test]
fn commands_on_fixed_paragraphs_are_refused_without_logging() {
    let mut doc = Document::new(dual());
    assert!(matches!(doc.enter(cur(2, 1)), Err(DocError::Fixed(_))));
    assert!(matches!(doc.delete_backward(cur(2, 0), false), Err(DocError::Fixed(_))));
    assert!(matches!(doc.delete_forward(cur(4, 6), false), Err(DocError::Fixed(_))));
    assert!(matches!(doc.cycle_kind(cur(1, 0), true), Err(DocError::Fixed(_))));
    let across = Selection { anchor: Pos::new(ParaId(0), 2), head: Pos::new(ParaId(5), 2) };
    assert!(matches!(doc.insert_text(across, "x"), Err(DocError::Fixed(_))));
    assert!(doc.log().is_empty());
    assert_eq!(doc.script(), Document::new(dual()).script());
}

#[test]
fn replaying_old_logs_is_not_checked_for_fixed() {
    // A log made before the rule (here: by an unfixed copy) still replays.
    let mut unfixed = dual();
    unfixed.fixed.clear();
    let mut doc = Document::new(unfixed);
    doc.enter(cur(2, 1)).unwrap();
    doc.undo();
    doc.redo();
    let replayed = Document::replay(dual(), doc.log()).unwrap();
    assert_eq!(texts(&replayed), texts(&doc));
}

#[test]
fn deleting_across_paragraphs_records_no_empty_splices() {
    use scriptorium_core::doc::{Event, Op};
    let mut doc = Document::new(script(&[("Action", "Hello"), ("Action", "Mid"), ("Action", "World")]));
    // From the end of "Hello" to the start of "World": both edge splices are empty.
    let sel = Selection { anchor: Pos::new(ParaId(0), 5), head: Pos::new(ParaId(2), 0) };
    doc.insert_text(sel, "").unwrap();
    let ops = match &doc.log()[0] {
        Event::Edit(t) => &t.ops,
        e => panic!("{e:?}"),
    };
    assert!(matches!(ops.as_slice(), [Op::RemovePara { .. }, Op::Merge { .. }]), "{ops:?}");
    assert_eq!(doc.next_undo_paras(false), [ParaId(1), ParaId(0)], "the removed middle line, then the merged one");
}

#[test]
fn removed_by_join_names_the_paragraph_a_join_key_would_take_away() {
    let s = script(&[("Action", "One."), ("Action", ""), ("Action", "Three."), ("Action", "Four.")]);
    let d = Document::new(s);
    let at = |p: u64, o: usize| Selection::cursor(Pos::new(ParaId(p), o));
    // Backspace at the start merges this paragraph into the one above...
    assert_eq!(d.removed_by_join(at(3, 0), false), Some(ParaId(3)));
    // ...unless the one above is empty: then that one goes.
    assert_eq!(d.removed_by_join(at(2, 0), false), Some(ParaId(1)));
    // Backspace in an empty paragraph after text removes it.
    assert_eq!(d.removed_by_join(at(1, 0), false), Some(ParaId(1)));
    // Delete at the end merges the next one in, or removes an empty self.
    assert_eq!(d.removed_by_join(at(2, 6), true), Some(ParaId(3)));
    assert_eq!(d.removed_by_join(at(1, 0), true), Some(ParaId(1)));
    assert_eq!(d.removed_by_join(at(0, 4), true), Some(ParaId(1)));
    // No join: mid-paragraph, the very start or end, a selection.
    assert_eq!(d.removed_by_join(at(2, 3), false), None);
    assert_eq!(d.removed_by_join(at(0, 0), false), None);
    assert_eq!(d.removed_by_join(at(3, 5), true), None);
    let sel = Selection { anchor: Pos::new(ParaId(2), 0), head: Pos::new(ParaId(2), 2) };
    assert_eq!(d.removed_by_join(sel, false), None);
    // It agrees with what the keys do.
    for (p, o, fwd) in [(3, 0, false), (2, 0, false), (1, 0, false), (2, 6, true), (1, 0, true), (0, 4, true)] {
        let mut d2 = d.clone();
        let gone = d.removed_by_join(at(p, o), fwd).unwrap();
        if fwd { d2.delete_forward(at(p, o), false) } else { d2.delete_backward(at(p, o), false) }.unwrap();
        assert!(d2.paragraph(gone).is_none(), "{p} {o} {fwd}");
    }
}
