//! Every tool over a small hand-written script and one attached review, in
//! memory (no files).

use scriptorium_core::{Document, ElementKind, ParaId, Selection};
use scriptorium_fdx::FdxFile;
use scriptorium_review::{Decision, Line, Review, Session};
use scriptorium_tools::prompt::{SYSTEM, system, unknown_tools};
use scriptorium_tools::{
    Append, Applied, Ctx, Failed, HostField, MAX_BATCH, Outcome, Proposal, Target, ToolError, add_before,
    add_host_fields, apply, call, catalogue, check_args, find_review, item_numbered, item_numbers, may_append,
    script_size,
};
use serde_json::{Value, json};

const SCRIPT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Scene Heading">
      <Text>EXT. FARM HOUSE - NIGHT</Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text>Laura smiles.</Text>
    </Paragraph>
    <Paragraph Type="Character">
      <Text>Cesar</Text>
    </Paragraph>
    <Paragraph Type="Parenthetical">
      <Text>(quietly)</Text>
    </Paragraph>
    <Paragraph Type="Dialogue">
      <Text>I am go to the town tomorrow.</Text>
    </Paragraph>
    <Paragraph Type="End of Act">
      <Text>END OF ACT ONE</Text>
    </Paragraph>
    <Paragraph Type="Scene Heading">
      <Text>int. kitchen - day</Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text>Rain on the window.</Text>
    </Paragraph>
    <Paragraph Type="Character">
      <Text>LAURA</Text>
    </Paragraph>
    <Paragraph Type="Dialogue">
      <Text>Thanks, really.</Text>
    </Paragraph>
  </Content>
</FinalDraft>
"#;

const REVIEW: &str = "SCRIPT REVIEW v1
Script: film.fdx

---
BEFORE: I am go to the town tomorrow.
AFTER: I am going to town tomorrow.
WHY: verb form
---
BEFORE: This line is not in the script at all.
AFTER: Nor is this one.
";

struct Fixture {
    fdx: FdxFile,
    doc: Document,
    session: Session,
    default_review: Option<usize>,
}

impl Fixture {
    /// The script with `a.review.txt` attached and the default review.
    fn new() -> Self {
        let mut f = Self::bare();
        let review = Review::open("a.review.txt", REVIEW, &f.fdx.to_script(), |k| f.fdx.is_caps(k.name()), "film.fdx")
            .with_path("a.review.txt");
        f.session = Session::new(vec![review]);
        f.default_review = Some(0);
        f
    }

    /// The script with no reviews.
    fn bare() -> Self {
        let fdx = FdxFile::parse(SCRIPT.to_string()).unwrap();
        let doc = Document::new(fdx.to_script());
        Self { fdx, doc, session: Session::default(), default_review: None }
    }

    fn call(&self, name: &str, args: Value) -> Result<Outcome, ToolError> {
        let is_caps = |k: &ElementKind| self.fdx.is_caps(k.name());
        let ctx = Ctx {
            doc: &self.doc,
            session: &self.session,
            script_name: "film.fdx",
            is_caps: &is_caps,
            default_review: self.default_review,
        };
        call(&ctx, name, &args)
    }

    /// The final text of a read tool.
    fn text(&self, name: &str, args: Value) -> String {
        match self.call(name, args).unwrap_or_else(|e| panic!("{name}: {e}")) {
            Outcome::Text(t) => t,
            o => panic!("{name} has effects: {o:?}"),
        }
    }

    fn json(&self, name: &str, args: Value) -> Value {
        serde_json::from_str(&self.text(name, args)).unwrap()
    }

    fn err(&self, name: &str, args: Value) -> String {
        match self.call(name, args) {
            Ok(o) => panic!("{name} succeeded: {o:?}"),
            Err(e) => e.to_string(),
        }
    }

    /// The single append effect of a suggest call.
    fn append(&self, name: &str, args: Value) -> Append {
        match self.call(name, args).unwrap_or_else(|e| panic!("{name}: {e}")) {
            Outcome::Append(a) => a,
            o => panic!("{name} has no append: {o:?}"),
        }
    }

    fn try_apply(&mut self, r: usize, a: Append) -> Result<Applied, ToolError> {
        let fdx = &self.fdx;
        apply(&mut self.session, &self.doc, r, a, &|k: &ElementKind| fdx.is_caps(k.name()))
    }

    fn apply(&mut self, r: usize, a: Append) -> Applied {
        self.try_apply(r, a).unwrap()
    }

    /// What an agent host does for `Target::Own`: create an empty review
    /// (with its `Id:`) and attach it. Returns its index.
    fn attach_own(&mut self) -> usize {
        let text = "SCRIPT REVIEW v1\nScript: film.fdx\nId: thread-1\n";
        let fdx = &self.fdx;
        let review = Review::open_live(
            "thread.review.txt",
            text,
            &fdx.to_script(),
            &self.doc,
            |k| fdx.is_caps(k.name()),
            "film.fdx",
        )
        .with_path("thread.review.txt");
        self.session.add(review).unwrap()
    }

    /// `Target::Review` for review 0, as a call sees it.
    fn own_target(&self) -> Target {
        Target::Review { index: 0, id: self.session.reviews[0].id.clone() }
    }

    fn is_caps(&self) -> impl Fn(&ElementKind) -> bool + '_ {
        |k: &ElementKind| self.fdx.is_caps(k.name())
    }

    fn accept_first(&mut self) {
        let sel = Selection::cursor(self.doc.start());
        self.session.decide(&mut self.doc, sel, 0, 0, Decision::Accept(0)).unwrap();
    }
}

#[test]
fn catalogue_lists_every_tool_with_base_schemas() {
    let specs = catalogue();
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "script_info",
            "list_paragraphs",
            "read_scene",
            "read_script",
            "list_reviews",
            "list_suggestions",
            "suggest_change",
            "suggest_changes"
        ]
    );
    for s in &specs {
        assert_eq!(s.schema["type"], "object", "{}", s.name);
        assert!(s.schema["properties"].get("script").is_none(), "{} has no `script`", s.name);
        assert!(!s.description.is_empty());
    }
    let read_scene = specs.iter().find(|s| s.name == "read_scene").unwrap();
    assert!(read_scene.description.contains("paragraph_id is the number after ¶"));
    let suggest = specs.iter().find(|s| s.name == "suggest_change").unwrap();
    assert_eq!(suggest.schema["properties"]["options"]["maxItems"], 3);
    let batch = specs.iter().find(|s| s.name == "suggest_changes").unwrap();
    assert_eq!(batch.schema["properties"]["suggestions"]["maxItems"], MAX_BATCH);
}

#[test]
fn unknown_tool_and_bad_arguments() {
    let f = Fixture::new();
    assert_eq!(f.call("nope", json!({})).unwrap_err(), ToolError::Unknown("nope".into()));
    assert!(matches!(f.call("script_info", json!([1])), Err(ToolError::Args(_))));
    // `null` arguments count as none.
    assert!(f.call("script_info", Value::Null).is_ok());
    assert!(f.err("list_paragraphs", json!({ "limit": "ten" })).contains("whole number"));
}

