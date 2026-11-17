//! Block suggestions (B5): `paragraph_ids`, `insert_after`, typed options and
//! cuts, their validation, `apply`, and how `list_suggestions` shows them.

use scriptorium_core::{Document, ElementKind, FIXED_MESSAGE, Group, NewPara, ParaId, Selection};
use scriptorium_fdx::FdxFile;
use scriptorium_review::{Decision, Line, Review, Session};
use scriptorium_tools::{Append, Applied, Ctx, MAX_LINES, Outcome, Spot, ToolError, apply, block_entry, call};
use serde_json::{Value, json};

// ¶0 heading, ¶1 action, ¶2 cue, ¶3 parenthetical, ¶4 dialogue (has a
// suggestion), ¶5 End of Act, ¶6 heading, ¶7 action, ¶8 cue, ¶9 dialogue.
const SCRIPT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Scene Heading"><Text>EXT. FARM HOUSE - NIGHT</Text></Paragraph>
    <Paragraph Type="Action"><Text>Laura smiles.</Text></Paragraph>
    <Paragraph Type="Character"><Text>Cesar</Text></Paragraph>
    <Paragraph Type="Parenthetical"><Text>(quietly)</Text></Paragraph>
    <Paragraph Type="Dialogue"><Text>I am go to the town tomorrow.</Text></Paragraph>
    <Paragraph Type="End of Act"><Text>END OF ACT ONE</Text></Paragraph>
    <Paragraph Type="Scene Heading"><Text>INT. KITCHEN - DAY</Text></Paragraph>
    <Paragraph Type="Action"><Text>Rain on the window.</Text></Paragraph>
    <Paragraph Type="Character"><Text>LAURA</Text></Paragraph>
    <Paragraph Type="Dialogue"><Text>Thanks, really.</Text></Paragraph>
  </Content>
</FinalDraft>
"#;

const REVIEW: &str = "SCRIPT REVIEW v1
Script: film.fdx
Id: a

---
PARA: 4
BEFORE: I am go to the town tomorrow.
AFTER: I am going to town tomorrow.
";

struct F {
    fdx: FdxFile,
    doc: Document,
    session: Session,
}

impl F {
    fn new() -> Self {
        let fdx = FdxFile::parse(SCRIPT.to_string()).unwrap();
        let doc = Document::new(fdx.to_script());
        let review = Review::open("a.review.txt", REVIEW, &fdx.to_script(), |k| fdx.is_caps(k.name()), "film.fdx")
            .with_path("a.review.txt");
        Self { fdx, doc, session: Session::new(vec![review]) }
    }

    fn call(&self, name: &str, args: Value) -> Result<Outcome, ToolError> {
        let is_caps = |k: &ElementKind| self.fdx.is_caps(k.name());
        let ctx = Ctx {
            doc: &self.doc,
            session: &self.session,
            script_name: "film.fdx",
            is_caps: &is_caps,
            default_review: Some(0),
            links: None,
        };
        call(&ctx, name, &args)
    }

    fn append(&self, args: Value) -> Append {
        match self.call("suggest_change", args).unwrap_or_else(|e| panic!("{e}")) {
            Outcome::Append(a) => a,
            o => panic!("{o:?}"),
        }
    }

    fn err(&self, args: Value) -> String {
        match self.call("suggest_change", args) {
            Ok(o) => panic!("succeeded: {o:?}"),
            Err(e) => e.to_string(),
        }
    }

    fn apply(&mut self, a: Append) -> Applied {
        let fdx = &self.fdx;
        apply(&mut self.session, &self.doc, 0, a, &|k: &ElementKind| fdx.is_caps(k.name()), None).unwrap()
    }

    fn list(&self) -> Value {
        match self.call("list_suggestions", json!({})).unwrap() {
            Outcome::Text(t) => serde_json::from_str(&t).unwrap(),
            o => panic!("{o:?}"),
        }
    }

    /// Insert an empty Action after `after`; returns its id.
    fn insert_empty_after(&mut self, after: u64) -> u64 {
        let at = self.doc.index_of(ParaId(after)).unwrap() + 1;
        let sel = Selection::cursor(self.doc.start());
        self.doc
            .edit(sel, Group::Other, |e| {
                let id = e.insert_para(at, NewPara::plain(ElementKind::Action, ""))?;
                Ok((sel, id))
            })
            .unwrap()
            .0
    }
}

fn l(kind: ElementKind, text: &str) -> Line {
    Line::typed(kind, text)
}

