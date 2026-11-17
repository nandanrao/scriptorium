//! Tool calls from an outside agent (`Caller::Agent`: MCP, the CLI `tool`)
//! through the one executor, `run_call(FileScript, Caller::Agent)`, on a
//! script in a temporary folder: the shared tools as the MCP server and the
//! CLI `tool` have run them, and `review_file` the way the CLI takes it (a
//! missing folder made, a new review written only with suggestions, its id
//! salted from the resolved path).

use std::path::{Path, PathBuf};

use scriptorium_agent::ToolCall;
use scriptorium_chat::exec::{self, Caller, FileScript, NO_REVIEW};
use scriptorium_review::file::FORMAT_HEADER;
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
      <Text>CESAR</Text>
    </Paragraph>
    <Paragraph Type="Dialogue">
      <Text>I am go to the town tomorrow.</Text>
    </Paragraph>
  </Content>
</FinalDraft>
"#;

/// A fresh folder holding `<sub>/film.fdx`; removed on drop.
struct Folder {
    base: PathBuf,
    dir: PathBuf,
}

impl Folder {
    fn new(name: &str) -> Self {
        Self::in_sub(name, "")
    }

    fn in_sub(name: &str, sub: &str) -> Self {
        let base = std::env::temp_dir().join(format!("scriptorium-chat-agent-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join(sub)).unwrap();
        let base = base.canonicalize().unwrap();
        let dir = base.join(sub);
        std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
        Folder { base, dir }
    }

    fn script(&self) -> String {
        self.dir.join("film.fdx").to_string_lossy().into_owned()
    }

    /// Tool `name` with `args` on `script`, as an outside agent: the
    /// result's text and whether it is an error.
    fn call_on(&self, script: &str, name: &str, mut args: Value) -> (String, bool) {
        if name != "guide" {
            args["script"] = json!(script);
        }
        let mut s = FileScript::new(Path::new(script)).unwrap();
        let call = ToolCall {
            id: "c".into(),
            name: name.into(),
            input: args,
            input_error: None,
            raw_input: None,
            extra: None,
        };
        let done = exec::run_call(&mut s, Caller::Agent, &call);
        assert!(!done.busy);
        (done.result.content, done.result.is_error)
    }

    fn call(&self, name: &str, args: Value) -> (String, bool) {
        self.call_on(&self.script(), name, args)
    }

    fn ok(&self, name: &str, args: Value) -> String {
        let (text, err) = self.call(name, args);
        assert!(!err, "{name} failed: {text}");
        text
    }

    fn session(&self) -> String {
        std::fs::read_to_string(self.dir.join("film.scriptorium.json")).unwrap_or_default()
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn suggest(review_file: &str, para: u64, option: &str) -> Value {
    json!({ "review_file": review_file, "paragraph_id": para, "options": [option] })
}

#[test]
fn a_missing_subfolder_is_made_with_the_review() {
    let f = Folder::new("subfolder");
    let out = f.ok("suggest_change", suggest("reviews/x.review.txt", 1, "Laura grins."));
    assert!(out.contains("#1"), "{out}");
    let text = std::fs::read_to_string(f.dir.join("reviews/x.review.txt")).unwrap();
    assert!(text.starts_with(&format!("{FORMAT_HEADER}\nScript: film.fdx\n")), "{text}");
    assert!(text.contains("Laura grins."), "{text}");
    assert!(f.session().contains("reviews/x.review.txt"), "{}", f.session());
    // Attached: the only review is the default, and named again it is the same one.
    let listed = f.ok("list_suggestions", json!({}));
    assert!(listed.contains("Laura grins."), "{listed}");
    f.ok("suggest_change", suggest("./reviews/x.review.txt", 3, "I am going to town tomorrow."));
    let text = std::fs::read_to_string(f.dir.join("reviews/x.review.txt")).unwrap();
    assert!(text.contains("going to town"), "{text}");
    let summary: Value = serde_json::from_str(&f.ok("list_reviews", json!({ "summary": true }))).unwrap();
    assert_eq!(summary.as_array().unwrap().len(), 1, "{summary}");
}

#[test]
fn an_append_that_fails_every_item_leaves_no_file_and_no_folder() {
    let f = Folder::new("all-fail");
    let (text, err) = f.call("suggest_change", suggest("reviews/x.review.txt", 999, "x"));
    assert!(err, "{text}");
    let out = f.ok(
        "suggest_changes",
        json!({ "review_file": "more/y.review.txt", "suggestions": [{ "paragraph_id": 999, "options": ["x"] }] }),
    );
    let out: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(out["added"], 0, "{out}");
    assert!(!f.dir.join("reviews").exists() && !f.dir.join("more").exists());
    assert!(!f.session().contains("review.txt"), "{}", f.session());
    // Nothing attached, so nothing to default to.
    let (text, err) = f.call("list_suggestions", json!({}));
    assert!(err, "{text}");
}

#[test]
fn a_new_reviews_id_is_the_clis() {
    // The CLI `tool`'s: the header it writes, an `Id:` hashed from it and
    // salted with the resolved path, so two new reviews never share one.
    let f = Folder::new("id");
    f.ok("suggest_change", suggest("a.review.txt", 1, "Laura grins."));
    f.ok("suggest_change", suggest("b.review.txt", 1, "Laura grins."));
    let id = |name: &str| {
        let text = std::fs::read_to_string(f.dir.join(name)).unwrap();
        let header = format!("{FORMAT_HEADER}\nScript: film.fdx\nId: ");
        assert!(text.starts_with(&header), "{text}");
        text[header.len()..].lines().next().unwrap().to_string()
    };
    let (a, b) = (id("a.review.txt"), id("b.review.txt"));
    assert!(a.len() == 12 && a.chars().all(|c| c.is_ascii_hexdigit()), "{a}");
    assert_ne!(a, b);
}

#[test]
fn an_existing_file_that_isnt_a_review_is_refused() {
    let f = Folder::new("not-review");
    std::fs::write(f.dir.join("config.review.txt"), "export PATH=/tmp\n").unwrap();
    let (text, err) = f.call("suggest_change", suggest("config.review.txt", 1, "Laura grins."));
    assert!(err && text.contains("review_file") && text.contains("ask the writer to attach it"), "{text}");
    assert_eq!(std::fs::read_to_string(f.dir.join("config.review.txt")).unwrap(), "export PATH=/tmp\n");
    assert!(!f.session().contains("config"), "{}", f.session());
    // An existing review is attached only with suggestions written into it,
    // as the CLI `tool` does: a failed append leaves it as it was, unattached.
    let empty = format!("{FORMAT_HEADER}\nScript: film.fdx\n\n");
    std::fs::write(f.dir.join("old.review.txt"), &empty).unwrap();
    let (text, err) = f.call("suggest_change", suggest("old.review.txt", 999, "x"));
    assert!(err, "{text}");
    let out = f.ok(
        "suggest_changes",
        json!({ "review_file": "old.review.txt", "suggestions": [{ "paragraph_id": 999, "options": ["x"] }] }),
    );
    assert!(out.contains("\"added\":0"), "{out}");
    assert_eq!(std::fs::read_to_string(f.dir.join("old.review.txt")).unwrap(), empty);
    assert!(!f.session().contains("old.review.txt"), "{}", f.session());
    f.ok("suggest_change", suggest("old.review.txt", 1, "Laura grins."));
    assert!(std::fs::read_to_string(f.dir.join("old.review.txt")).unwrap().contains("Laura grins."));
}

#[test]
fn without_review_file_the_only_attached_review_is_the_default() {
    let f = Folder::new("default");
    let args = json!({ "paragraph_id": 1, "options": ["Laura grins."] });
    let (text, err) = f.call("suggest_change", args.clone());
    assert!(err && text == NO_REVIEW, "{text}");
    f.ok("suggest_change", suggest("a.review.txt", 3, "I am going to town tomorrow."));
    f.ok("suggest_change", args.clone());
    assert!(std::fs::read_to_string(f.dir.join("a.review.txt")).unwrap().contains("Laura grins."));
    f.ok("suggest_change", suggest("b.review.txt", 3, "I go to town tomorrow."));
    let (text, err) = f.call("suggest_change", json!({ "paragraph_id": 1, "options": ["Laura beams."] }));
    assert!(err && text == NO_REVIEW, "{text}");
    // No thread review is ever made.
    let names: Vec<_> = std::fs::read_dir(&f.dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".review.txt"))
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
}

#[test]
fn offers_tools_by_caller() {
    let f = Folder::new("offer");
    // `guide` needs no script.
    let out = f.ok("guide", json!({ "topic": "review" }));
    assert!(!out.is_empty());
    // Agent-only arguments are offered to agents, made-up ones aren't.
    let (text, err) = f.call("read_scene", json!({ "scene": 1, "nope": 1 }));
    assert!(err && text.contains("nope"), "{text}");
    let out = f.ok("read_scene", json!({ "scene": 1 }));
    assert!(out.contains("¶1 Laura smiles."), "{out}");
    let (text, err) = f.call("no_such_tool", json!({}));
    assert!(err && text.contains("unknown tool"), "{text}");
}

// From the MCP server's tests (`crates/mcp/tests/mcp.rs`), the shared tools.

#[test]
fn suggest_changes_adds_valid_and_reports_failed() {
    let f = Folder::new("batch");
    let rows: Value = serde_json::from_str(&f.ok("list_paragraphs", json!({}))).unwrap();
    let id_of = |text: &str| {
        rows["paragraphs"].as_array().unwrap().iter().find(|p| p["text"] == text).unwrap()["id"].as_u64().unwrap()
    };
    let (action, dialogue) = (id_of("Laura smiles."), id_of("I am go to the town tomorrow."));
    let out = f.ok(
        "suggest_changes",
        json!({ "review_file": "batch.review.txt", "suggestions": [
            { "paragraph_id": action, "options": ["Laura grins."], "why": "livelier" },
            { "paragraph_id": 999, "options": ["x"] },
            { "paragraph_id": dialogue, "options": ["I am going to town tomorrow."] },
        ] }),
    );
    let result: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(result["added"], 2, "{out}");
    assert_eq!(result["failed"][0]["index"], 1, "{out}");
    let list: Value = serde_json::from_str(&f.ok("list_suggestions", json!({}))).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 2, "{list}");
    let summary: Value = serde_json::from_str(&f.ok("list_reviews", json!({ "summary": true }))).unwrap();
    assert_eq!(summary[0]["pending"], 2, "{summary}");
}

#[test]
fn review_files_are_confined_to_the_scripts_folder() {
    let f = Folder::in_sub("paths", "project/scripts");
    let scripts = f.dir.clone();
    let found: Value = serde_json::from_str(&f.ok("list_paragraphs", json!({ "speaker": "cesar" }))).unwrap();
    let id = found["paragraphs"][0]["id"].as_u64().unwrap();
    f.ok("suggest_change", suggest("pass.review.txt", id, "I am going to town tomorrow."));
    assert!(scripts.join("pass.review.txt").is_file());
    f.ok("suggest_change", suggest("reviews/dialogue.review.txt", id, "I am going into town tomorrow."));
    assert!(scripts.join("reviews/dialogue.review.txt").is_file());
    let (text, err) = f.call("suggest_change", suggest("../../nowhere/x.review.txt", id, "x y z"));
    assert!(err && text.contains("..") && text.contains("review_file"), "{text}");
    assert!(!f.base.join("nowhere").exists());

    std::fs::write(scripts.join("notes.txt"), "keep me\n").unwrap();
    std::fs::write(scripts.join("config.review.txt"), "export PATH=/tmp\n").unwrap();
    std::fs::write(f.base.join("outside.review.txt"), "SCRIPT REVIEW v1\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&f.base, scripts.join("up")).unwrap();
    let abs_new = f.base.join("new.review.txt").to_string_lossy().into_owned();
    let abs_old = f.base.join("outside.review.txt").to_string_lossy().into_owned();
    let inside = "inside the script's folder";
    let mut bad = vec![
        ("notes.txt", "end in .review.txt"),
        ("made/x.txt", "end in .review.txt"),
        ("config.review.txt", "ask the writer to attach it"),
        ("../outside.review.txt", "`..`"),
        ("reviews/../../outside.review.txt", "`..`"),
        (".bashrc.d/x.review.txt", "starting with `.`"),
        (abs_new.as_str(), inside),
        (abs_old.as_str(), inside),
    ];
    if cfg!(unix) {
        bad.extend([("up/new.review.txt", inside), ("up/made/new.review.txt", inside)]);
    }
    for (bad, says) in bad {
        let (text, err) = f.call("suggest_change", suggest(bad, id, "x y z"));
        assert!(err && text.contains(says), "{bad}: {text}");
    }
    assert_eq!(std::fs::read_to_string(scripts.join("notes.txt")).unwrap(), "keep me\n");
    assert_eq!(std::fs::read_to_string(scripts.join("config.review.txt")).unwrap(), "export PATH=/tmp\n");
    assert_eq!(std::fs::read_to_string(f.base.join("outside.review.txt")).unwrap(), "SCRIPT REVIEW v1\n");
    assert!(!scripts.join("made").exists() && !f.base.join("made").exists() && !f.base.join("new.review.txt").exists());
    assert!(!scripts.join(".bashrc.d").exists());
    let session = f.session();
    assert!(!session.contains("outside") && !session.contains("config"), "{session}");
}

#[test]
fn a_planted_session_never_aims_writes_at_other_files() {
    let f = Folder::in_sub("planted", "film");
    let scripts = f.dir.clone();
    f.ok("suggest_change", suggest("pass.review.txt", 1, "Laura grins."));
    std::fs::write(scripts.join("run.sh"), "#!/bin/sh\necho hello\n").unwrap();
    std::fs::write(f.base.join("outside.review.txt"), "SCRIPT REVIEW v1\n").unwrap();
    let session_path = scripts.join("film.scriptorium.json");
    let original: Value = serde_json::from_str(&std::fs::read_to_string(&session_path).unwrap()).unwrap();
    let script_before = std::fs::read_to_string(f.script()).unwrap();
    let outside = f.base.join("outside.review.txt").to_string_lossy().into_owned();
    for (target, says, remedy) in [
        ("run.sh", "doesn't start with SCRIPT REVIEW", None),
        ("film.fdx", "a script or one of its own files", None),
        (outside.as_str(), "is outside the script's folder", Some("copy it into that folder")),
    ] {
        let mut planted = original.clone();
        planted["reviews"] = json!([{ "path": target, "id": "zz", "text_hash": "0" }]);
        std::fs::write(&session_path, planted.to_string()).unwrap();
        let (text, err) = f.call("suggest_change", json!({ "paragraph_id": 1, "options": ["$(echo INJECTED >&2)"] }));
        assert!(err && text.contains(says), "{target}: {text}");
        match remedy {
            Some(r) => assert!(text.contains(r), "{target}: {text}"),
            None => assert!(!text.contains("The writer can"), "{target}: {text}"),
        }
        let (text, err) = f.call("suggest_change", suggest(target, 1, "$(echo INJECTED >&2)"));
        assert!(err, "{target}: {text}");
    }
    assert_eq!(std::fs::read_to_string(scripts.join("run.sh")).unwrap(), "#!/bin/sh\necho hello\n");
    assert_eq!(std::fs::read_to_string(f.script()).unwrap(), script_before);
    assert_eq!(std::fs::read_to_string(f.base.join("outside.review.txt")).unwrap(), "SCRIPT REVIEW v1\n");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(f.base.join("outside.review.txt"), scripts.join("link.review.txt")).unwrap();
        let mut planted = original.clone();
        planted["reviews"] = json!([{ "path": "link.review.txt", "id": "zz", "text_hash": "0" }]);
        std::fs::write(&session_path, planted.to_string()).unwrap();
        let (text, err) = f.call("suggest_change", json!({ "paragraph_id": 1, "options": ["Laura beams."] }));
        assert!(err && text.contains("outside the script's folder"), "{text}");
        assert_eq!(std::fs::read_to_string(f.base.join("outside.review.txt")).unwrap(), "SCRIPT REVIEW v1\n");
    }
    // A script that isn't a regular .fdx gets no store at all.
    std::fs::write(scripts.join(".bashrc"), "x").unwrap();
    assert!(FileScript::new(&scripts.join(".bashrc")).is_err());
}

#[test]
fn the_versions_tools_read_the_pair_and_rows_are_suggested_on_the_version() {
    let f = Folder::new("versions");
    let pair: Value = serde_json::from_str(&f.ok("list_versions", json!({}))).unwrap();
    assert_eq!(pair["pair"], Value::Null);
    let args = json!({ "language": "es", "original_language": "en" });
    let out = f.ok("new_version", args.clone());
    let es = out.lines().find_map(|l| l.strip_prefix("The version: ")).expect(&out).to_string();
    assert!(Path::new(&es).is_absolute() && es.ends_with("film.es.fdx"), "{out}");
    let again = f.ok("new_version", args);
    assert!(again.contains("was already made") && again.contains(&format!("The version: {es}")), "{again}");

    let rows: Value = serde_json::from_str(&f.call_on(&es, "read_rows", json!({ "needs": true })).0).unwrap();
    let row = &rows["rows"][1];
    assert_eq!(rows["rows"].as_array().unwrap().len(), 3, "heading, action, dialogue: {rows}");
    assert_eq!(row["other"][0]["text"], "Laura smiles.");
    assert!(!f.dir.join("film.es.scriptorium.json").exists(), "reading wrote no session");
    let batch = json!({
        "review_file": "Translation from English.review.txt",
        "suggestions": [{ "row": row["row"], "paragraph_id": row["this"][0]["id"], "options": ["Laura sonríe."] }],
    });
    let (out, err) = f.call_on(&es, "suggest_changes", batch);
    assert!(!err, "{out}");
    let added: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(added["added"], 1, "{added}");
    let (reviews, _) = f.call_on(&es, "list_reviews", json!({}));
    assert!(reviews.contains("Translation from English.review.txt"), "{reviews}");
    assert!(!f.session().contains("Translation"), "{}", f.session());
}

// The host operations (the tools only outside agents have), over
// `FileScript`: the MCP server's tests for them, and the CLI's texts.

#[test]
fn propose_attach_decide_undo_redo_write() {
    let f = Folder::new("ops");
    f.ok("suggest_change", suggest("pass.review.txt", 3, "I am going to town tomorrow."));
    // Attached already: attaching again says so, with its count.
    let out = f.ok("attach_reviews", json!({ "files": ["pass.review.txt"] }));
    assert_eq!(out, "attached pass.review.txt (1 suggestions)");
    let out = f.ok("decide_suggestion", json!({ "n": 1, "decision": "accept" }));
    assert!(out.starts_with("#1 ") && out.ends_with(": I am going to town tomorrow."), "{out}");
    let out = f.ok("undo", json!({}));
    assert_eq!(out, "¶3: I am go to the town tomorrow.");
    let out = f.ok("redo", json!({}));
    assert_eq!(out, "¶3: I am going to town tomorrow.");
    let (text, err) = f.call("redo", json!({}));
    assert!(err && text == "nothing to redo", "{text}");
    let written = f.ok("write_script", json!({}));
    let out_path = f.dir.join("film (scriptorium).fdx");
    assert_eq!(written, format!("wrote {} (1 paragraphs changed)", out_path.display()));
    assert!(std::fs::read_to_string(&out_path).unwrap().contains("I am going to town tomorrow."));
    // The decision was saved before the reply.
    assert!(f.session().contains("pass.review.txt"), "{}", f.session());

    let (text, err) = f.call("edit_paragraph", json!({ "paragraph_id": 999, "text": "x" }));
    assert!(err && text.contains("use list_paragraphs to find ids") && !text.contains("scriptorium"), "{text}");
    let (text, err) = f.call("edit_paragraph", json!({ "paragraph_id": 1, "text": "a\nb" }));
    assert!(err && text.contains("newlines"), "{text}");
}

#[test]
fn undo_undoes_the_last_step_whoever_made_it() {
    let f = Folder::new("undo-any");
    assert_eq!(f.ok("edit_paragraph", json!({ "paragraph_id": 1, "text": "Laura grins." })), "¶1: Laura grins.");
    let scene = f.ok("read_scene", json!({ "scene": 1 }));
    assert!(scene.contains("Laura grins."), "{scene}");
    // The edit (anyone's: the session holds one history) is the step undone.
    assert_eq!(f.ok("undo", json!({})), "¶1: Laura smiles.");
    let (text, err) = f.call("undo", json!({}));
    assert!(err && text == "nothing to undo", "{text}");
}

#[test]
fn reads_live_text_after_decide() {
    let f = Folder::new("live");
    f.ok("suggest_change", suggest("pass.review.txt", 3, "I am going to town tomorrow."));
    f.ok("decide_suggestion", json!({ "n": 1, "decision": "accept" }));
    let scene = f.ok("read_scene", json!({ "paragraph_id": 3 }));
    assert!(scene.contains("I am going to town tomorrow.") && !scene.contains("I am go to"), "{scene}");
    // Undecided by its stable entry, the line reads as written again.
    f.ok("decide_suggestion", json!({ "n": "e0", "decision": "undecide" }));
    let scene = f.ok("read_scene", json!({ "scene": 1 }));
    assert!(scene.contains("I am go to the town tomorrow."), "{scene}");
}

#[test]
fn decide_takes_only_positive_numbers_or_entries() {
    let f = Folder::new("refs");
    f.ok("suggest_change", suggest("pass.review.txt", 3, "I am going to town tomorrow."));
    for bad in [json!(0), json!(-1), json!(1.5), json!("1"), json!("x"), json!("e"), json!(null)] {
        let (text, err) = f.call("decide_suggestion", json!({ "n": bad, "decision": "keep" }));
        assert!(err && text.contains("`n` must be a suggestion number from 1"), "{bad}: {text}");
    }
    let (text, err) = f.call("decide_suggestion", json!({ "n": 1, "decision": "accept", "option": 0 }));
    assert!(err && text.contains("`option`"), "{text}");
    let (text, err) = f.call("decide_suggestion", json!({ "n": 1, "decision": "maybe" }));
    assert!(err && text.contains("unknown decision"), "{text}");
    let (text, err) = f.call("decide_suggestion", json!({ "n": 2, "decision": "keep" }));
    assert!(err && text.contains("no suggestion 2 (1 to 1)"), "{text}");
    f.ok("decide_suggestion", json!({ "n": "e0", "decision": "keep" }));
}

#[test]
fn a_block_is_rewritten_with_lines() {
    let f = Folder::new("blocks");
    let out = f.ok(
        "suggest_changes",
        json!({ "review_file": "blocks.review.txt", "suggestions": [
            { "paragraph_ids": [2, 3], "options": [[
                { "kind": "Character", "text": "CESAR" },
                { "kind": "Dialogue", "text": "I'm going to town tomorrow." },
            ]] },
        ] }),
    );
    assert!(out.contains("\"added\": 1") || out.contains("\"added\":1"), "{out}");
    let (text, err) = f.call("decide_suggestion", json!({ "n": 1, "decision": "rewrite", "text": "x" }));
    assert!(err && text.contains("that suggestion is a block"), "{text}");
    let (text, err) = f.call(
        "decide_suggestion",
        json!({ "n": 1, "decision": "rewrite", "lines": [{ "kind": "Action", "text": "a\nb" }] }),
    );
    assert!(err && text.contains("can't contain a newline"), "{text}");
    let out = f.ok(
        "decide_suggestion",
        json!({ "n": 1, "decision": "rewrite", "lines": [{ "kind": "Action", "text": "Cesar nods." }] }),
    );
    assert!(out.contains("ACTION: Cesar nods."), "{out}");
    let scene = f.ok("read_scene", json!({ "scene": 1 }));
    assert!(scene.contains("Cesar nods.") && !scene.contains("I am go"), "{scene}");
}

#[test]
fn host_paths_are_confined_to_the_scripts_folder() {
    let f = Folder::in_sub("host-paths", "project/scripts");
    let scripts = f.dir.clone();
    f.ok("suggest_change", suggest("pass.review.txt", 3, "I am going to town tomorrow."));
    f.ok("suggest_change", suggest("reviews/dialogue.review.txt", 3, "I am going into town tomorrow."));
    std::fs::write(scripts.join("notes.txt"), "keep me\n").unwrap();
    std::fs::write(scripts.join("config.review.txt"), "export PATH=/tmp\n").unwrap();
    std::fs::write(f.base.join("outside.review.txt"), "SCRIPT REVIEW v1\n").unwrap();
    std::fs::create_dir_all(scripts.join(".hidden")).unwrap();
    std::fs::write(scripts.join(".hidden/h.review.txt"), "SCRIPT REVIEW v1\n").unwrap();
    let abs_old = f.base.join("outside.review.txt").to_string_lossy().into_owned();
    let inside = "inside the script's folder";

    // `~` is the home folder, never a folder of that name here.
    let (text, err) = f.call("suggest_change", suggest("~/scriptorium-chat-test-never.review.txt", 3, "x y z"));
    assert!(err && text.contains(inside), "{text}");
    assert!(!scripts.join("~").exists());

    for (files, says) in [
        (json!([abs_old]), inside),
        (json!(["../outside.review.txt"]), "`..`"),
        (json!(["config.review.txt"]), "ask the writer to attach it"),
        (json!(["notes.txt"]), "end in .review.txt"),
        (json!([".hidden/h.review.txt"]), "starting with `.`"),
        (json!(["missing.review.txt"]), "no such file"),
    ] {
        let (text, err) = f.call("attach_reviews", json!({ "files": files }));
        assert!(err && text.contains(says), "{files}: {text}");
        let (text, err) = f.call("detach_reviews", json!({ "files": files }));
        assert!(err && text.contains("isn't attached"), "{files}: {text}");
    }
    let (text, err) = f.call("attach_reviews", json!({ "files": [] }));
    assert!(err && text.contains("give at least one file"), "{text}");
    let session = f.session();
    assert!(!session.contains("outside") && !session.contains("config") && !session.contains("hidden"), "{session}");

    // decide's `review` file: never attached from outside.
    let (text, err) = f.call("decide_suggestion", json!({ "review": abs_old, "n": 1, "decision": "accept" }));
    assert!(err && text.contains("no attached review matches"), "{text}");
    assert!(!f.session().contains("outside"));
    let (text, err) = f.call("decide_suggestion", json!({ "n": 1, "decision": "accept" }));
    assert!(err && text.contains("name one with review") && !text.contains("--"), "{text}");

    // Relative to the script's folder, not the process's.
    let out = f.ok("detach_reviews", json!({ "files": ["reviews/dialogue.review.txt"] }));
    let at = scripts.join("reviews/dialogue.review.txt");
    assert_eq!(out, format!("removed {} (the script's text keeps its decisions)", at.display()));
    let out = f.ok("attach_reviews", json!({ "files": ["reviews/dialogue.review.txt"] }));
    assert_eq!(out, "attached reviews/dialogue.review.txt (1 suggestions)");
    let out = f.ok(
        "decide_suggestion",
        json!({ "review": "reviews/dialogue.review.txt", "n": 1, "decision": "rewrite", "text": "--review x" }),
    );
    assert!(out.contains("rewritten: --review x"), "{out}");

    // write_script: a .fdx in the script's folder; never over the script.
    f.ok("write_script", json!({ "output": "final.fdx" }));
    assert!(scripts.join("final.fdx").is_file());
    std::fs::write(f.base.join("profile"), "keep me\n").unwrap();
    let abs_out = f.base.join("out.fdx").to_string_lossy().into_owned();
    let mut bad_out = vec![
        ("film.fdx", "another name"),
        ("final.fdx", "another name"),
        ("../out.fdx", "`..`"),
        (abs_out.as_str(), inside),
        ("notes.txt", "end in .fdx"),
        (".hidden/out.fdx", "starting with `.`"),
        ("~/scriptorium-chat-test-never.fdx", inside),
    ];
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(f.base.join("profile"), scripts.join("link.fdx")).unwrap();
        bad_out.push(("link.fdx", "link"));
    }
    for (bad, says) in bad_out {
        let (text, err) = f.call("write_script", json!({ "output": bad }));
        assert!(err && text.contains(says), "{bad}: {text}");
    }
    assert_eq!(std::fs::read_to_string(f.base.join("profile")).unwrap(), "keep me\n");
    assert!(!f.base.join("out.fdx").exists());
    assert_eq!(std::fs::read_to_string(f.script()).unwrap(), SCRIPT);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(f.base.join("profile"), scripts.join("film (scriptorium).fdx")).unwrap();
        let (text, err) = f.call("write_script", json!({}));
        assert!(err && text.contains("link"), "{text}");
        assert_eq!(std::fs::read_to_string(f.base.join("profile")).unwrap(), "keep me\n");
    }
}