#[test]
fn script_info_counts_and_reviews() {
    let f = Fixture::new();
    let text = f.text("script_info", json!({}));
    assert!(text.starts_with("paragraphs  10\nscenes      2\nwords       "), "{text}");
    assert!(text.contains("reviews     1\n"), "{text}");
    assert!(text.contains("      2  Dialogue"), "{text}");
    assert!(text.contains("      1  Cesar"), "{text}");
    assert!(Fixture::bare().text("script_info", json!({})).contains("reviews     0"));
}

#[test]
fn list_paragraphs_filters() {
    let f = Fixture::new();
    let all = f.json("list_paragraphs", json!({}));
    assert_eq!(all["total"], 10);
    assert_eq!(all["more"], false);
    assert_eq!(
        all["paragraphs"][4],
        json!({ "id": 4, "kind": "Dialogue", "scene": "EXT. FARM HOUSE - NIGHT", "speaker": "Cesar", "text": "I am go to the town tomorrow." })
    );
    let ids =
        |v: &Value| v["paragraphs"].as_array().unwrap().iter().map(|p| p["id"].as_u64().unwrap()).collect::<Vec<_>>();
    assert_eq!(ids(&f.json("list_paragraphs", json!({ "kind": "dialogue" }))), [4, 9]);
    assert_eq!(ids(&f.json("list_paragraphs", json!({ "speaker": "laura" }))), [9]);
    assert_eq!(ids(&f.json("list_paragraphs", json!({ "contains": "RAIN" }))), [7]);
    assert_eq!(ids(&f.json("list_paragraphs", json!({ "scene": "kitchen" }))), [6, 7, 8, 9]);
    assert_eq!(ids(&f.json("list_paragraphs", json!({ "scene": "kitchen", "kind": "action" }))), [7]);
}

#[test]
fn list_paragraphs_pages() {
    let f = Fixture::new();
    let page = f.json("list_paragraphs", json!({ "limit": 3 }));
    assert_eq!(
        (page["total"].as_u64(), page["offset"].as_u64(), page["more"].as_bool()),
        (Some(10), Some(0), Some(true))
    );
    assert_eq!(page["paragraphs"].as_array().unwrap().len(), 3);
    let page = f.json("list_paragraphs", json!({ "offset": 3, "limit": 3 }));
    assert_eq!(page["paragraphs"][0]["id"], 3);
    assert_eq!(page["more"], true);
    let last = f.json("list_paragraphs", json!({ "offset": 9, "limit": 3 }));
    assert_eq!(last["paragraphs"].as_array().unwrap().len(), 1);
    assert_eq!(last["more"], false);
    let past = f.json("list_paragraphs", json!({ "offset": 50 }));
    assert_eq!(past["paragraphs"], json!([]));
    assert_eq!(past["total"], 10);
    // Limits are clamped, not refused.
    assert_eq!(f.json("list_paragraphs", json!({ "limit": 0 }))["paragraphs"].as_array().unwrap().len(), 1);
}

const SCENE_1: &str = "SCENE 1 of 2 (¶0–¶5)
¶0 EXT. FARM HOUSE - NIGHT
¶1 Laura smiles.
¶2 CESAR
¶3 (quietly)
¶4   I am go to the town tomorrow.
¶5 END OF ACT ONE [not editable]";

const SCENE_2: &str = "SCENE 2 of 2 (¶6–¶9)
¶6 INT. KITCHEN - DAY
¶7 Rain on the window.
¶8 LAURA
¶9   Thanks, really.";

#[test]
fn read_scene_exact_format() {
    let f = Fixture::new();
    assert_eq!(f.text("read_scene", json!({ "scene": 1 })), SCENE_1);
    assert_eq!(f.text("read_scene", json!({ "scene": 2 })), SCENE_2);
    assert_eq!(f.text("read_scene", json!({ "scene": "2" })), SCENE_2);
    assert_eq!(f.text("read_scene", json!({ "scene": "Kitchen" })), SCENE_2);
    assert_eq!(f.text("read_scene", json!({ "paragraph_id": 3 })), SCENE_1);
    assert_eq!(f.text("read_scene", json!({ "paragraph_id": 6 })), SCENE_2);
}

#[test]
fn read_scene_errors() {
    let f = Fixture::new();
    assert_eq!(f.err("read_scene", json!({ "scene": 3 })), "no scene 3 (1 to 2)");
    assert_eq!(f.err("read_scene", json!({ "scene": 0 })), "no scene 0 (1 to 2)");
    assert_eq!(f.err("read_scene", json!({ "scene": "garage" })), "no scene heading contains \"garage\"");
    assert_eq!(f.err("read_scene", json!({ "paragraph_id": 99 })), "no paragraph 99");
    assert_eq!(f.err("read_scene", json!({})), "give exactly one of scene or paragraph_id");
    assert_eq!(
        f.err("read_scene", json!({ "scene": 1, "paragraph_id": 1 })),
        "give exactly one of scene or paragraph_id"
    );
}

#[test]
fn reads_use_the_live_document() {
    let mut f = Fixture::new();
    f.accept_first();
    assert!(f.text("read_scene", json!({ "scene": 1 })).contains("¶4   I am going to town tomorrow.\n"));
    assert_eq!(
        f.json("list_paragraphs", json!({ "kind": "Dialogue" }))["paragraphs"][0]["text"],
        "I am going to town tomorrow."
    );
    assert!(f.text("read_script", json!({})).contains("I am going to town tomorrow."));
    let rows = f.json("list_suggestions", json!({}));
    assert_eq!(rows[0]["current"], "I am going to town tomorrow.");
    assert_eq!(rows[0]["status"], "accepted 1");
}

#[test]
fn read_scene_before_the_first_heading() {
    let mut f = Fixture::bare();
    // A line before any heading (e.g. a title card): its own unnumbered part.
    let mut script = f.fdx.to_script();
    script.paragraphs.insert(0, scriptorium_core::Paragraph::plain(ParaId(100), ElementKind::Action, "FADE IN:"));
    f.doc = Document::new(script);
    assert_eq!(f.text("read_scene", json!({ "paragraph_id": 100 })), "BEFORE SCENE 1 (¶100–¶100)\n¶100 FADE IN:");
    assert_eq!(f.text("read_scene", json!({ "scene": 1 })), SCENE_1);
    let all = f.text("read_script", json!({}));
    assert!(all.starts_with("film.fdx: 2 scenes, 11 paragraphs, ≈"), "{all}");
    assert!(all.contains("\n\nBEFORE SCENE 1 (¶100–¶100)\n¶100 FADE IN:\n\nSCENE 1 of 2"), "{all}");
}