#[test]
fn a_range_replaces_the_lines_as_one_suggestion() {
    let mut f = F::new();
    let a = f.append(json!({
        "paragraph_ids": [8, 9],
        "options": [[{ "kind": "Character", "text": "LAURA" }, { "kind": "dialogue", "text": "Thank you." }]],
        "why": "warmer",
    }));
    let e = &a.proposals[0].entry;
    assert_eq!(e.para, [ParaId(8), ParaId(9)]);
    assert_eq!(e.before, [l(ElementKind::Character, "LAURA"), l(ElementKind::Dialogue, "Thanks, really.")]);
    assert_eq!(e.options, [vec![l(ElementKind::Character, "LAURA"), l(ElementKind::Dialogue, "Thank you.")]]);
    let applied = f.apply(a);
    assert_eq!(applied.message, "added suggestion #2 (e1, ¶8–¶9) to a.review.txt");
    let text = applied.file_text.unwrap();
    assert!(text.starts_with("SCRIPT REVIEW v2"), "{text}");
    assert!(text.contains("PARA: 8, 9"), "{text}");

    let rows = f.list();
    assert_eq!(rows[1]["shape"], "block");
    assert_eq!(rows[1]["paragraph_ids"], json!([8, 9]));
    assert_eq!(
        rows[1]["before"],
        json!([{ "kind": "Character", "text": "LAURA" }, { "kind": "Dialogue", "text": "Thanks, really." }])
    );
    assert_eq!(rows[1]["current"], rows[1]["before"]);
    assert_eq!(rows[1]["options"][0][1], json!({ "kind": "Dialogue", "text": "Thank you." }));
    // The line row keeps its strings.
    assert_eq!(rows[0]["shape"], "line");
    assert_eq!(rows[0]["options"], json!(["I am going to town tomorrow."]));
}

#[test]
fn a_full_list_and_a_single_paragraph_block() {
    let f = F::new();
    let a = f.append(json!({
        "paragraph_ids": [6, 7, 8],
        "options": [[{ "kind": "Scene Heading", "text": "INT. KITCHEN - NIGHT" }]],
    }));
    assert_eq!(a.proposals[0].entry.para, [ParaId(6), ParaId(7), ParaId(8)]);
    // One paragraph turned into two lines.
    let a = f.append(json!({
        "paragraph_id": 1,
        "options": [[{ "kind": "Action", "text": "Laura smiles." }, { "kind": "Action", "text": "Then frowns." }]],
    }));
    assert_eq!(a.proposals[0].entry.options[0].len(), 2);
    // A one-id list is a one-line block.
    let a = f.append(json!({ "paragraph_ids": [1], "options": [[{ "kind": "Dialogue", "text": "Laura smiles." }]] }));
    assert!(!a.proposals[0].entry.is_line(), "a retype is a block");
}

#[test]
fn insert_after_adds_lines_after_an_unchanged_anchor() {
    let mut f = F::new();
    let a = f.append(json!({ "insert_after": 7, "options": [[{ "kind": "Action", "text": "Thunder." }]] }));
    let e = &a.proposals[0].entry;
    assert_eq!(e.para, [ParaId(7)]);
    assert_eq!(e.options, [vec![l(ElementKind::Action, "Rain on the window."), l(ElementKind::Action, "Thunder.")]]);
    let applied = f.apply(a);
    assert_eq!(applied.message, "added suggestion #2 (e1, after ¶7) to a.review.txt");
    let rows = f.list();
    assert_eq!(rows[1]["shape"], "insert");
    assert_eq!(rows[1]["paragraph_ids"], json!([7]));
}

#[test]
fn an_empty_option_cuts_and_can_be_accepted() {
    let mut f = F::new();
    let a = f.append(json!({ "paragraph_ids": [1], "options": [[]], "why": "redundant" }));
    assert_eq!(a.proposals[0].entry.options, [Vec::<Line>::new()]);
    let applied = f.apply(a);
    assert!(applied.file_text.unwrap().contains("AFTER: [cut]"));
    let rows = f.list();
    assert_eq!(rows[0]["shape"], "block");
    assert_eq!(rows[0]["options"], json!([[]]));
    let i = f.session.reviews[0].items.iter().position(|it| it.block).unwrap();
    let sel = Selection::cursor(f.doc.start());
    f.session.decide(&mut f.doc, sel, 0, i, Decision::Accept(0)).unwrap();
    assert!(f.doc.paragraph(ParaId(1)).is_none());
    assert_eq!(f.list()[0]["current"], json!([]));
}

