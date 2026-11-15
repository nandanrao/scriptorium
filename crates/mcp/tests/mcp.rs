//! The server driven over stdio as an MCP client would: handshake, list tools,
//! then a review round trip on a small script in a temporary folder.
//!
//! The server runs the CLI library in-process: no `scriptorium` binary needed.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
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
    assert!(err && text.contains("paragraph_id or before"), "{text}");
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
    assert!(err && text.contains("doesn't exist") && text.contains("review_file"), "{text}");
    assert!(!s.dir.join("nowhere").exists());

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