#[test]
fn read_script_is_every_scene() {
    let f = Fixture::new();
    let (chars, tokens) = script_size(&f.doc, &f.is_caps());
    assert_eq!(chars, format!("{SCENE_1}\n\n{SCENE_2}").chars().count());
    assert_eq!(tokens, chars.div_ceil(4));
    assert_eq!(
        f.text("read_script", json!({})),
        format!("film.fdx: 2 scenes, 10 paragraphs, ≈{tokens} tokens\n\n{SCENE_1}\n\n{SCENE_2}")
    );
}

/// A script of `scenes` scenes, each a heading and `lines` long action lines,
/// after one line before the first heading.
fn big_script(scenes: usize, lines: usize) -> Fixture {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\" ?>\n<FinalDraft DocumentType=\"Script\" Template=\"No\" Version=\"5\">\n<Content>\n<Paragraph Type=\"Action\"><Text>FADE IN:</Text></Paragraph>\n",
    );
    for s in 1..=scenes {
        xml.push_str(&format!("<Paragraph Type=\"Scene Heading\"><Text>INT. ROOM {s} - DAY</Text></Paragraph>\n"));
        for l in 0..lines {
            let text = format!("Scene {s} line {l}: {}", "the rain keeps falling on the long road home. ".repeat(4));
            xml.push_str(&format!("<Paragraph Type=\"Action\"><Text>{text}</Text></Paragraph>\n"));
        }
    }
    xml.push_str("</Content>\n</FinalDraft>\n");
    let fdx = FdxFile::parse(xml).unwrap();
    let doc = Document::new(fdx.to_script());
    Fixture { fdx, doc, session: Session::default(), default_review: None }
}

#[test]
fn read_script_pages_a_long_script() {
    // ≈200 scenes of ≈1k tokens: far over the default budget.
    let f = big_script(200, 18);
    let (_, total) = script_size(&f.doc, &f.is_caps());
    assert!(total > 150_000, "{total}");
    let mut from = 1;
    let mut seen = vec![];
    let mut parts = 0;
    loop {
        let args = if from == 1 { json!({}) } else { json!({ "from_scene": from }) };
        let part = f.text("read_script", args);
        parts += 1;
        let (head, body) = part.split_once("\n\n").unwrap();
        assert!(head.starts_with("film.fdx: 200 scenes, "), "{head}");
        assert!(body.chars().count().div_ceil(4) <= scriptorium_tools::READ_BUDGET, "part {parts} too long");
        assert_eq!(body.contains("FADE IN:"), from == 1, "the lines before scene 1 come with the first part");
        for line in body.lines().filter(|l| l.starts_with("SCENE ")) {
            seen.push(line.split_whitespace().nth(1).unwrap().parse::<usize>().unwrap());
        }
        let last = *seen.last().unwrap();
        assert!(head.contains(&format!("This part: scenes {from}–{last} of 200")), "{head}");
        if head.contains("This part ends the script.") {
            break;
        }
        assert!(head.contains(&format!("call read_script with from_scene {}", last + 1)), "{head}");
        from = last + 1;
    }
    assert_eq!(seen, (1..=200).collect::<Vec<_>>(), "every scene once, in order");
    assert!(parts >= 8, "{parts}");
    // A small budget still gives a whole scene; bad scene numbers are refused.
    let one = f.text("read_script", json!({ "from_scene": 5, "max_tokens": 1 }));
    assert!(one.contains("This part: scene 5 of 200"), "{one}");
    assert!(one.contains("from_scene 6"), "{one}");
    assert_eq!(f.err("read_script", json!({ "from_scene": 201 })), "no scene 201 (1 to 200)");
    assert_eq!(f.err("read_script", json!({ "from_scene": 0 })), "no scene 0 (1 to 200)");
    // A short script is still one part with the plain header.
    let small = Fixture::new();
    assert!(!small.text("read_script", json!({ "max_tokens": 1000 })).contains("This part"));
}