#[test]
fn block_refusals() {
    let f = F::new();
    let line = |k: &str, t: &str| json!({ "kind": k, "text": t });
    let one = |t: &str| json!([[line("Action", t)]]);
    assert!(
        f.err(json!({ "paragraph_ids": [8, 9], "options": ["x"] })).contains("give each option as a list of lines")
    );
    assert!(f.err(json!({ "insert_after": 7, "options": ["x"] })).contains("list of lines"));
    assert!(f.err(json!({ "insert_after": 7, "options": [[]] })).starts_with("insert_after adds lines"));
    assert!(f.err(json!({ "paragraph_id": 1, "options": ["x", [line("Action", "y")]] })).contains("every option"));
    assert!(f.err(json!({ "paragraph_id": 1, "options": [[line("Montage", "y")]] })).contains("unknown element type"));
    assert!(f.err(json!({ "paragraph_id": 1, "options": [[{ "kind": "Action" }]] })).contains("`text`"));
    assert_eq!(f.err(json!({ "paragraph_id": 1, "options": one("  ") })), "lines can't be empty");
    assert!(f.err(json!({ "paragraph_id": 1, "options": one("a\nb") })).contains("newline"));
    let many: Vec<Value> = (0..=MAX_LINES).map(|i| line("Action", &format!("Line {i}."))).collect();
    assert_eq!(f.err(json!({ "paragraph_id": 1, "options": [many] })), "at most 40 lines per option, not 41");
    assert_eq!(f.err(json!({ "paragraph_id": 1, "options": [[], [], [], []] })), "give at most 3 options, not 4");
    // The same lines (letter case in a cue doesn't show) change nothing.
    assert_eq!(
        f.err(json!({ "paragraph_ids": [8, 9], "options": [[line("Character", "laura"), line("Dialogue", "Thanks, really.")]] })),
        "that change wouldn't be visible in the script"
    );
    // Targets.
    assert_eq!(
        f.err(json!({ "paragraph_id": 1, "insert_after": 1, "options": [[]] })),
        "give exactly one of paragraph_id, paragraph_ids or insert_after"
    );
    assert_eq!(f.err(json!({ "paragraph_ids": [], "options": [[]] })), "`paragraph_ids` is empty");
    assert!(f.err(json!({ "paragraph_ids": [9, 8], "options": [[]] })).contains("comes before"));
    assert!(f.err(json!({ "paragraph_ids": [6, 8, 9], "options": [[]] })).contains("¶7 between them"));
    assert!(f.err(json!({ "paragraph_ids": [8, 7, 9], "options": [[]] })).contains("comes before"));
    assert!(f.err(json!({ "paragraph_ids": [7, 7, 8], "options": [[]] })).contains("listed twice"));
    assert_eq!(f.err(json!({ "paragraph_ids": [3, 99], "options": [[]] })), "no paragraph 99");
    assert_eq!(f.err(json!({ "paragraph_ids": [5, 6], "options": [[]] })), "paragraph 5 (End of Act) isn't editable");
    // Overlapping the review's suggestion on ¶4.
    assert!(
        f.err(json!({ "paragraph_ids": [2, 3, 4], "options": [[]] }))
            .starts_with("this conversation's review already has a suggestion on ¶4 (#1, pending)")
    );
    let ids: Vec<u64> = (0..=MAX_LINES as u64).collect();
    assert_eq!(f.err(json!({ "paragraph_ids": ids, "options": [[]] })), "at most 40 lines per suggestion, not 41");
}

#[test]
fn a_batch_mixes_shapes_and_refuses_overlaps() {
    let mut f = F::new();
    let a = match f
        .call(
            "suggest_changes",
            json!({ "suggestions": [
                { "paragraph_ids": [8, 9], "options": [[]] },
                { "paragraph_id": 9, "options": ["Thanks."] },
                { "insert_after": 7, "options": [[{ "kind": "Action", "text": "Thunder." }]] },
                { "paragraph_id": 1, "options": ["Laura grins."] },
            ]}),
        )
        .unwrap()
    {
        Outcome::Append(a) => a,
        o => panic!("{o:?}"),
    };
    assert_eq!(a.failed.len(), 1);
    assert_eq!(a.failed[0].error, "paragraph 9 is already suggested on by item 0 of this batch");
    let applied = f.apply(a);
    let result: Value = serde_json::from_str(&applied.message).unwrap();
    assert_eq!(result["added"], 3);
    let shapes: Vec<&str> =
        result["suggestions"].as_array().unwrap().iter().map(|s| s["shape"].as_str().unwrap()).collect();
    assert_eq!(shapes, ["block", "insert", "line"]);
    assert_eq!(result["suggestions"][0]["paragraph_ids"], json!([8, 9]));
    assert!(result["suggestions"][2].get("paragraph_ids").is_none());
}

#[test]
fn empty_lines_are_trimmed_at_the_edges_and_must_be_named_inside() {
    let mut f = F::new();
    let gap = f.insert_empty_after(8);
    let err = f.err(json!({ "paragraph_ids": [8, 9], "options": [[]] }));
    assert!(err.contains("aren't next to each other") && err.contains(&format!("¶{gap} between them")), "{err}");
    let a = f.append(json!({ "paragraph_ids": [8, gap, 9], "options": [[]] }));
    assert_eq!(a.proposals[0].entry.para, [ParaId(8), ParaId(gap), ParaId(9)]);
    let applied = f.apply(a);
    assert!(applied.message.starts_with("added suggestion #2"), "{}", applied.message);

    let mut f = F::new();
    let gap = f.insert_empty_after(8);
    let a = f.append(json!({ "paragraph_ids": [7, 8, gap], "options": [[]] }));
    assert_eq!(a.proposals[0].entry.para, [ParaId(7), ParaId(8), ParaId(gap)], "named empty edges are kept");
    assert_eq!(f.err(json!({ "paragraph_ids": [gap], "options": [[]] })), "every listed paragraph is empty");
}

