//! `insert`, `remove`, `split`, `join` and `retype` on a small script: each
//! is one undoable step, saved to the session, with `--json` listing ids.

use std::path::PathBuf;
use std::process::Command;

fn para(kind: &str, text: &str) -> String {
    format!("    <Paragraph Type=\"{kind}\">\n      <Text>{text}</Text>\n    </Paragraph>\n")
}

fn film(body: &[String]) -> String {
    format!(
        "<?xml version=\"1.0\"?>\n<FinalDraft Version=\"5\">\n  <Content>\n{}  </Content>\n</FinalDraft>\n",
        body.concat()
    )
}

/// ids 0..=3: heading, action, character, dialogue.
fn plain_script() -> String {
    film(&[
        para("Scene Heading", "EXT. FARM - NIGHT"),
        para("Action", "Laura smiles. She waits."),
        para("Character", "CESAR"),
        para("Dialogue", "Hello there."),
    ])
}

/// ids: 0 heading, 1..=4 dual dialogue members, 5 action.
fn dual_script() -> String {
    let inner = |k: &str, t: &str| {
        format!("        <Paragraph Type=\"{k}\">\n          <Text>{t}</Text>\n        </Paragraph>\n")
    };
    let dual = format!(
        "    <Paragraph>\n      <DualDialogue>\n{}{}{}{}      </DualDialogue>\n    </Paragraph>\n",
        inner("Character", "A"),
        inner("Dialogue", "One."),
        inner("Character", "B"),
        inner("Dialogue", "Two.")
    );
    film(&[para("Scene Heading", "INT. ROOM"), dual, para("Action", "End.")])
}

struct Folder(PathBuf);

impl Folder {
    fn new(name: &str, script: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("scriptorium-struct-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("film.fdx"), script).unwrap();
        Self(dir)
    }

    fn run(&self, args: &[&str]) -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_scriptorium")).current_dir(&self.0).args(args).output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr))
    }

    fn ok(&self, args: &[&str]) -> String {
        let (ok, text) = self.run(args);
        assert!(ok, "{args:?} failed: {text}");
        text
    }

    fn err(&self, args: &[&str]) -> String {
        let (ok, text) = self.run(args);
        assert!(!ok, "{args:?} should fail: {text}");
        text
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        serde_json::from_str(&self.ok(args)).unwrap()
    }

    /// (id, kind, text) of every paragraph, in script order.
    fn dump(&self) -> Vec<(u64, String, String)> {
        let v = self.json(&["dump", "film.fdx", "--json"]);
        let list = v.as_array().or_else(|| v["paragraphs"].as_array()).expect("a list").clone();
        list.iter()
            .map(|p| {
                let kind = p["kind"].as_str().or_else(|| p["type"].as_str()).unwrap().to_string();
                (p["id"].as_u64().unwrap(), kind, p["text"].as_str().unwrap().to_string())
            })
            .collect()
    }