#[test]
fn list_reviews_with_and_without_summary() {
    let f = Fixture::new();
    let rows = f.json("list_reviews", json!({}));
    let r = &rows[0];
    assert_eq!(r["name"], "a.review.txt");
    assert_eq!(r["path"], "a.review.txt");
    assert_eq!(r["default"], true);
    assert_eq!((r["total"].as_u64(), r["pending"].as_u64(), r["accepted"].as_u64()), (Some(1), Some(1), Some(0)));
    assert_eq!((r["kept"].as_u64(), r["rewritten"].as_u64()), (Some(0), Some(0)));
    assert!(r["id"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(r.get("unmatched").is_none());

    let rows = f.json("list_reviews", json!({ "summary": true }));
    assert_eq!(rows[0]["unmatched"], json!([{ "block": 2, "before": "This line is not in the script at all." }]));
    assert_eq!(rows[0]["invisible"], 0);
    assert_eq!(rows[0]["warnings"], json!([]));

    let mut f = Fixture::new();
    f.default_review = None;
    assert_eq!(f.json("list_reviews", json!({}))[0]["default"], false);
    assert_eq!(Fixture::bare().json("list_reviews", json!({})), json!([]));
}

#[test]
fn list_suggestions_rows() {
    let mut f = Fixture::new();
    let rows = f.json("list_suggestions", json!({}));
    assert_eq!(
        rows,
        json!([{
            "n": 1, "entry": 0, "paragraph": 4, "paragraph_ids": [4], "shape": "line", "kind": "Dialogue", "scene": "EXT. FARM HOUSE - NIGHT",
            "speaker": "Cesar", "status": "pending", "outdated": false, "fuzzy": false,
            "current": "I am go to the town tomorrow.", "before": "I am go to the town tomorrow.",
            "options": ["I am going to town tomorrow."], "why": "verb form",
        }])
    );
    assert_eq!(f.json("list_suggestions", json!({ "review": "a.review.txt" })), rows);
    assert_eq!(f.json("list_suggestions", json!({ "review": "notes/a.review.txt" })), rows, "by file name");
    assert!(f.err("list_suggestions", json!({ "review": "b.txt" })).contains("no attached review \"b.txt\""));

    f.accept_first();
    assert_eq!(f.json("list_suggestions", json!({ "pending_only": true })), json!([]));

    f.default_review = None;
    assert!(f.err("list_suggestions", json!({})).contains("which review?"));
    assert_eq!(Fixture::bare().err("list_suggestions", json!({})), "no reviews attached yet");
}

#[test]
fn suggest_change_appends_to_the_default_review() {
    let mut f = Fixture::new();
    let a = f.append("suggest_change", json!({ "paragraph_id": 1, "options": ["Laura grins."], "why": "livelier" }));
    assert_eq!(a.target, f.own_target());
    assert!(!a.batch && a.failed.is_empty());
    let e = &a.proposals[0].entry;
    assert_eq!(
        (e.para.as_slice(), e.before.as_slice(), e.why.as_str()),
        ([ParaId(1)].as_slice(), [Line::untyped("Laura smiles.")].as_slice(), "livelier")
    );
    assert_eq!(e.options, [vec![Line::untyped("Laura grins.")]]);

    let applied = f.apply(0, a);
    // Numbered by position in the script: ¶1 comes before the existing
    // suggestion on ¶4, though its item is added after it.
    assert_eq!(applied.message, "added suggestion #1 (e2, ¶1) to a.review.txt");
    assert_eq!(applied.numbers, [Some(1)]);
    assert_eq!(applied.entries, [Some(2)]);
    let file_text = applied.file_text.unwrap();
    assert_eq!(file_text, f.session.reviews[0].text());
    assert!(file_text.starts_with(REVIEW.split("\n---").next().unwrap()), "{file_text}");
    assert!(file_text.contains("BEFORE: Laura smiles.\nPARA: 1\nAFTER: Laura grins.\nWHY: livelier"));
    assert!(file_text.contains("\nId: "), "an Id: header is pinned");
    // The new suggestion is listed, numbered as the message said, in script
    // order; the items themselves keep their order (hosts index them).
    let rows = f.json("list_suggestions", json!({}));
    assert_eq!(
        (rows[0]["n"].as_u64(), rows[0]["entry"].as_u64(), rows[0]["paragraph"].as_u64()),
        (Some(1), Some(2), Some(1))
    );
    assert_eq!((rows[1]["n"].as_u64(), rows[1]["paragraph"].as_u64()), (Some(2), Some(4)));
    let items = &f.session.reviews[0].items;
    assert_eq!((items[0].para, items[1].para), (ParaId(4), ParaId(1)));
    assert_eq!(item_numbers(&f.doc, items), [2, 1]);
    assert_eq!(item_numbered(&f.doc, items, 1), Some(1));
    assert_eq!(item_numbered(&f.doc, items, 3), None);
    // A duplicate names the existing suggestion by that number too.
    let again = f.err("suggest_change", json!({ "paragraph_id": 1, "options": ["Laura beams."] }));
    assert!(again.contains("(#1, pending)"), "{again}");
}

#[test]
fn suggest_change_without_a_review_targets_the_hosts_own() {
    let f = Fixture::bare();
    let a = f.append("suggest_change", json!({ "paragraph_id": 4, "options": ["I'm going."] }));
    assert_eq!(a.target, Target::Own);
    // Line 4 already has a suggestion in a.review.txt, but that isn't the target.
    let mut f = Fixture::new();
    f.default_review = None;
    assert_eq!(f.append("suggest_change", json!({ "paragraph_id": 4, "options": ["I'm going."] })).target, Target::Own);
}

#[test]
fn suggest_change_review_file_and_before() {
    let f = Fixture::new();
    let a = f.append(
        "suggest_change",
        json!({ "review_file": "new.review.txt", "before": "rain on the  window.", "options": ["Rain."] }),
    );
    assert_eq!(a.target, Target::File("new.review.txt".into()));
    assert_eq!(a.proposals[0].entry.para, [ParaId(7)]);
    // A review_file that is attached is checked for duplicates.
    let err = f.err("suggest_change", json!({ "review_file": "a.review.txt", "paragraph_id": 4, "options": ["x"] }));
    assert!(err.starts_with("review a.review.txt already has a suggestion on ¶4 (#1, pending)"), "{err}");
    assert_eq!(
        f.err("suggest_change", json!({ "paragraph_id": 4, "before": "x", "options": ["y"] })),
        "give exactly one of paragraph_id, paragraph_ids, insert_after or before"
    );
    assert_eq!(
        f.err("suggest_change", json!({ "options": ["y"] })),
        "give exactly one of paragraph_id, paragraph_ids or insert_after"
    );
    assert!(
        f.err("suggest_change", json!({ "before": "Nothing like it", "options": ["y"] }))
            .starts_with("no line in the script matches")
    );
}

#[test]
fn suggest_change_refusals() {
    let f = Fixture::new();
    let err = |args: Value| f.err("suggest_change", args);
    assert_eq!(err(json!({ "paragraph_id": 999, "options": ["x"] })), "no paragraph 999");
    assert_eq!(err(json!({ "paragraph_id": 5, "options": ["x"] })), "paragraph 5 (End of Act) isn't editable");
    // Letter case in a character cue doesn't show; edge whitespace never does.
    assert_eq!(
        err(json!({ "paragraph_id": 2, "options": ["CESAR"] })),
        "that change wouldn't be visible in the script"
    );
    assert_eq!(
        err(json!({ "paragraph_id": 1, "options": [" Laura smiles. "] })),
        "that change wouldn't be visible in the script"
    );
    assert!(err(json!({ "paragraph_id": 1, "options": ["Laura\nsmiles."] })).contains("newlines"));
    assert_eq!(err(json!({ "paragraph_id": 1, "options": ["  "] })), "options can't be empty");
    assert_eq!(
        err(json!({ "paragraph_id": 1, "options": [] })),
        "give at least one option (an option `[]` cuts the lines)"
    );
    assert_eq!(err(json!({ "paragraph_id": 1, "options": ["a", "b", "c", "d"] })), "give at most 3 options, not 4");
    assert!(err(json!({ "paragraph_id": 1 })).contains("`options` must be a list"));
    let dup = err(json!({ "paragraph_id": 4, "options": ["Something else."] }));
    assert_eq!(
        dup,
        "this conversation's review already has a suggestion on ¶4 (#1, pending): one suggestion per line per \
         review; the writer can ask for a new one in a new conversation"
    );
    assert!(err(json!({ "paragraph_id": 1, "options": ["Laura\u{2028}smiles."] })).contains("newlines"));
    assert!(err(json!({ "paragraph_id": 1, "options": ["Laura\u{2029}smiles."] })).contains("newlines"));
    // One visible option among invisible ones is enough.
    let a = f.append("suggest_change", json!({ "paragraph_id": 2, "options": ["CESAR", "CARLOS"] }));
    assert_eq!(a.proposals[0].entry.options, [vec![Line::untyped("CESAR")], vec![Line::untyped("CARLOS")]]);
}

#[test]
fn suggest_changes_partial_failure() {
    let mut f = Fixture::new();
    let a = f.append(
        "suggest_changes",
        json!({ "suggestions": [
            { "paragraph_id": 1, "options": ["Laura grins."] },
            { "paragraph_id": 999, "options": ["x"] },
            { "paragraph_id": 7, "options": ["Rain hammers the window."], "why": "stronger" },
            { "paragraph_id": 7, "options": ["Rain on the glass."] },
            { "paragraph_id": 5, "options": ["x"] },
            { "paragraph_id": 4, "options": ["I'm going to town."] },
            { "options": ["x"] },
        ]}),
    );
    assert!(a.batch);
    assert_eq!(a.target, f.own_target());
    assert_eq!(a.proposals.iter().map(|p| p.index).collect::<Vec<_>>(), [0, 2]);
    let failed: Vec<(usize, Option<u64>)> = a.failed.iter().map(|f| (f.index, f.paragraph_id)).collect();
    assert_eq!(failed, [(1, Some(999)), (3, Some(7)), (4, Some(5)), (5, Some(4)), (6, None)]);
    assert_eq!(a.failed[1].error, "paragraph 7 is already suggested on by item 2 of this batch");

    let applied = f.apply(0, a);
    // By script position: ¶1, then the existing ¶4, then ¶7.
    assert_eq!(applied.numbers, [Some(1), Some(3)]);
    let result: Value = serde_json::from_str(&applied.message).unwrap();
    assert_eq!(result["added"], 2);
    assert_eq!(
        result["suggestions"],
        json!([
            { "index": 0, "paragraph_id": 1, "shape": "line", "n": 1, "entry": "e2" },
            { "index": 2, "paragraph_id": 7, "shape": "line", "n": 3, "entry": "e3" },
        ])
    );
    let failed = result["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 5);
    assert_eq!(failed[0], json!({ "index": 1, "paragraph_id": 999, "error": "no paragraph 999" }));
    assert_eq!(failed[4]["paragraph_id"], Value::Null);
    // One write holding both blocks.
    let file_text = applied.file_text.unwrap();
    assert_eq!(file_text.matches("PARA: ").count(), 2);
    assert!(file_text.contains("PARA: 7\nAFTER: Rain hammers the window.\nWHY: stronger"));
    assert_eq!(f.session.reviews[0].items.len(), 3);
}

#[test]
fn suggest_changes_all_failed_is_still_success_shaped() {
    let f = Fixture::new();
    let v = f.json("suggest_changes", json!({ "suggestions": [{ "paragraph_id": 999, "options": ["x"] }] }));
    assert_eq!(
        v,
        json!({ "added": 0, "suggestions": [], "failed": [{ "index": 0, "paragraph_id": 999, "error": "no paragraph 999" }] })
    );
}

#[test]
fn suggest_changes_limits() {
    let f = Fixture::new();
    let many: Vec<Value> = (0..=MAX_BATCH).map(|_| json!({ "paragraph_id": 1, "options": ["x"] })).collect();
    assert!(matches!(f.call("suggest_changes", json!({ "suggestions": many })), Err(ToolError::Args(_))));
    assert_eq!(f.err("suggest_changes", json!({ "suggestions": [] })), "give at least one suggestion");
    assert_eq!(f.err("suggest_changes", json!({})), "`suggestions` must be a list");
}

#[test]
fn suggest_changes_into_a_new_review_skips_the_duplicate_check() {
    let f = Fixture::bare();
    let a = f.append("suggest_changes", json!({ "suggestions": [{ "paragraph_id": 4, "options": ["I'm going."] }] }));
    assert_eq!(a.target, Target::Own);
    assert_eq!(a.failed, Vec::<Failed>::new());
}

#[test]
fn apply_refuses_empty_proposals() {
    let mut f = Fixture::new();
    let empty = Append { target: f.own_target(), proposals: vec![], failed: vec![], batch: false };
    assert!(matches!(f.try_apply(0, empty), Err(ToolError::Invalid(_))));
    // A batch is success-shaped even then, and writes nothing.
    let empty = Append { target: f.own_target(), proposals: vec![], failed: vec![], batch: true };
    let applied = f.apply(0, empty);
    assert_eq!(applied.file_text, None);
    assert_eq!(
        serde_json::from_str::<Value>(&applied.message).unwrap(),
        json!({ "added": 0, "suggestions": [], "failed": [] })
    );
}

#[test]
fn add_before_relaxes_the_suggest_change_schema() {
    let mut spec = catalogue().into_iter().find(|s| s.name == "suggest_change").unwrap();
    assert_eq!(spec.schema["required"], json!(["options"]), "the tool checks there is exactly one target");
    add_before(&mut spec);
    assert_eq!(spec.schema["required"], json!(["options"]));
    assert_eq!(spec.schema["properties"]["before"]["type"], "string");
    assert!(spec.description.ends_with("Or name one line by its text with before instead of paragraph_id."));
}

/// Two identical lines (¶2 and ¶4), an empty paragraph (¶5), and a scene
/// heading that is just a number.
const TWINS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Scene Heading">
      <Text>INT. HALL - DAY</Text>
    </Paragraph>
    <Paragraph Type="Character">
      <Text>LAURA</Text>
    </Paragraph>
    <Paragraph Type="Dialogue">
      <Text>We should go.</Text>
    </Paragraph>
    <Paragraph Type="Character">
      <Text>CESAR</Text>
    </Paragraph>
    <Paragraph Type="Dialogue">
      <Text>We should go.</Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text></Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text>They leave the hall together.</Text>
    </Paragraph>
    <Paragraph Type="Scene Heading">
      <Text>1987</Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text>Snow.</Text>
    </Paragraph>
  </Content>
</FinalDraft>
"#;

fn twins() -> Fixture {
    let fdx = FdxFile::parse(TWINS.to_string()).unwrap();
    let doc = Document::new(fdx.to_script());
    Fixture { fdx, doc, session: Session::default(), default_review: None }
}

#[test]
fn parallel_suggestions_on_one_line_before_the_review_exists() {
    let mut f = twins();
    // Both calls are validated before the host has created its review.
    let first = f.append("suggest_change", json!({ "paragraph_id": 4, "options": ["Let's go."] }));
    let second = f.append("suggest_change", json!({ "paragraph_id": 4, "options": ["We must go."] }));
    let batch = f.append("suggest_changes", json!({ "suggestions": [{ "paragraph_id": 4, "options": ["Go."] }] }));
    assert_eq!((&first.target, &second.target), (&Target::Own, &Target::Own));

    let r = f.attach_own();
    f.default_review = Some(r);
    let applied = f.apply(r, first);
    assert_eq!(applied.message, "added suggestion #1 (e0, ¶4) to thread.review.txt");
    let text = f.session.reviews[r].text().to_string();

    // The second is refused against the review that now exists, and nothing is written.
    let err = f.try_apply(r, second).unwrap_err().to_string();
    assert_eq!(
        err,
        "this conversation's review already has a suggestion on ¶4 (#1, pending): one suggestion per line per \
         review; the writer can ask for a new one in a new conversation"
    );
    // The batch form: still success-shaped, nothing to save.
    let applied = f.apply(r, batch);
    assert_eq!(applied.file_text, None);
    assert_eq!((applied.numbers.as_slice(), applied.entries.as_slice()), ([None].as_slice(), [None].as_slice()));
    let v: Value = serde_json::from_str(&applied.message).unwrap();
    assert_eq!(v["added"], 0);
    assert_eq!(v["failed"][0]["paragraph_id"], 4);
    assert!(v["failed"][0]["error"].as_str().unwrap().contains("already has a suggestion on ¶4"));

    // Never placed on the other line with the same text.
    let review = &f.session.reviews[r];
    assert_eq!(review.text(), text);
    assert_eq!(review.file.entries.len(), 1);
    assert_eq!(review.items.iter().map(|it| it.para).collect::<Vec<_>>(), [ParaId(4)]);
    assert_eq!(text.matches("BEFORE:").count(), 1);
}

#[test]
fn apply_rechecks_duplicates_within_one_call() {
    let mut f = twins();
    let a = f.append("suggest_change", json!({ "paragraph_id": 4, "options": ["Let's go."] }));
    let b = f.append("suggest_change", json!({ "paragraph_id": 4, "options": ["We must go."] }));
    let mut proposals = a.proposals;
    proposals.push(Proposal { index: 1, entry: b.proposals[0].entry.clone(), fuzzy: false });
    let r = f.attach_own();
    let applied = f.apply(r, Append { target: Target::Own, proposals, failed: vec![], batch: true });
    let v: Value = serde_json::from_str(&applied.message).unwrap();
    assert_eq!(v["added"], 1);
    assert_eq!(v["failed"][0]["error"], "paragraph 4 is already suggested on by item 0 of this batch");
    assert_eq!(applied.entries, [Some(0), None]);
    assert_eq!(applied.file_text.unwrap().matches("BEFORE:").count(), 1);
}

#[test]
fn a_line_changed_before_apply_is_not_written_elsewhere() {
    let mut f = twins();
    let a = f.append("suggest_change", json!({ "paragraph_id": 4, "options": ["Let's go."] }));
    // The writer changes ¶4 before the host applies; ¶2 still reads like the
    // suggestion's BEFORE.
    let mut script = f.fdx.to_script();
    script.paragraphs[4] = scriptorium_core::Paragraph::plain(ParaId(4), ElementKind::Dialogue, "We must stay.");
    f.doc = Document::new(script);
    let r = f.attach_own();
    let err = f.try_apply(r, a).unwrap_err().to_string();
    assert!(err.starts_with("¶4 changed since the suggestion was made"), "{err}");
    assert!(f.session.reviews[r].file.entries.is_empty());
    assert!(f.session.reviews[r].items.is_empty());
    assert!(!f.session.reviews[r].text().contains("BEFORE:"));

    // A change that became invisible meanwhile is refused too.
    let mut f = twins();
    let a = f.append("suggest_change", json!({ "paragraph_id": 4, "options": ["We must go."] }));
    let mut script = f.fdx.to_script();
    script.paragraphs[4] = scriptorium_core::Paragraph::plain(ParaId(4), ElementKind::Dialogue, "We must go.");
    f.doc = Document::new(script);
    let r = f.attach_own();
    assert_eq!(f.try_apply(r, a).unwrap_err().to_string(), "that change wouldn't be visible in the script");
    assert!(f.session.reviews[r].file.entries.is_empty());
}

#[test]
fn before_with_several_exact_matches_lists_them() {
    let f = twins();
    assert_eq!(
        f.err("suggest_change", json!({ "before": "We  should go.", "options": ["Go."] })),
        "2 lines read \"We  should go.\" (¶2, ¶4): give paragraph_id instead"
    );
    // One exact match is used; otherwise the closest line, as review matching finds it.
    let a = f.append("suggest_change", json!({ "before": "Snow.", "options": ["Snow falls."] }));
    assert_eq!(a.proposals[0].entry.para, [ParaId(8)]);
    let a = f.append("suggest_change", json!({ "before": "They leave the hall together", "options": ["They go."] }));
    assert_eq!(a.proposals[0].entry.para, [ParaId(6)]);
    assert_eq!(f.err("suggest_change", json!({ "before": " ", "options": ["Go."] })), "`before` is empty");
}

#[test]
fn before_skips_lines_the_review_already_uses_for_close_matches() {
    let mut f = twins();
    let r = f.attach_own();
    f.default_review = Some(r);
    let a = f.append("suggest_change", json!({ "paragraph_id": 6, "options": ["They go."] }));
    f.apply(r, a);
    // ¶6 is taken: the best match is reported, not skipped for another line,
    // whether `before` finds it approximately or exactly.
    let dup = "this conversation's review already has a suggestion on ¶6 (#1, pending): one suggestion per line \
               per review; the writer can ask for a new one in a new conversation";
    assert_eq!(f.err("suggest_change", json!({ "before": "They leave the hall together", "options": ["x"] })), dup);
    assert_eq!(f.err("suggest_change", json!({ "before": "They leave the hall together.", "options": ["x"] })), dup);
}

#[test]
fn before_reports_a_non_editable_best_match() {
    let f = Fixture::new();
    // ¶5 is an End of Act (not editable): an error, not a fall-through to another line.
    for before in ["END OF ACT ONE", "end of act one", "END OF ACT ON"] {
        assert_eq!(f.err("suggest_change", json!({ "before": before, "options": ["x"] })), "¶5 isn't editable");
    }
}

#[test]
fn before_with_several_fuzzy_candidates_is_ambiguous_however_far_apart() {
    let mut f = twins();
    let mut script = f.fdx.to_script();
    script.paragraphs[8] =
        scriptorium_core::Paragraph::plain(ParaId(8), ElementKind::Action, "They leave the hall together, slowly.");
    f.doc = Document::new(script);
    // ¶6 scores about 0.98 and ¶8 about 0.86: both reach the threshold, so neither is guessed.
    assert_eq!(
        f.err("suggest_change", json!({ "before": "They leave the hall together", "options": ["x"] })),
        "several lines read much like \"They leave the hall together\" (¶6, ¶8): give paragraph_id instead"
    );
    // A single exact match still wins over fuzzy candidates.
    let a = f.append("suggest_change", json!({ "before": "They leave the hall together.", "options": ["x"] }));
    assert_eq!((a.proposals[0].entry.para.as_slice(), a.proposals[0].fuzzy), ([ParaId(6)].as_slice(), false));
}

#[test]
fn read_scene_skips_empty_lines_and_reads_number_headings() {
    let f = twins();
    let scene_1 = "SCENE 1 of 2 (¶0–¶6)
¶0 INT. HALL - DAY
¶1 LAURA
¶2   We should go.
¶3 CESAR
¶4   We should go.
¶6 They leave the hall together.";
    let scene_2 = "SCENE 2 of 2 (¶7–¶8)\n¶7 1987\n¶8 Snow.";
    assert_eq!(f.text("read_scene", json!({ "scene": 1 })), scene_1);
    assert_eq!(f.text("read_scene", json!({ "scene": "2" })), scene_2);
    // Digits that aren't a scene number are heading text.
    assert_eq!(f.text("read_scene", json!({ "scene": "1987" })), scene_2);
    assert_eq!(f.text("read_scene", json!({ "scene": "98" })), scene_2);
    assert_eq!(f.err("read_scene", json!({ "scene": 1987 })), "no scene 1987 (1 to 2)");
    assert_eq!(f.err("read_scene", json!({ "scene": "5" })), "no scene 5 (1 to 2) and no scene heading contains \"5\"");
    assert_eq!(f.err("read_scene", json!({ "scene": "" })), "`scene` is empty: give a scene number or heading text");
    assert_eq!(f.err("read_scene", json!({ "scene": "  " })), "`scene` is empty: give a scene number or heading text");
}

#[test]
fn read_scene_capitals_follow_is_caps() {
    let f = Fixture::new();
    let read = |is_caps: &dyn Fn(&ElementKind) -> bool| {
        let ctx = Ctx { doc: &f.doc, session: &f.session, script_name: "film.fdx", is_caps, default_review: None };
        match call(&ctx, "read_scene", &json!({ "scene": 2 })).unwrap() {
            Outcome::Text(t) => t,
            o => panic!("{o:?}"),
        }
    };
    assert_eq!(
        read(&|_| false),
        "SCENE 2 of 2 (¶6–¶9)\n¶6 int. kitchen - day\n¶7 Rain on the window.\n¶8 LAURA\n¶9   Thanks, really."
    );
    assert_eq!(
        read(&|k| *k == ElementKind::Dialogue),
        "SCENE 2 of 2 (¶6–¶9)\n¶6 int. kitchen - day\n¶7 Rain on the window.\n¶8 LAURA\n¶9   THANKS, REALLY."
    );
}

#[test]
fn read_scene_numbers_are_plain_digits() {
    let f = twins();
    // Signs and leading zeros make heading text, not a scene number.
    assert_eq!(f.err("read_scene", json!({ "scene": "02" })), "no scene heading contains \"02\"");
    assert_eq!(f.err("read_scene", json!({ "scene": "+2" })), "no scene heading contains \"+2\"");
    assert_eq!(f.err("read_scene", json!({ "scene": "007" })), "no scene heading contains \"007\"");
    // A heading that is a zero-padded number is found as text.
    assert!(f.text("read_scene", json!({ "scene": "198" })).starts_with("SCENE 2 of 2"));
    assert!(f.text("read_scene", json!({ "scene": " 2 " })).starts_with("SCENE 2 of 2"));
}

#[test]
fn before_matches_case_in_capitals_and_refuses_close_fuzzy_matches() {
    let mut f = twins();
    // Character cues show in capitals, so letter case doesn't matter there.
    let a = f.append("suggest_change", json!({ "before": "laura", "options": ["LAURA B."] }));
    assert_eq!((a.proposals[0].entry.para.as_slice(), a.proposals[0].fuzzy), ([ParaId(1)].as_slice(), false));
    // Dialogue isn't in capitals: not exact, and the two identical lines are equally close.
    assert_eq!(
        f.err("suggest_change", json!({ "before": "we should go.", "options": ["Go."] })),
        "several lines read much like \"we should go.\" (¶2, ¶4): give paragraph_id instead"
    );
    assert_eq!(
        f.err("suggest_change", json!({ "before": "We should go!", "options": ["Go."] })),
        "several lines read much like \"We should go!\" (¶2, ¶4): give paragraph_id instead"
    );
    // A clear fuzzy match is used, and the result quotes the line it went on.
    let a = f.append("suggest_change", json!({ "before": "They leave the hall together", "options": ["They go."] }));
    assert!(a.proposals[0].fuzzy);
    let r = f.attach_own();
    let applied = f.apply(r, a);
    assert_eq!(
        applied.message,
        "added suggestion #1 (e0, ¶6) to thread.review.txt; `before` matched ¶6 approximately, which reads \
         \"They leave the hall together.\""
    );
}

#[test]
fn a_line_that_now_reads_as_an_option_is_not_written() {
    let mut f = twins();
    let a = f.append("suggest_change", json!({ "paragraph_id": 6, "options": ["They go.", "They leave."] }));
    // The line now reads as one of the options: the review would take that as
    // a decided entry, but a new suggestion is for the line as it was read.
    let mut script = f.fdx.to_script();
    script.paragraphs[6] = scriptorium_core::Paragraph::plain(ParaId(6), ElementKind::Action, "They go.");
    f.doc = Document::new(script);
    let r = f.attach_own();
    let err = f.try_apply(r, a).unwrap_err().to_string();
    assert!(err.starts_with("¶6 changed since the suggestion was made"), "{err}");
    assert!(f.session.reviews[r].file.entries.is_empty());
    assert!(!f.session.reviews[r].text().contains("BEFORE:"));
}

#[test]
fn an_invisible_entry_that_was_written_is_reported_ok_so_the_host_saves_it() {
    let mut f = twins();
    let entry =
        scriptorium_review::Entry { para: vec![ParaId(3)], ..scriptorium_review::Entry::single("CESAR", ["cesar"]) };
    let a = Append {
        target: Target::Own,
        proposals: vec![Proposal { index: 0, entry, fuzzy: false }],
        failed: vec![],
        batch: false,
    };
    let r = f.attach_own();
    // Simulate review matching disagreeing with apply's visibility check: the
    // first question (the check) says no capitals, every later one (matching) yes.
    let asked = std::cell::Cell::new(0);
    let is_caps = |k: &ElementKind| {
        asked.set(asked.get() + 1);
        asked.get() > 1 && *k == ElementKind::Character
    };
    let applied = apply(&mut f.session, &f.doc, r, a, &is_caps).unwrap();
    assert_eq!(
        applied.message,
        "written to thread.review.txt as e0, but it changes nothing visible on ¶3, so it isn't shown"
    );
    assert_eq!((applied.numbers.as_slice(), applied.entries.as_slice()), ([None].as_slice(), [Some(0)].as_slice()));
    let text = applied.file_text.expect("written, so the host must save it");
    assert_eq!(text, f.session.reviews[r].text());
    assert!(text.contains("PARA: 3"));
}

#[test]
fn apply_refuses_a_review_whose_id_changed() {
    let mut f = Fixture::new();
    let mut a = f.append("suggest_change", json!({ "paragraph_id": 1, "options": ["Laura grins."] }));
    let before = f.session.reviews[0].text().to_string();
    a.target = Target::Review { index: 0, id: "someone-else".into() };
    let err = f.try_apply(0, a).unwrap_err().to_string();
    assert_eq!(
        err,
        "a.review.txt isn't the review this call was for (id someone-else): reviews changed meanwhile; call the \
         tool again"
    );
    assert_eq!(f.session.reviews[0].text(), before);
}

#[test]
fn find_review_compares_stored_paths_not_file_names() {
    let mut f = Fixture::bare();
    let script = f.fdx.to_script();
    let open = |path: &str, id: &str| {
        let text = format!("SCRIPT REVIEW v1\nScript: film.fdx\nId: {id}\n");
        Review::open("x.review.txt", &text, &script, |k| f.fdx.is_caps(k.name()), "film.fdx").with_path(path)
    };
    f.session = Session::new(vec![open("a/x.review.txt", "a"), open("b/x.review.txt", "b")]);
    for spelling in ["b/x.review.txt", "./b//x.review.txt", "b\\x.review.txt", "a/../b/x.review.txt"] {
        assert_eq!(find_review(&f.session, spelling), Some(1), "{spelling}");
    }
    assert_eq!(find_review(&f.session, "x.review.txt"), None, "a file name alone isn't a path");
    assert_eq!(find_review(&f.session, "c/x.review.txt"), None);
    assert_eq!(find_review(&f.session, ""), None);
    // Comparison is case-sensitive: hosts pass canonical stored paths.
    assert_eq!(find_review(&f.session, "B/x.review.txt"), None);
    // `..` never pops a root or a drive prefix.
    f.session = Session::new(vec![open("C:/x.review.txt", "c"), open("/x.review.txt", "r")]);
    for spelling in ["C:/../x.review.txt", "C:\\..\\..\\x.review.txt", "C:/a/../x.review.txt"] {
        assert_eq!(find_review(&f.session, spelling), Some(0), "{spelling}");
    }
    assert_eq!(find_review(&f.session, "C:/../../x.review.txt"), Some(0));
    assert_eq!(find_review(&f.session, "/../x.review.txt"), Some(1));
    assert_eq!(find_review(&f.session, "C:/a/../../x.review.txt"), Some(0));
    assert_eq!(find_review(&f.session, "C:x.review.txt"), None, "drive-relative isn't the root");
    assert_eq!(find_review(&f.session, "C:/../D:/x.review.txt"), None);
    f.session = Session::new(vec![open("a/x.review.txt", "a"), open("b/x.review.txt", "b")]);
    // So a review_file in another folder is a new file, not the attached one.
    f.default_review = Some(0);
    let a = f.append("suggest_change", json!({ "review_file": "c/x.review.txt", "paragraph_id": 1, "options": ["x"] }));
    assert_eq!(a.target, Target::File("c/x.review.txt".into()));
}

#[test]
fn add_host_fields_adds_script_everywhere_and_review_file_to_suggest_tools() {
    let mut specs = catalogue();
    for spec in &mut specs {
        add_host_fields(spec, &[HostField::Script, HostField::ReviewFile]);
        add_host_fields(spec, &[HostField::Script]);
    }
    let get = |name: &str| specs.iter().find(|s| s.name == name).unwrap();
    let read = get("read_scene");
    assert_eq!(read.schema["properties"]["script"]["type"], "string");
    assert_eq!(read.schema["required"], json!(["script"]));
    assert!(read.schema["properties"].get("review_file").is_none());
    for name in ["suggest_change", "suggest_changes"] {
        let s = get(name);
        assert_eq!(s.schema["properties"]["review_file"]["type"], "string", "{name}");
        assert_eq!(s.schema["required"][0], "script", "{name}");
        assert!(!s.schema["required"].as_array().unwrap().iter().any(|r| r == "review_file"), "{name}");
    }
    assert_eq!(get("suggest_change").schema["required"], json!(["script", "options"]));
}

#[test]
fn the_system_prompt_names_only_tools_in_the_catalogue() {
    assert_eq!(unknown_tools(SYSTEM, &catalogue()), Vec::<String>::new());
    for tool in ["read_scene", "read_script", "list_paragraphs", "suggest_change", "suggest_changes"] {
        assert!(SYSTEM.contains(tool), "{tool}");
    }
    let prompt = system("film.fdx");
    assert!(prompt.contains("the screenplay film.fdx") && !prompt.contains("{script}"));
    // Sent with every request: keep it tight (about four characters a token).
    assert!(SYSTEM.len() < 4500, "the system prompt grew to {} bytes", SYSTEM.len());
    // It does notice a tool that isn't there.
    let tools: Vec<_> = catalogue().into_iter().filter(|t| t.name == "suggest_change").collect();
    let prompt = "Use read_scene, then suggest_change. Not_a_tool or snake_ alone don't count.";
    assert_eq!(unknown_tools(prompt, &tools), ["read_scene"]);
    assert_eq!(unknown_tools(prompt, &[]), ["read_scene", "suggest_change"]);
}

/// The tools that only read. Every catalogued tool is either here or in
/// `may_append`, never both, so a new tool must be sorted before it ships:
/// hosts take the session lock only for `may_append` tools.
const READS: &[&str] =
    &["script_info", "list_paragraphs", "read_scene", "read_script", "list_reviews", "list_suggestions"];

#[test]
fn every_tool_that_can_append_is_in_may_append() {
    let f = Fixture::new();
    for spec in catalogue() {
        let name = spec.name.as_str();
        assert!(READS.contains(&name) != may_append(name), "{name}: sort it into READS or may_append");
        // Read tools never append, whatever their arguments.
        if READS.contains(&name) {
            for args in [json!({}), json!({ "scene": 1 }), json!({ "summary": true }), json!({ "from": 1, "to": 3 })] {
                if let Ok(o) = f.call(name, args) {
                    assert!(matches!(o, Outcome::Text(_)), "{name} appended");
                }
            }
        }
    }
    // And the appending tools really do.
    assert!(matches!(
        f.call("suggest_change", json!({ "paragraph_id": 1, "options": ["Laura grins."] })),
        Ok(Outcome::Append(_))
    ));
    assert!(matches!(
        f.call("suggest_changes", json!({ "suggestions": [{ "paragraph_id": 1, "options": ["Laura grins."] }] })),
        Ok(Outcome::Append(_))
    ));
    assert!(!may_append("no_such_tool"));
}

#[test]
fn check_args_refuses_arguments_not_offered() {
    let specs = catalogue();
    let get = |name: &str| specs.iter().find(|s| s.name == name).unwrap();
    let ok = |name: &str, args: serde_json::Value| check_args(get(name), &args);
    assert_eq!(ok("suggest_change", json!({ "paragraph_id": 1, "options": ["x"], "why": "y" })), Ok(()));
    assert_eq!(ok("script_info", serde_json::Value::Null), Ok(()));
    let block = json!({ "paragraph_ids": [1, 2], "options": [[{ "kind": "Action", "text": "x" }], []] });
    assert_eq!(ok("suggest_change", block), Ok(()));
    // Wrong types are the tool's to refuse, with its own wording.
    assert_eq!(ok("suggest_change", json!({ "paragraph_id": "one", "options": "x" })), Ok(()));

    let refused = |name: &str, args: serde_json::Value, at: &str| {
        let e = ok(name, args).unwrap_err().to_string();
        assert!(e.contains(&format!("`{at}`")) && e.contains(name), "{e}");
    };
    refused("suggest_change", json!({ "paragraph_id": 1, "options": ["x"], "review_file": "/etc/x" }), "review_file");
    refused("suggest_change", json!({ "before": "Laura smiles.", "options": ["x"] }), "before");
    refused(
        "suggest_changes",
        json!({ "suggestions": [{ "paragraph_id": 1, "options": ["x"] }], "review_file": "a" }),
        "review_file",
    );
    refused(
        "suggest_changes",
        json!({ "suggestions": [{ "paragraph_id": 1, "options": ["x"] }, { "paragraph_id": 2, "options": ["y"], "review_file": "a" }] }),
        "suggestions[1].review_file",
    );
    refused(
        "suggest_change",
        json!({ "insert_after": 1, "options": [[{ "kind": "Action", "text": "x", "file": "a" }]] }),
        "options[0][0].file",
    );
    refused("read_scene", json!({ "scene": 1, "path": "/etc" }), "path");
}