#[test]
fn write_script_never_replaces_another_screenplay() {
    let f = Folder::new("other-fdx");
    std::fs::write(f.dir.join("other.fdx"), SCRIPT.replace("Laura", "Ana")).unwrap();
    let before = std::fs::read_to_string(f.dir.join("other.fdx")).unwrap();
    let (text, err) = f.call("write_script", json!({ "output": "other.fdx" }));
    assert!(err && text.contains("other.fdx already exists and isn't this script's own output"), "{text}");
    assert_eq!(std::fs::read_to_string(f.dir.join("other.fdx")).unwrap(), before);
    // Its own default output is replaced.
    f.ok("write_script", json!({}));
    f.ok("write_script", json!({}));
}

#[test]
fn a_planted_session_can_still_lose_a_review() {
    let f = Folder::new("planted-detach");
    f.ok("suggest_change", suggest("pass.review.txt", 1, "Laura grins."));
    std::fs::write(f.dir.join("run.sh"), "#!/bin/sh\necho hello\n").unwrap();
    let session_path = f.dir.join("film.scriptorium.json");
    let mut planted: Value = serde_json::from_str(&f.session()).unwrap();
    planted["reviews"] = json!([{ "path": "run.sh", "id": "zz", "text_hash": "0" }]);
    std::fs::write(&session_path, planted.to_string()).unwrap();
    let out = f.ok("detach_reviews", json!({ "files": ["run.sh"] }));
    assert!(out.starts_with("removed "), "{out}");
    assert!(!f.session().contains("run.sh"), "{}", f.session());
    assert_eq!(std::fs::read_to_string(f.dir.join("run.sh")).unwrap(), "#!/bin/sh\necho hello\n");
}