    fn ids(&self) -> Vec<u64> {
        self.dump().into_iter().map(|p| p.0).collect()
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ids(v: &serde_json::Value) -> Vec<u64> {
    v.as_array().unwrap().iter().map(|i| i.as_u64().unwrap()).collect()
}

#[test]
fn insert_a_block_in_one_step_and_undo() {
    let f = Folder::new("insert", &plain_script());
    let v = f.json(&[
        "insert",
        "film.fdx",
        "--after",
        "1",
        "--block",
        "Character: MARIA\nParenthetical: (softly)\nDialogue: Not yet.",
        "--json",
    ]);
    assert_eq!(ids(&v["new"]), [4, 5, 6]);
    assert!(ids(&v["removed"]).is_empty());
    let paras = v["paras"].as_array().unwrap();
    assert_eq!(paras[0]["kind"], "Character");
    assert_eq!(paras[2]["text"], "Not yet.");
    assert_eq!(f.ids(), [0, 1, 4, 5, 6, 2, 3]);

    // One undo step for all three.
    f.ok(&["undo", "film.fdx"]);
    assert_eq!(f.ids(), [0, 1, 2, 3]);
    f.ok(&["redo", "film.fdx"]);
    assert_eq!(f.ids(), [0, 1, 4, 5, 6, 2, 3]);
}

#[test]
fn block_types_take_any_case_and_indent() {
    let f = Folder::new("blockcase", &plain_script());
    let v = f.json(&[
        "insert",
        "film.fdx",
        "--end",
        "--block",
        "  action: a\ndialogue: b\n  scene-heading: INT. X\n  SCENEHEADING: INT. Y",
        "--json",
    ]);
    let kinds: Vec<_> =
        v["paras"].as_array().unwrap().iter().map(|p| p["kind"].as_str().unwrap().to_string()).collect();
    assert_eq!(kinds, ["Action", "Dialogue", "Scene Heading", "Scene Heading"]);
    assert!(
        f.err(&["insert", "film.fdx", "--end", "--block", "BANANA: x"]).contains("unknown element type \"BANANA\"")
    );
}

#[test]
fn insert_guess_classifies_plain_text_as_a_paste_does() {
    let f = Folder::new("guess", &plain_script());
    let text = "INT/EXT. CAR - NIGHT\n\nRain.\n\nMARIA (V.O.)\n(softly)\nNot yet.\n\nCUT TO:";
    let v = f.json(&["insert", "film.fdx", "--end", "--guess", text, "--json"]);
    let got: Vec<(String, String)> = v["paras"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["kind"].as_str().unwrap().to_string(), p["text"].as_str().unwrap().to_string()))
        .collect();
    let want = [
        ("Scene Heading", "INT/EXT. CAR - NIGHT"),
        ("Action", "Rain."),
        ("Character", "MARIA (V.O.)"),
        ("Parenthetical", "(softly)"),
        ("Dialogue", "Not yet."),
        ("Transition", "CUT TO:"),
    ];
    assert_eq!(got, want.map(|(k, t)| (k.to_string(), t.to_string())));
    // Same classifier as the app's paste.
    let core: Vec<(String, String)> = scriptorium_core::paste::classify(text)
        .iter()
        .map(|p| (p.kind.name().to_string(), scriptorium_core::spans::text(&p.spans)))
        .collect();
    assert_eq!(got, core);
    assert!(f.err(&["insert", "film.fdx", "--end", "--guess", "\n\n"]).contains("nothing to insert"));
    assert!(
        f.err(&["insert", "film.fdx", "--end", "--guess", "a", "--block", "Action: b"]).contains("wrong arguments")
    );
}

#[test]
fn insert_before_start_end_and_short_form() {
    let f = Folder::new("insert2", &plain_script());
    f.ok(&["insert", "film.fdx", "--start", "--block", "general: top"]);
    assert!(f.err(&["insert", "film.fdx", "--before", "0", "Action", "x"]).contains("unknown option --before"));
    f.ok(&["insert", "film.fdx", "--start", "scene heading", "INT. HALL"]);
    f.ok(&["insert", "film.fdx", "--end", "Transition", "CUT TO:"]);
    let d = f.dump();
    assert_eq!(d[0].2, "INT. HALL");
    assert_eq!(d[1].1, "General");
    assert_eq!(d.last().unwrap().1, "Transition");
    // Plain output lists the new paragraph.
    let out = f.ok(&["insert", "film.fdx", "--after", "0", "Action", "Hi."]);
    assert!(out.contains("(new): Hi."), "{out}");
}

#[test]
fn remove_a_range_in_live_order_with_new_ids() {
    let f = Folder::new("remove", &plain_script());
    // New ids 4 and 5 sit between 1 and 2; live order, not numeric, defines the range.
    f.ok(&["insert", "film.fdx", "--after", "1", "--block", "Action: x\nAction: y"]);
    assert_eq!(f.ids(), [0, 1, 4, 5, 2, 3]);
    let v = f.json(&["remove", "film.fdx", "1..2", "--json"]);
    assert_eq!(ids(&v["removed"]), [1, 2, 4, 5]);
    assert_eq!(f.ids(), [0, 3]);
    f.ok(&["undo", "film.fdx"]);
    assert_eq!(f.ids(), [0, 1, 4, 5, 2, 3]);

    // A range whose end is above its start is refused, and so is an unknown id.
    assert!(f.err(&["remove", "film.fdx", "3..1"]).contains("comes before"));
    assert!(f.err(&["remove", "film.fdx", "99"]).contains("no paragraph 99"));
    // A single id.
    f.ok(&["remove", "film.fdx", "5"]);
    assert_eq!(f.ids(), [0, 1, 4, 2, 3]);
}

#[test]
fn removing_every_paragraph_is_refused() {
    let f = Folder::new("last", &plain_script());
    let e = f.err(&["remove", "film.fdx", "0..3"]);
    assert!(e.contains("last paragraph"), "{e}");
    assert_eq!(f.ids(), [0, 1, 2, 3], "rolled back");
    f.ok(&["remove", "film.fdx", "0..2"]);
    assert!(f.err(&["remove", "film.fdx", "3"]).contains("last paragraph"));
}

#[test]
fn split_at_text_and_offset() {
    let f = Folder::new("split", &plain_script());
    let v = f.json(&["split", "film.fdx", "1", "--at", "She", "--json"]);
    assert_eq!(ids(&v["new"]), [4]);
    let d = f.dump();
    assert_eq!((d[1].0, d[1].2.as_str()), (1, "Laura smiles. "));
    assert_eq!((d[2].0, d[2].2.as_str()), (4, "She waits."));
    f.ok(&["undo", "film.fdx"]);
    assert_eq!(f.dump()[1].2, "Laura smiles. She waits.");

    let (ok, text) = f.run(&["split", "film.fdx", "1", "--at", "nowhere"]);
    assert!(!ok && text.contains("isn't in the paragraph"), "{text}");

    // Offsets count characters, not bytes.
    f.ok(&["edit", "film.fdx", "3", "Café noir"]);
    f.ok(&["split", "film.fdx", "3", "--offset", "5"]);
    let d = f.dump();
    let at = d.iter().position(|p| p.0 == 3).unwrap();
    assert_eq!((d[at].2.as_str(), d[at + 1].2.as_str()), ("Café ", "noir"));
    assert!(f.err(&["split", "film.fdx", "3", "--offset", "99"]).contains("past the end"));
}

#[test]
fn join_and_retype() {
    let f = Folder::new("join", &plain_script());
    f.ok(&["split", "film.fdx", "1", "--offset", "14"]);
    assert_eq!(f.ids().len(), 5);
    let v = f.json(&["join", "film.fdx", "1", "4", "--json"]);
    assert_eq!(ids(&v["removed"]), [4]);
    assert_eq!(f.dump()[1].2, "Laura smiles. She waits.");
    assert!(f.err(&["join", "film.fdx", "1", "3"]).contains("adjacent"));
    f.ok(&["undo", "film.fdx"]);
    assert_eq!(f.ids().len(), 5);

    let v = f.json(&["retype", "film.fdx", "1", "SCENE HEADING", "--json"]);
    assert_eq!(v["paras"][0]["kind"], "Scene Heading");
    assert!(ids(&v["new"]).is_empty());
    f.ok(&["undo", "film.fdx"]);
    assert_eq!(f.dump()[1].1, "Action");
    assert!(f.err(&["retype", "film.fdx", "1", "Banana"]).contains("unknown element type"));
}

#[test]
fn bad_input_is_refused_clearly() {
    let f = Folder::new("bad", &plain_script());
    assert!(f.err(&["insert", "film.fdx", "--after", "1", "--block", "no type here"]).contains("TYPE: text"));
    assert!(f.err(&["insert", "film.fdx", "--block", "Action: x"]).contains("say where"));
    assert!(f.err(&["insert", "film.fdx", "--after", "42", "--block", "Action: x"]).contains("no paragraph 42"));
    assert_eq!(f.ids(), [0, 1, 2, 3]);
}

#[test]
fn newlines_empty_split_and_json_as_text() {
    let f = Folder::new("misc", &plain_script());
    for args in [
        vec!["insert", "film.fdx", "--end", "Action", "a\nb"],
        vec!["insert", "film.fdx", "--end", "Action", "a\rb"],
        vec!["insert", "film.fdx", "--end", "--block", "ACTION: a\rb"],
    ] {
        assert!(f.err(&args).contains("can't contain newlines"), "{args:?}");
    }
    assert!(f.err(&["split", "film.fdx", "1", "--at", ""]).contains("--at needs"));
    assert_eq!(f.ids(), [0, 1, 2, 3]);
    // Control characters XML can't hold, and stray CR, are refused everywhere text enters.
    for args in [
        vec!["insert", "film.fdx", "--end", "Action", "a\u{1}b"],
        vec!["insert", "film.fdx", "--end", "--block", "ACTION: a\u{FFFE}b"],
        vec!["edit", "film.fdx", "1", "a\u{8}b"],
        vec!["edit", "film.fdx", "1", "a\rb"],
        vec!["split", "film.fdx", "1", "--at", "\u{1b}"],
        vec!["split", "film.fdx", "1", "--at", "\r"],
    ] {
        let e = f.err(&args);
        assert!(e.contains("control character") || e.contains("newlines"), "{args:?}: {e}");
    }
    assert!(f.err(&["edit", "film.fdx", "1", "a\u{1}b"]).contains("U+0001"));
    assert_eq!(f.ids(), [0, 1, 2, 3]);
    // A text equal to "--json" is text; only the last argument is the flag.
    f.err(&["insert", "film.fdx", "--end", "Action", "--json"]);
    let out = f.ok(&["insert", "film.fdx", "--end", "--block", "ACTION: x"]);
    assert!(serde_json::from_str::<serde_json::Value>(&out).is_err());
    let v = f.json(&["insert", "film.fdx", "--end", "Action", "--json", "--json"]);
    assert_eq!(v["paras"][0]["text"], "--json");
}

#[test]
fn dual_dialogue_insert_names_both_neighbours() {
    let f = Folder::new("dualins", &dual_script());
    let e = f.err(&["insert", "film.fdx", "--after", "1", "Action", "x"]);
    assert!(e.contains("between ¶1 and ¶2") && e.contains("dual dialogue"), "{e}");
}

#[test]
fn dual_dialogue_members_refuse_structure_but_take_text() {
    let f = Folder::new("dual", &dual_script());
    for args in [
        vec!["remove", "film.fdx", "2"],
        vec!["remove", "film.fdx", "0..2"],
        vec!["retype", "film.fdx", "2", "Action"],
        vec!["split", "film.fdx", "2", "--offset", "2"],
        vec!["join", "film.fdx", "2", "3"],
        vec!["insert", "film.fdx", "--after", "2", "Action", "x"],
    ] {
        let e = f.err(&args);
        assert!(e.contains("dual dialogue"), "{args:?}: {e}");
    }
    assert_eq!(f.ids(), [0, 1, 2, 3, 4, 5], "nothing changed");
    f.ok(&["edit", "film.fdx", "2", "Fine."]);
    // Around the block is fine.
    f.ok(&["insert", "film.fdx", "--after", "4", "Action", "After."]);
}

/// `apply` after a structural edit: the .fdx writer (A2) either handles it,
/// or reports a "structural changes" error. Both are acceptable here; the
/// session itself holds the edit either way.
#[test]
fn apply_after_a_structural_edit_succeeds_or_names_the_writer_limit() {
    let f = Folder::new("apply", &plain_script());
    f.ok(&["insert", "film.fdx", "--after", "1", "Action", "Brand new."]);
    let (ok, text) = f.run(&["apply", "film.fdx"]);
    if ok {
        let written = std::fs::read_to_string(f.0.join("film (scriptorium).fdx")).unwrap();
        assert!(written.contains("Brand new."), "{written}");
    } else {
        assert!(text.contains("structural"), "{text}");
    }
    assert_eq!(f.dump()[2].2, "Brand new.");
}
