//! The server driven over stdio as an MCP client would: handshake, list tools,
//! then a review round trip on a small script in a temporary folder.
//!
//! The server runs the CLI library in-process: no `scriptorium` binary needed.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

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

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    dir: PathBuf,
}

impl Server {
    /// The script `film.fdx` in a new folder, with the server running there.
    fn start(name: &str) -> Self {
        Self::start_in(name, "", "")
    }

    /// The script in `dir/<script_sub>`, the server running in `dir/<cwd_sub>`
    /// (as in Cowork or Claude Desktop, where the server's folder isn't the
    /// script's).
    fn start_in(name: &str, script_sub: &str, cwd_sub: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("scriptorium-mcp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(script_sub)).unwrap();
        std::fs::create_dir_all(dir.join(cwd_sub)).unwrap();
        std::fs::write(dir.join(script_sub).join("film.fdx"), SCRIPT).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_scriptorium-mcp"))
            .current_dir(dir.join(cwd_sub))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self { child, stdin, stdout, next_id: 1, dir }
    }

    fn send(&mut self, msg: &Value) {
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }

    /// A tool call's text, and whether it was an error.
    fn call(&mut self, name: &str, args: Value) -> (String, bool) {
        let r = self.request("tools/call", json!({ "name": name, "arguments": args }));
        let result = &r["result"];
        (result["content"][0]["text"].as_str().unwrap_or("").to_string(), result["isError"] == true)
    }

    fn ok(&mut self, name: &str, args: Value) -> String {
        let (text, err) = self.call(name, args);
        assert!(!err, "{name} failed: {text}");
        text
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn handshake_and_tool_list() {
    let mut s = Server::start("handshake");
    let init = s.request("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }));
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "scriptorium");
    // A notification gets no reply: the next line read must answer the next request.
    s.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    let tools = s.request("tools/list", json!({}));
    let names: Vec<&str> =
        tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for want in [
        "list_paragraphs",
        "read_scene",
        "read_script",
        "suggest_change",
        "suggest_changes",
        "decide_suggestion",
        "undo",
        "write_script",
    ] {
        assert!(names.contains(&want), "{want} missing from {names:?}");
    }
    let suggest = tools["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "suggest_change").unwrap();
    assert_eq!(suggest["inputSchema"]["properties"]["options"]["maxItems"], 3);
    assert!(suggest["inputSchema"]["properties"]["before"].is_object());
    let decide =
        tools["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "decide_suggestion").unwrap();
    assert_eq!(
        decide["inputSchema"]["properties"]["lines"]["items"]["properties"]["kind"]["enum"],
        suggest["inputSchema"]["properties"]["options"]["items"]["anyOf"][1]["items"]["properties"]["kind"]["enum"]
    );
    assert!(s.request("no/such/method", json!({}))["error"]["code"] == -32601);
}

#[test]
fn propose_decide_undo_write() {
    let mut s = Server::start("review");

    let found: Value =
        serde_json::from_str(&s.ok("list_paragraphs", json!({ "script": "film.fdx", "speaker": "cesar" }))).unwrap();
    assert_eq!(found["total"], 1);
    let id = found["paragraphs"][0]["id"].as_u64().unwrap();

    let added = s.ok(
        "suggest_change",
        json!({ "script": "film.fdx", "review_file": "pass.review.txt", "paragraph_id": id,
                "options": ["I am going to town tomorrow."], "why": "verb form" }),
    );
    assert!(added.contains("#1"), "{added}");
    // suggest_change attached the file; attaching it again changes nothing.
    let file = std::fs::read_to_string(s.dir.join("pass.review.txt")).unwrap();
    assert!(file.contains("Id: ") && file.contains(&format!("PARA: {id}")), "{file}");
    s.ok("attach_reviews", json!({ "script": "film.fdx", "files": ["pass.review.txt"] }));

    let list: Value =
        serde_json::from_str(&s.ok("list_suggestions", json!({ "script": "film.fdx", "pending_only": true }))).unwrap();
    assert_eq!(list[0]["options"][0], "I am going to town tomorrow.");

    s.ok("decide_suggestion", json!({ "script": "film.fdx", "n": 1, "decision": "accept" }));
    s.ok("undo", json!({ "script": "film.fdx" }));
    s.ok("redo", json!({ "script": "film.fdx" }));
    let written = s.ok("write_script", json!({ "script": "film.fdx" }));
    assert!(written.contains("1 paragraphs changed"), "{written}");
    let out = std::fs::read_to_string(s.dir.join("film (scriptorium).fdx")).unwrap();
    assert!(out.contains("I am going to town tomorrow."));

    // Failures come back as tool results the model can read.
    let (text, err) = s.call("edit_paragraph", json!({ "script": "film.fdx", "paragraph_id": 999, "text": "x" }));
    assert!(err && text.contains("no paragraph 999"), "{text}");
    let (text, err) =
        s.call("suggest_change", json!({ "script": "film.fdx", "review_file": "pass.review.txt", "options": ["x"] }));
    assert!(err && text.contains("give exactly one of paragraph_id"), "{text}");
}

#[test]
fn mcp_reads_live_text_after_decide() {
    let mut s = Server::start("live");
    let found: Value =
        serde_json::from_str(&s.ok("list_paragraphs", json!({ "script": "film.fdx", "speaker": "cesar" }))).unwrap();
    let id = found["paragraphs"][0]["id"].as_u64().unwrap();
    s.ok(
        "suggest_change",
        json!({ "script": "film.fdx", "review_file": "pass.review.txt", "paragraph_id": id,
                "options": ["I am going to town tomorrow."] }),
    );
    s.ok("decide_suggestion", json!({ "script": "film.fdx", "n": 1, "decision": "accept" }));

    let scene = s.ok("read_scene", json!({ "script": "film.fdx", "paragraph_id": id }));
    assert!(scene.contains(&format!("¶{id} ")) && scene.contains("I am going to town tomorrow."), "{scene}");
    assert!(!scene.contains("I am go to"), "{scene}");
    let script = s.ok("read_script", json!({ "script": "film.fdx" }));
    assert!(script.contains("I am going to town tomorrow."), "{script}");
    let found: Value =
        serde_json::from_str(&s.ok("list_paragraphs", json!({ "script": "film.fdx", "contains": "going to town" })))
            .unwrap();
    assert_eq!(found["total"], 1);

    // Undecided by its stable entry, the line reads as written again.
    s.ok("decide_suggestion", json!({ "script": "film.fdx", "n": "e0", "decision": "undecide" }));
    let scene = s.ok("read_scene", json!({ "script": "film.fdx", "scene": 1 }));
    assert!(scene.contains("I am go to the town tomorrow."), "{scene}");
}

#[test]
fn suggest_changes_adds_valid_and_reports_failed() {
    let mut s = Server::start("batch");
    let rows: Value = serde_json::from_str(&s.ok("list_paragraphs", json!({ "script": "film.fdx" }))).unwrap();
    let id_of = |text: &str| {
        rows["paragraphs"].as_array().unwrap().iter().find(|p| p["text"] == text).unwrap()["id"].as_u64().unwrap()
    };
    let (action, dialogue) = (id_of("Laura smiles."), id_of("I am go to the town tomorrow."));
    let out = s.ok(
        "suggest_changes",
        json!({ "script": "film.fdx", "review_file": "batch.review.txt", "suggestions": [
            { "paragraph_id": action, "options": ["Laura grins."], "why": "livelier" },
            { "paragraph_id": 999, "options": ["x"] },
            { "paragraph_id": dialogue, "options": ["I am going to town tomorrow."] },
        ] }),
    );
    let result: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(result["added"], 2, "{out}");
    assert_eq!(result["failed"][0]["index"], 1, "{out}");

    // Auto-attached: the suggestions are listed without naming the review.
    let list: Value = serde_json::from_str(&s.ok("list_suggestions", json!({ "script": "film.fdx" }))).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 2, "{list}");
    let summary: Value =
        serde_json::from_str(&s.ok("list_reviews", json!({ "script": "film.fdx", "summary": true }))).unwrap();
    assert_eq!(summary[0]["pending"], 2, "{summary}");
}

#[test]
fn paths_are_relative_to_the_script_not_the_server() {
    let mut s = Server::start_in("paths", "project/scripts", "elsewhere");
    let scripts = s.dir.join("project/scripts");
    let script = scripts.join("film.fdx").to_string_lossy().to_string();
    let found: Value =
        serde_json::from_str(&s.ok("list_paragraphs", json!({ "script": script, "speaker": "cesar" }))).unwrap();
    let id = found["paragraphs"][0]["id"].as_u64().unwrap();

    // review_file: next to the script, not in the server's folder.
    s.ok(
        "suggest_change",
        json!({ "script": script, "review_file": "pass.review.txt", "paragraph_id": id,
                "options": ["I am going to town tomorrow."] }),
    );
    assert!(scripts.join("pass.review.txt").is_file());
    assert!(!s.dir.join("elsewhere/pass.review.txt").exists());

    // A missing folder inside the script's folder is created; outside, refused.
    s.ok(
        "suggest_change",
        json!({ "script": script, "review_file": "reviews/dialogue.review.txt", "paragraph_id": id,
                "options": ["I am going into town tomorrow."] }),
    );
    assert!(scripts.join("reviews/dialogue.review.txt").is_file());
    let (text, err) = s.call(
        "suggest_change",
        json!({ "script": script, "review_file": "../../nowhere/x.review.txt", "paragraph_id": id,
                "options": ["x y z"] }),
    );
    assert!(err && text.contains("..") && text.contains("review_file"), "{text}");
    assert!(!s.dir.join("nowhere").exists());
    // Not attached: only a review inside the script's folder. Nothing
    // outside is created, attached or written, and no folder is made. Each
    // refusal says why.
    std::fs::write(scripts.join("notes.txt"), "keep me\n").unwrap();
    std::fs::write(scripts.join("config.review.txt"), "export PATH=/tmp\n").unwrap();
    std::fs::write(s.dir.join("outside.review.txt"), "SCRIPT REVIEW v1\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&s.dir, scripts.join("up")).unwrap();
    let abs_new = s.dir.join("new.review.txt").to_string_lossy().into_owned();
    let abs_old = s.dir.join("outside.review.txt").to_string_lossy().into_owned();
    let inside = "inside the script's folder";
    let mut bad = vec![
        ("notes.txt", "end in .review.txt"),
        ("made/x.txt", "end in .review.txt"),
        ("config.review.txt", "ask the writer to attach it"),
        ("../outside.review.txt", "`..`"),
        ("reviews/../../outside.review.txt", "`..`"),
        ("~/scriptorium-mcp-test-never.review.txt", inside),
        (".bashrc.d/x.review.txt", "starting with `.`"),
        (abs_new.as_str(), inside),
        (abs_old.as_str(), inside),
    ];
    if cfg!(unix) {
        bad.extend([("up/new.review.txt", inside), ("up/made/new.review.txt", inside)]);
    }
    for (bad, says) in bad {
        let (text, err) = s.call(
            "suggest_change",
            json!({ "script": script, "review_file": bad, "paragraph_id": id, "options": ["x y z"] }),
        );
        assert!(err && text.contains(says), "{bad}: {text}");
    }
    assert_eq!(std::fs::read_to_string(scripts.join("notes.txt")).unwrap(), "keep me\n");
    assert_eq!(std::fs::read_to_string(scripts.join("config.review.txt")).unwrap(), "export PATH=/tmp\n");
    assert_eq!(std::fs::read_to_string(s.dir.join("outside.review.txt")).unwrap(), "SCRIPT REVIEW v1\n");
    assert!(!scripts.join("made").exists() && !s.dir.join("made").exists() && !s.dir.join("new.review.txt").exists());
    assert!(!scripts.join(".bashrc.d").exists());
    if let Some(home) = std::env::var_os("HOME") {
        assert!(!Path::new(&home).join("scriptorium-mcp-test-never.review.txt").exists());
    }

    // attach_reviews and detach_reviews are the model's choice too: only
    // reviews in the script's folder, and an attached one must read as one.
    std::fs::create_dir_all(scripts.join(".hidden")).unwrap();
    std::fs::write(scripts.join(".hidden/h.review.txt"), "SCRIPT REVIEW v1\n").unwrap();
    for (files, says) in [
        (json!([abs_old]), inside),
        (json!(["../outside.review.txt"]), "`..`"),
        (json!(["config.review.txt"]), "ask the writer to attach it"),
        (json!(["notes.txt"]), "end in .review.txt"),
        (json!([".hidden/h.review.txt"]), "starting with `.`"),
    ] {
        let (text, err) = s.call("attach_reviews", json!({ "script": script, "files": files }));
        assert!(err && text.contains(says), "{files}: {text}");
        // Detaching only edits the session: these aren't attached.
        let (text, err) = s.call("detach_reviews", json!({ "script": script, "files": files }));
        assert!(err && text.contains("isn't attached"), "{files}: {text}");
    }
    let session = std::fs::read_to_string(scripts.join("film.scriptorium.json")).unwrap();
    assert!(!session.contains("outside") && !session.contains("config") && !session.contains("hidden"), "{session}");
    assert_eq!(std::fs::read_to_string(s.dir.join("outside.review.txt")).unwrap(), "SCRIPT REVIEW v1\n");
    // decide_suggestion's `review` file: never attached from outside.
    let (text, err) =
        s.call("decide_suggestion", json!({ "script": script, "review": abs_old, "n": 1, "decision": "accept" }));
    assert!(err && text.contains("no attached review matches"), "{text}");
    let session = std::fs::read_to_string(scripts.join("film.scriptorium.json")).unwrap();
    assert!(!session.contains("outside"), "{session}");
    assert_eq!(std::fs::read_to_string(s.dir.join("outside.review.txt")).unwrap(), "SCRIPT REVIEW v1\n");

    // Two reviews attached: the error names the parameter, not a CLI flag.
    let (text, err) = s.call("decide_suggestion", json!({ "script": script, "n": 1, "decision": "accept" }));
    assert!(err && text.contains("name one with review") && !text.contains("--"), "{text}");

    // detach and attach by paths relative to the script.
    s.ok("detach_reviews", json!({ "script": script, "files": ["reviews/dialogue.review.txt"] }));
    s.ok("attach_reviews", json!({ "script": script, "files": ["reviews/dialogue.review.txt"] }));

    // `review` as a file relative to the script; rewrite text that looks like a flag is just text.
    let out = s.ok(
        "decide_suggestion",
        json!({ "script": script, "review": "reviews/dialogue.review.txt", "n": 1, "decision": "rewrite",
                "text": "--review pass.review.txt" }),
    );
    assert!(out.contains("rewritten: --review pass.review.txt"), "{out}");

    // A `review` name that's a file only in the server's folder is never attached.
    let stray = "SCRIPT REVIEW v1\nScript: film.fdx\n\n---\nBEFORE: Laura smiles.\nAFTER: Laura grins.\n";
    std::fs::write(s.dir.join("elsewhere/stray.review.txt"), stray).unwrap();
    let (text, err) = s.call(
        "decide_suggestion",
        json!({ "script": script, "review": "stray.review.txt", "n": 1, "decision": "accept" }),
    );
    assert!(err && text.contains("no attached review matches"), "{text}");
    let session = std::fs::read_to_string(scripts.join("film.scriptorium.json")).unwrap();
    assert!(!session.contains("stray"), "{session}");

    // write_script output: in the script's folder; never over the script.
    let out = s.ok("write_script", json!({ "script": script, "output": "final.fdx" }));
    assert!(scripts.join("final.fdx").is_file(), "{out}");
    let (text, err) = s.call("write_script", json!({ "script": script, "output": "film.fdx" }));
    assert!(err && text.contains("give an output with another name"), "{text}");
    // Only a .fdx inside the script's folder, never through a link.
    std::fs::write(s.dir.join("profile"), "keep me\n").unwrap();
    let abs_out = s.dir.join("out.fdx").to_string_lossy().into_owned();
    let mut bad_out = vec![
        ("../out.fdx", "`..`"),
        (abs_out.as_str(), inside),
        ("notes.txt", "end in .fdx"),
        (".hidden/out.fdx", "starting with `.`"),
        ("~/scriptorium-mcp-test-never.fdx", inside),
    ];
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(s.dir.join("profile"), scripts.join("link.fdx")).unwrap();
        bad_out.push(("link.fdx", "link"));
    }
    for (bad, says) in bad_out {
        let (text, err) = s.call("write_script", json!({ "script": script, "output": bad }));
        assert!(err && text.contains(says), "{bad}: {text}");
    }
    assert_eq!(std::fs::read_to_string(s.dir.join("profile")).unwrap(), "keep me\n");
    assert!(!s.dir.join("out.fdx").exists());
    assert_eq!(std::fs::read_to_string(scripts.join("notes.txt")).unwrap(), "keep me\n");
    // Never another screenplay: an existing .fdx other than the default
    // output is refused (on a case-insensitive disk, `FILM.fdx` is the
    // script itself: `same_file` compares the file, not the name).
    std::fs::write(scripts.join("sibling.fdx"), "<FinalDraft/>").unwrap();
    for other in ["sibling.fdx", "final.fdx"] {
        let (text, err) = s.call("write_script", json!({ "script": script, "output": other }));
        assert!(err && text.contains("another name"), "{other}: {text}");
    }
    assert_eq!(std::fs::read_to_string(scripts.join("sibling.fdx")).unwrap(), "<FinalDraft/>");
    // The default name too: a link planted there is refused.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(s.dir.join("profile"), scripts.join("film (scriptorium).fdx")).unwrap();
        let (text, err) = s.call("write_script", json!({ "script": script }));
        assert!(err && text.contains("link"), "{text}");
        assert_eq!(std::fs::read_to_string(s.dir.join("profile")).unwrap(), "keep me\n");
    }

    let (text, err) = s.call("edit_paragraph", json!({ "script": script, "paragraph_id": 999, "text": "x" }));
    assert!(err && text.contains("use list_paragraphs to find ids") && !text.contains("scriptorium"), "{text}");
}

#[test]
fn decide_takes_only_positive_numbers_or_entries() {
    let mut s = Server::start("refs");
    let found: Value =
        serde_json::from_str(&s.ok("list_paragraphs", json!({ "script": "film.fdx", "speaker": "cesar" }))).unwrap();
    let id = found["paragraphs"][0]["id"].as_u64().unwrap();
    s.ok(
        "suggest_change",
        json!({ "script": "film.fdx", "review_file": "pass.review.txt", "paragraph_id": id,
                "options": ["I am going to town tomorrow."] }),
    );
    for bad in [json!(0), json!(-1), json!(1.5), json!("1"), json!("x"), json!("e"), json!(null)] {
        let (text, err) = s.call("decide_suggestion", json!({ "script": "film.fdx", "n": bad, "decision": "keep" }));
        assert!(err && text.contains("`n` must be a suggestion number from 1"), "{bad}: {text}");
    }
    let (text, err) =
        s.call("decide_suggestion", json!({ "script": "film.fdx", "n": 1, "decision": "accept", "option": 0 }));
    assert!(err && text.contains("`option`"), "{text}");
    s.ok("decide_suggestion", json!({ "script": "film.fdx", "n": "e0", "decision": "keep" }));
}

#[test]
fn suggest_changes_with_a_block_and_a_cut_then_list_and_rewrite() {
    let mut s = Server::start("blocks");
    // ¶0 heading, ¶1 action, ¶2 cue, ¶3 dialogue.
    let out = s.ok(
        "suggest_changes",
        json!({ "script": "film.fdx", "review_file": "blocks.review.txt", "suggestions": [
            { "paragraph_ids": [2, 3], "options": [[
                { "kind": "Character", "text": "CESAR" },
                { "kind": "Dialogue", "text": "I'm going to town tomorrow." },
            ]], "why": "verb form" },
            { "paragraph_id": 1, "options": [[]], "why": "not needed" },
        ] }),
    );
    let result: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(result["added"], 2, "{out}");
    assert_eq!(result["suggestions"][0]["paragraph_ids"], json!([2, 3]), "{out}");
    assert_eq!(result["suggestions"][0]["shape"], "block", "{out}");

    let list: Value = serde_json::from_str(&s.ok("list_suggestions", json!({ "script": "film.fdx" }))).unwrap();
    let rows = list.as_array().unwrap();
    assert_eq!(rows.len(), 2, "{list}");
    assert_eq!(rows[0]["paragraph_ids"], json!([1]), "{list}");
    assert_eq!(rows[0]["options"], json!([[]]), "a cut: {list}");
    assert_eq!(rows[1]["paragraph_ids"], json!([2, 3]), "{list}");
    assert_eq!(rows[1]["before"][1], json!({ "kind": "Dialogue", "text": "I am go to the town tomorrow." }));
    assert_eq!(rows[1]["options"][0][1]["text"], "I'm going to town tomorrow.");

    // A block rewritten with lines when the writer asks.
    s.ok(
        "decide_suggestion",
        json!({ "script": "film.fdx", "n": 2, "decision": "rewrite",
                "lines": [{ "kind": "Action", "text": "Cesar nods." }] }),
    );
    let scene = s.ok("read_scene", json!({ "script": "film.fdx", "scene": 1 }));
    assert!(scene.contains("Cesar nods.") && !scene.contains("I am go"), "{scene}");
    // Both shapes refuse string options where lines are needed.
    let (text, err) = s.call(
        "suggest_change",
        json!({ "script": "film.fdx", "review_file": "other.review.txt", "insert_after": 0, "options": ["x"] }),
    );
    assert!(err && text.contains("list of lines"), "{text}");
}

#[test]
fn a_planted_session_never_aims_writes_at_other_files() {
    // A session file in the script's folder (planted with the script, say)
    // listing a shell script, the script itself and a review outside.
    let mut s = Server::start_in("planted", "film", "film");
    let scripts = s.dir.join("film");
    let script = scripts.join("film.fdx").to_string_lossy().into_owned();
    s.ok(
        "suggest_change",
        json!({ "script": script, "review_file": "pass.review.txt", "paragraph_id": 1, "options": ["Laura grins."] }),
    );
    std::fs::write(scripts.join("run.sh"), "#!/bin/sh\necho hello\n").unwrap();
    std::fs::write(s.dir.join("outside.review.txt"), "SCRIPT REVIEW v1\n").unwrap();
    let session_path = scripts.join("film.scriptorium.json");
    let original: Value = serde_json::from_str(&std::fs::read_to_string(&session_path).unwrap()).unwrap();
    let script_before = std::fs::read_to_string(&script).unwrap();
    let outside = s.dir.join("outside.review.txt").to_string_lossy().into_owned();
    for (target, says, remedy) in [
        ("run.sh", "doesn't start with SCRIPT REVIEW", None),
        ("film.fdx", "a script or one of its own files", None),
        (outside.as_str(), "is outside the script's folder", Some("copy it into that folder")),
    ] {
        let mut planted = original.clone();
        planted["reviews"] = json!([{ "path": target, "id": "zz", "text_hash": "0" }]);
        std::fs::write(&session_path, planted.to_string()).unwrap();
        // The only attached review is the default.
        let (text, err) = s.call(
            "suggest_change",
            json!({ "script": script, "paragraph_id": 1, "options": ["$(echo INJECTED >&2)"] }),
        );
        assert!(err && text.contains(says), "{target}: {text}");
        match remedy {
            Some(r) => assert!(text.contains(r), "{target}: {text}"),
            None => assert!(!text.contains("The writer can"), "{target}: {text}"),
        }
        // Named by its path, too.
        let (text, err) = s.call(
            "suggest_change",
            json!({ "script": script, "review_file": target, "paragraph_id": 1, "options": ["$(echo INJECTED >&2)"] }),
        );
        assert!(err, "{target}: {text}");
        // Detaching only edits the session: it works wherever the file is.
        let out = s.ok("detach_reviews", json!({ "script": script, "files": [target] }));
        assert!(out.contains("removed"), "{out}");
    }
    assert_eq!(std::fs::read_to_string(scripts.join("run.sh")).unwrap(), "#!/bin/sh\necho hello\n");
    assert_eq!(std::fs::read_to_string(&script).unwrap(), script_before);
    assert_eq!(std::fs::read_to_string(s.dir.join("outside.review.txt")).unwrap(), "SCRIPT REVIEW v1\n");
    // An attached review in the folder that is a link out: never written.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(s.dir.join("outside.review.txt"), scripts.join("link.review.txt")).unwrap();
        let mut planted = original.clone();
        planted["reviews"] = json!([{ "path": "link.review.txt", "id": "zz", "text_hash": "0" }]);
        std::fs::write(&session_path, planted.to_string()).unwrap();
        let (text, err) =
            s.call("suggest_change", json!({ "script": script, "paragraph_id": 1, "options": ["Laura beams."] }));
        assert!(err && text.contains("outside the script's folder"), "{text}");
        assert_eq!(std::fs::read_to_string(s.dir.join("outside.review.txt")).unwrap(), "SCRIPT REVIEW v1\n");
    }
    // decide_suggestion says why a review file can't be used.
    std::fs::write(&session_path, original.to_string()).unwrap();
    std::fs::write(scripts.join("notes.review.txt"), "export X=1\n").unwrap();
    let (text, err) = s.call(
        "decide_suggestion",
        json!({ "script": script, "review": "notes.review.txt", "n": 1, "decision": "accept" }),
    );
    assert!(err && text.contains("isn't a review file yet") && text.contains("ask the writer"), "{text}");
    // A script that isn't a regular .fdx gets no lock beside it.
    std::fs::write(scripts.join(".bashrc"), "x").unwrap();
    let dot = scripts.join(".bashrc").to_string_lossy().into_owned();
    let (text, err) = s.call("suggest_change", json!({ "script": dot, "paragraph_id": 1, "options": ["x"] }));
    assert!(err && text.contains(".fdx"), "{text}");
    assert!(!scripts.join(".bashrc.scriptorium.lock").exists() && !scripts.join("..scriptorium.lock").exists());
    let names: Vec<_> = std::fs::read_dir(&scripts).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert!(
        !names.iter().any(|n| n.to_string_lossy().starts_with(".bashrc.") || n == ".scriptorium.lock"),
        "{names:?}"
    );
}

#[cfg(unix)]
#[test]
fn write_script_follows_a_linked_script_to_its_folder() {
    // The script is a link into another folder: its folder is the target's,
    // and the default output goes there.
    let mut s = Server::start_in("linked", "real", "elsewhere");
    let real = s.dir.join("real");
    std::os::unix::fs::symlink(real.join("film.fdx"), s.dir.join("elsewhere/film.fdx")).unwrap();
    let link = s.dir.join("elsewhere/film.fdx").to_string_lossy().into_owned();
    let out = s.ok("write_script", json!({ "script": link }));
    assert!(real.join("film (scriptorium).fdx").is_file(), "{out}");
    assert!(!s.dir.join("elsewhere/film (scriptorium).fdx").exists());
    // Written again: its own default output is replaced.
    s.ok("write_script", json!({ "script": link }));
}

#[cfg(unix)]
#[test]
fn write_script_never_replaces_a_file_named_after_the_link() {
    // film/linked.fdx is a link to other/real.fdx: the script's own output
    // is other/real (scriptorium).fdx; other/linked (scriptorium).fdx is
    // someone else's and stays untouched.
    let mut s = Server::start_in("linkname", "other", "film");
    let other = s.dir.join("other");
    std::fs::rename(other.join("film.fdx"), other.join("real.fdx")).unwrap();
    std::os::unix::fs::symlink(other.join("real.fdx"), s.dir.join("film/linked.fdx")).unwrap();
    std::fs::write(other.join("linked (scriptorium).fdx"), "keep me").unwrap();
    let link = s.dir.join("film/linked.fdx").to_string_lossy().into_owned();
    s.ok("write_script", json!({ "script": link }));
    assert!(other.join("real (scriptorium).fdx").is_file());
    let (text, err) = s.call("write_script", json!({ "script": link, "output": "linked (scriptorium).fdx" }));
    assert!(err && text.contains("another name"), "{text}");
    assert_eq!(std::fs::read_to_string(other.join("linked (scriptorium).fdx")).unwrap(), "keep me");
}