#[test]
fn the_made_version_is_written_only_with_an_output() {
    let f = Folder::new("made-write");
    let out = f.ok("new_version", json!({ "language": "es", "original_language": "en" }));
    // An outside agent names the script in each call: it goes on with the version's path.
    assert!(out.contains("From now on, pass this path as `script`"), "{out}");
    let es = out.lines().find_map(|l| l.strip_prefix("The version: ")).expect(&out).to_string();
    let (text, err) = f.call_on(&es, "write_script", json!({}));
    assert!(err && text.contains("the writer saves it in the app"), "{text}");
    let (text, err) = f.call_on(&es, "write_script", json!({ "output": "copy.fdx" }));
    assert!(!err, "{text}");
    assert!(f.dir.join("copy.fdx").is_file());
}

#[cfg(unix)]
#[test]
fn write_script_follows_a_linked_script_to_its_folder() {
    let f = Folder::in_sub("host-linked", "real");
    std::fs::create_dir_all(f.base.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(f.dir.join("film.fdx"), f.base.join("elsewhere/film.fdx")).unwrap();
    let link = f.base.join("elsewhere/film.fdx").to_string_lossy().into_owned();
    let (out, err) = f.call_on(&link, "write_script", json!({}));
    assert!(!err && f.dir.join("film (scriptorium).fdx").is_file(), "{out}");
    assert!(!f.base.join("elsewhere/film (scriptorium).fdx").exists());
    let (out, err) = f.call_on(&link, "write_script", json!({}));
    assert!(!err, "{out}");
}