#[test]
fn apply_rechecks_that_the_run_still_reads_as_before() {
    let mut f = F::new();
    let a = f.append(json!({ "paragraph_ids": [8, 9], "options": [[]] }));
    let sel = Selection::cursor(f.doc.start());
    f.doc.replace_text(sel, ParaId(9), "Thanks a lot.", &[]).unwrap();
    let fdx = &f.fdx;
    let err = apply(&mut f.session, &f.doc, 0, a, &|k: &ElementKind| fdx.is_caps(k.name()), None).unwrap_err();
    assert_eq!(
        err.to_string(),
        "¶8–¶9 changed since the suggestion was made: read them again and suggest on their current text"
    );
    assert_eq!(f.session.reviews[0].items.len(), 1);
}

#[test]
fn dual_dialogue_lines_take_text_changes_only() {
    let fdx = FdxFile::parse(SCRIPT.to_string()).unwrap();
    let mut script = fdx.to_script();
    script.fixed.insert(ParaId(8), 1);
    script.fixed.insert(ParaId(9), 1);
    let doc = Document::new(script);
    let caps = |k: &ElementKind| fdx.is_caps(k.name());
    let typed = |k: ElementKind, t: &str| vec![l(k, t)];
    let cut = block_entry(&doc, &caps, &Spot::Range(8, 9), vec![vec![]], "");
    assert_eq!(cut.unwrap_err(), FIXED_MESSAGE);
    let add = block_entry(&doc, &caps, &Spot::InsertAfter(8), vec![typed(ElementKind::Dialogue, "Hm.")], "");
    assert_eq!(add.unwrap_err(), FIXED_MESSAGE);
    let text = vec![l(ElementKind::Character, "LAURA"), l(ElementKind::Dialogue, "Thanks, really!")];
    assert!(block_entry(&doc, &caps, &Spot::Range(8, 9), vec![text], "").is_ok());
    // After the block's last line is outside it.
    assert!(block_entry(&doc, &caps, &Spot::InsertAfter(9), vec![typed(ElementKind::Action, "Rain.")], "").is_ok());
}

#[test]
fn two_ids_are_exactly_two_lines_not_a_range() {
    let f = F::new();
    // ¶6 and ¶9 would have been a range; now they are two lines that aren't next to each other.
    let e = f.err(json!({ "paragraph_ids": [6, 9], "options": [[]] }));
    assert!(e.contains("aren't next to each other") && e.contains("¶7, ¶8"), "{e}");
    let a = f.append(json!({ "paragraph_ids": [8, 9], "options": [[]] }));
    assert_eq!(a.proposals[0].entry.para, vec![ParaId(8), ParaId(9)]);
}

#[test]
fn cut_wording_is_the_same_everywhere() {
    let f = F::new();
    let e = f.err(json!({ "paragraph_id": 1, "options": [] }));
    assert!(e.contains("an option `[]` cuts the lines"), "{e}");
    let e = f.err(json!({ "insert_after": 1, "options": [[]] }));
    assert!(e.contains("an option `[]`"), "{e}");
}

#[test]
fn read_scene_marks_dual_dialogue_lines() {
    let fdx = FdxFile::parse(SCRIPT.to_string()).unwrap();
    let mut script = fdx.to_script();
    script.fixed.insert(ParaId(8), 1);
    script.fixed.insert(ParaId(9), 1);
    let f = F { doc: Document::new(script), fdx, session: F::new().session };
    let Outcome::Text(t) = f.call("read_scene", json!({ "scene": 2 })).unwrap() else { panic!() };
    assert!(t.contains("¶8 LAURA [dual dialogue]") && t.contains("¶9   Thanks, really. [dual dialogue]"), "{t}");
    assert!(!t.contains("¶7 Rain on the window. [dual"), "{t}");
}

#[test]
fn a_range_over_a_hidden_empty_line_names_the_cli_flag() {
    let mut f = F::new();
    let gap = f.insert_empty_after(8);
    let caps = |_: &ElementKind| false;
    let e = block_entry(&f.doc, &caps, &Spot::Range(8, 9), vec![vec![]], "").unwrap_err();
    assert!(e.contains(&format!("--paras 8,{gap},9")) && !e.contains("read_scene"), "{e}");
}
