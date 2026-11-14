use scriptorium_core::doc::Event;
use scriptorium_core::{Document, ElementKind, ParaId, Paragraph, Pos, Script, Selection, Span, Style};

fn script(paras: &[(&str, &str)]) -> Script {
    Script {
        paragraphs: paras
            .iter()
            .enumerate()
            .map(|(i, (k, t))| Paragraph::plain(ParaId(i as u64), ElementKind::from_name(k), *t))
            .collect(),
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

fn texts(doc: &Document) -> Vec<String> {
    doc.paragraphs().iter().map(|p| p.text()).collect()
}

fn ids(doc: &Document) -> Vec<u64> {
    doc.paragraphs().iter().map(|p| p.id.0).collect()
}

fn at(para: u64, offset: usize) -> Selection {
    Selection::cursor(Pos::new(ParaId(para), offset))
}

fn range(a: (u64, usize), b: (u64, usize)) -> Selection {
    Selection { anchor: Pos::new(ParaId(a.0), a.1), head: Pos::new(ParaId(b.0), b.1) }
}

#[test]
fn typing_inserts_and_keeps_ids() {
    let mut doc = Document::new(sample());
    let sel = doc.insert_text(at(3, 7), "ing").unwrap();
    assert_eq!(doc.paragraphs()[3].text(), "I am going to the town tomorrow.");
    assert_eq!(sel, at(3, 10));
    assert_eq!(ids(&doc), [0, 1, 2, 3, 4]);
}

#[test]
fn typing_takes_the_style_before_the_cursor() {
    let italic = Style { italic: true, ..Style::default() };
    let p = Paragraph {
        id: ParaId(0),
        kind: ElementKind::Action,
        spans: vec![Span { text: "a ".into(), style: Style::default() }, Span { text: "truco".into(), style: italic }],
    };
    let mut doc = Document::new(Script { paragraphs: vec![p] });
    doc.insert_text(at(0, 7), "s").unwrap();
    assert_eq!(doc.paragraphs()[0].spans[1], Span { text: "trucos".into(), style: italic });
}

#[test]
fn enter_in_the_middle_splits_and_the_tail_gets_a_new_id() {
    let mut doc = Document::new(sample());
    let sel = doc.enter(at(1, 16)).unwrap();
    assert_eq!(texts(&doc)[1..3], ["They are playing", " truco."]);
    assert_eq!(ids(&doc), [0, 1, 5, 2, 3, 4]);
    assert_eq!(doc.paragraphs()[2].kind, ElementKind::Action, "a mid split keeps the element");
    assert_eq!(sel, at(5, 0));
}

#[test]
fn enter_at_the_end_follows_the_element_rules() {
    let mut doc = Document::new(sample());
    doc.enter(at(2, 5)).unwrap();
    assert_eq!(doc.paragraphs()[3].kind, ElementKind::Dialogue, "Character → Dialogue");
    assert_eq!(doc.paragraphs()[3].text(), "");
}

#[test]
fn enter_at_the_start_inserts_above_so_the_text_keeps_its_id() {
    let mut doc = Document::new(sample());
    let sel = doc.enter(at(1, 0)).unwrap();
    assert_eq!(texts(&doc)[1..3], ["", "They are playing truco."]);
    assert_eq!(ids(&doc), [0, 5, 1, 2, 3, 4]);
    assert_eq!(sel, at(1, 0), "cursor stays with the text");
}

#[test]
fn backspace_at_start_joins_and_undo_restores_ids() {
    let mut doc = Document::new(sample());
    let sel = doc.delete_backward(at(1, 0), false).unwrap();
    assert_eq!(texts(&doc)[0], "EXT. FARM HOUSE - NIGHTThey are playing truco.");
    assert_eq!(sel, at(0, 23));
    assert_eq!(ids(&doc), [0, 2, 3, 4]);
    assert_eq!(doc.undo(), Some(at(1, 0)));
    assert_eq!(doc.script(), &Document::new(sample()).script().clone());
}

#[test]
fn removing_an_empty_paragraph_never_costs_the_neighbour_its_id() {
    let mut doc = Document::new(sample());
    doc.enter(at(1, 0)).unwrap(); // empty paragraph 5 above paragraph 1
    let sel = doc.delete_backward(at(1, 0), false).unwrap();
    assert_eq!(ids(&doc), [0, 1, 2, 3, 4]);
    assert_eq!(sel, at(1, 0));
    assert_eq!(doc.paragraphs()[1].kind, ElementKind::Action);
}

#[test]
fn deleting_across_paragraphs_joins_the_ends() {
    let mut doc = Document::new(sample());
    let sel = doc.delete_backward(range((1, 9), (3, 5)), false).unwrap();
    assert_eq!(texts(&doc), ["EXT. FARM HOUSE - NIGHT", "They are go to the town tomorrow.", "Laura smiles."]);
    assert_eq!(sel, at(1, 9));
    doc.undo();
    assert_eq!(texts(&doc), texts(&Document::new(sample())));
    assert_eq!(ids(&doc), [0, 1, 2, 3, 4]);
}

#[test]
fn pasting_lines_makes_paragraphs() {
    let mut doc = Document::new(sample());
    let sel = doc.insert_text(at(4, 13), "\nShe leaves.\r\nThe end.").unwrap();
    assert_eq!(texts(&doc)[4..], ["Laura smiles.", "She leaves.", "The end."]);
    assert_eq!(sel.head.offset, 8);
    assert_eq!(doc.undo(), Some(at(4, 13)));
    assert_eq!(doc.paragraphs().len(), 5, "a paste is one undo step");
}

#[test]
fn copying_text_across_paragraphs() {
    let doc = Document::new(sample());
    assert_eq!(doc.text_in(range((3, 19), (2, 2))), "SAR\nI am go to the town");
}

#[test]
fn tab_cycles_kind_and_set_kind_covers_the_selection() {
    let mut doc = Document::new(sample());
    doc.cycle_kind(at(1, 0), true).unwrap();
    assert_eq!(doc.paragraphs()[1].kind, ElementKind::Character);
    doc.set_kind(range((3, 2), (4, 1)), ElementKind::Shot).unwrap();
    assert_eq!(doc.paragraphs()[3].kind, ElementKind::Shot);
    assert_eq!(doc.paragraphs()[4].kind, ElementKind::Shot);
}

#[test]
fn typing_undoes_word_by_word_and_redo_reapplies() {
    let mut doc = Document::new(sample());
    let mut sel = at(4, 13);
    for ch in [" ", "S", "h", "e", " ", "w", "a", "v", "e", "s"] {
        sel = doc.insert_text(sel, ch).unwrap();
    }
    assert_eq!(doc.paragraphs()[4].text(), "Laura smiles. She waves");
    doc.undo();
    assert_eq!(doc.paragraphs()[4].text(), "Laura smiles. She");
    doc.undo();
    assert_eq!(doc.paragraphs()[4].text(), "Laura smiles.");
    assert!(!doc.can_undo());
    doc.redo();
    doc.redo();
    assert_eq!(doc.paragraphs()[4].text(), "Laura smiles. She waves");
    // A new edit after undo drops the redo branch.
    doc.undo();
    doc.insert_text(at(4, 17), "!").unwrap();
    assert!(!doc.can_redo());
}

#[test]
fn word_movement_and_word_selection() {
    let doc = Document::new(sample());
    assert_eq!(doc.move_right(Pos::new(ParaId(3), 0), true), Pos::new(ParaId(3), 1));
    assert_eq!(doc.move_right(Pos::new(ParaId(3), 1), true), Pos::new(ParaId(3), 4));
    assert_eq!(doc.move_left(Pos::new(ParaId(3), 7), true), Pos::new(ParaId(3), 5));
    assert_eq!(doc.move_left(Pos::new(ParaId(3), 0), false), Pos::new(ParaId(2), 5), "crosses paragraphs");
    assert_eq!(doc.word_at(Pos::new(ParaId(1), 18)), range((1, 17), (1, 22)));
}

#[test]
fn accents_move_by_grapheme() {
    let mut doc = Document::new(script(&[("Action", "")]));
    let sel = doc.insert_text(at(0, 0), "Señor é").unwrap();
    assert_eq!(sel, at(0, 9));
    assert_eq!(doc.move_left(sel.head, false), Pos::new(ParaId(0), 7));
    // A combining accent (e + U+0301) is one grapheme.
    let sel = doc.insert_text(sel, " e\u{301}").unwrap();
    assert_eq!(doc.move_left(sel.head, false).offset, sel.head.offset - 3);
    let sel = doc.delete_backward(sel, false).unwrap();
    assert_eq!(doc.paragraphs()[0].text(), "Señor é ");
    assert_eq!(sel.head.offset, 10);
}

#[test]
fn clamping_bad_positions() {
    let doc = Document::new(script(&[("Action", "ñ")]));
    assert_eq!(doc.clamp(Pos::new(ParaId(0), 1)), Pos::new(ParaId(0), 0), "inside a multi-byte char");
    assert_eq!(doc.clamp(Pos::new(ParaId(9), 3)), Pos::new(ParaId(0), 0), "missing paragraph");
}

/// Tiny deterministic PRNG so the fuzz test needs no dependencies.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn random_pos(doc: &Document, rng: &mut Lcg) -> Pos {
    let p = &doc.paragraphs()[rng.below(doc.paragraphs().len())];
    doc.clamp(Pos::new(p.id, rng.below(p.text().len() + 1)))
}

#[test]
fn fuzz_undo_all_restores_the_original_and_replay_matches() {
    for seed in 0..40 {
        let mut rng = Lcg(seed);
        let mut doc = Document::new(sample());
        let original = doc.script().clone();
        for _ in 0..150 {
            let a = random_pos(&doc, &mut rng);
            let sel = if rng.below(3) == 0 {
                Selection { anchor: random_pos(&doc, &mut rng), head: a }
            } else {
                Selection::cursor(a)
            };
            let result = match rng.below(9) {
                0 | 1 => doc.insert_text(sel, ["x", "ñ", " ", "e\u{301}", "Hola"][rng.below(5)]),
                2 => doc.insert_text(sel, "one\ntwo\n\nthree"),
                3 => doc.enter(sel),
                4 => doc.delete_backward(sel, rng.below(2) == 0),
                5 => doc.delete_forward(sel, rng.below(2) == 0),
                6 => doc.cycle_kind(sel, rng.below(2) == 0),
                7 => Ok(doc.undo().unwrap_or(sel)),
                _ => Ok(doc.redo().unwrap_or(sel)),
            };
            result.unwrap_or_else(|e| panic!("seed {seed}: {e}"));
            let mut seen = std::collections::HashSet::new();
            assert!(doc.paragraphs().iter().all(|p| seen.insert(p.id)), "seed {seed}: duplicate ids");
        }
        let replayed = Document::replay(original.clone(), doc.log()).unwrap();
        assert_eq!(replayed.script(), doc.script(), "seed {seed}: replay differs");
        while doc.undo().is_some() {}
        assert_eq!(doc.script(), &original, "seed {seed}: undo-all differs");
        assert!(doc.log().iter().any(|e| matches!(e, Event::Undo)));
    }
}

// ---- replace_text and metadata ------------------------------------------

#[test]
fn replace_text_with_meta_is_one_undo_step() {
    let mut doc = Document::new(sample());
    let sel = Selection::cursor(Pos::new(ParaId(3), 0));
    let after = doc
        .replace_text(sel, ParaId(3), "I am going to the town tomorrow.", &[("review/0/1", Some("accepted:0"))])
        .unwrap();
    assert_eq!(texts(&doc)[3], "I am going to the town tomorrow.");
    assert_eq!(doc.meta("review/0/1"), Some("accepted:0"));
    assert_eq!(after.head, Pos::new(ParaId(3), "I am going".len()));
    doc.undo();
    assert_eq!(texts(&doc)[3], "I am go to the town tomorrow.");
    assert_eq!(doc.meta("review/0/1"), None);
    doc.redo();
    assert_eq!(texts(&doc)[3], "I am going to the town tomorrow.");
    assert_eq!(doc.meta("review/0/1"), Some("accepted:0"));
}

#[test]
fn replace_text_meta_only_and_no_op() {
    let mut doc = Document::new(sample());
    let sel = Selection::cursor(doc.start());
    let same = texts(&doc)[1].clone();
    doc.replace_text(sel, ParaId(1), &same, &[("k", Some("rejected"))]).unwrap();
    assert_eq!(doc.meta("k"), Some("rejected"));
    assert_eq!(doc.log().len(), 1);
    // Nothing changes: nothing logged.
    doc.replace_text(sel, ParaId(1), &same, &[("k", Some("rejected"))]).unwrap();
    assert_eq!(doc.log().len(), 1);
    doc.replace_text(sel, ParaId(1), &same, &[("k", None)]).unwrap();
    assert_eq!(doc.meta("k"), None);
}

#[test]
fn replace_text_keeps_styles_outside_the_change() {
    let italic = Style { italic: true, ..Style::default() };
    let s = Script {
        paragraphs: vec![Paragraph {
            id: ParaId(0),
            kind: ElementKind::Action,
            spans: vec![
                Span { text: "They play ".into(), style: Style::default() },
                Span { text: "truco".into(), style: italic },
                Span { text: " (cards game).".into(), style: Style::default() },
            ],
        }],
    };
    let mut doc = Document::new(s);
    let sel = Selection::cursor(doc.start());
    doc.replace_text(sel, ParaId(0), "They play truco (a card game).", &[]).unwrap();
    let p = &doc.paragraphs()[0];
    assert_eq!(p.text(), "They play truco (a card game).");
    assert!(p.spans.iter().any(|s| s.text == "truco" && s.style == italic));
}

#[test]
fn replace_text_multibyte() {
    let mut doc = Document::new(script(&[("Action", "Córdoba, señor.")]));
    let sel = Selection::cursor(doc.start());
    doc.replace_text(sel, ParaId(0), "Córdoba, señora.", &[]).unwrap();
    assert_eq!(texts(&doc), ["Córdoba, señora."]);
    doc.replace_text(sel, ParaId(0), "Cordoba", &[]).unwrap();
    assert_eq!(texts(&doc), ["Cordoba"]);
}

#[test]
fn rebase_and_replay_with_meta() {
    let mut doc = Document::new(sample());
    let sel = Selection::cursor(doc.start());
    doc.replace_text(sel, ParaId(1), "Restored.", &[("r", Some("accepted:0"))]).unwrap();
    let base = doc.rebase();
    assert!(!base.can_undo());
    assert_eq!(base.meta("r"), Some("accepted:0"));
    let mut live = base.clone();
    live.replace_text(sel, ParaId(4), "Laura grins.", &[("s", Some("rewritten"))]).unwrap();
    live.undo();
    live.redo();
    let replayed = base.replay_onto(live.log()).unwrap();
    assert_eq!(texts(&replayed), texts(&live));
    assert_eq!(replayed.all_meta(), live.all_meta());
    assert!(matches!(live.log().last(), Some(Event::Redo)));
}

#[test]
fn merge_undo_makes_one_step_and_replays() {
    let mut doc = Document::new(sample());
    let start = doc.undo_depth();
    let mut sel = Selection::cursor(Pos::new(ParaId(4), "Laura smiles".len()));
    for c in [" ", "w", "i", "d", "e", "l", "y"] {
        sel = doc.insert_text(sel, c).unwrap();
    }
    doc.replace_text(sel, ParaId(4), "Laura smiles widely.", &[("k", Some("rewritten"))]).unwrap();
    assert!(doc.undo_depth() > start + 1);
    assert!(doc.next_undo_paras(false).is_empty(), "the last step only set the decision");
    doc.merge_undo_since(start);
    assert_eq!(doc.undo_depth(), start + 1);
    assert_eq!(doc.next_undo_paras(false), [ParaId(4)]);
    let replayed = Document::replay(sample(), doc.log()).unwrap();
    assert_eq!(replayed.undo_depth(), doc.undo_depth());
    doc.undo();
    assert_eq!(texts(&doc)[4], "Laura smiles.");
    assert_eq!(doc.meta("k"), None);
    doc.redo();
    assert_eq!(texts(&doc)[4], "Laura smiles widely.");
}
