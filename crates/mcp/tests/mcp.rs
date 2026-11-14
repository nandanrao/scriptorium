//! The server driven over stdio as an MCP client would: handshake, list tools,
//! then a review round trip on a small script in a temporary folder.
//!
//! The tools run the `scriptorium` binary, which `cargo test` at the workspace
//! root builds next to this one. Tests skip when it's missing (e.g. when only
//! this package was built).

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

fn cli() -> Option<PathBuf> {
    let server = Path::new(env!("CARGO_BIN_EXE_scriptorium-mcp"));
    let cli = server.with_file_name(format!("scriptorium{}", std::env::consts::EXE_SUFFIX));
    cli.is_file().then_some(cli)
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    dir: PathBuf,
}

impl Server {
    fn start(name: &str, cli: &Path) -> Self {
        let dir = std::env::temp_dir().join(format!("scriptorium-mcp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_scriptorium-mcp"))
            .current_dir(&dir)
            .env("SCRIPTORIUM_BIN", cli)
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
    let Some(cli) = cli() else { return eprintln!("skipped: scriptorium binary not built") };
    let mut s = Server::start("handshake", &cli);
    let init = s.request("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }));
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "scriptorium");
    // A notification gets no reply: the next line read must answer the next request.
    s.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    let tools = s.request("tools/list", json!({}));
    let names: Vec<&str> =
        tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for want in ["list_paragraphs", "suggest_change", "decide_suggestion", "undo", "write_script"] {
        assert!(names.contains(&want), "{want} missing from {names:?}");
    }
    assert!(s.request("no/such/method", json!({}))["error"]["code"] == -32601);
}

#[test]
fn propose_decide_undo_write() {
    let Some(cli) = cli() else { return eprintln!("skipped: scriptorium binary not built") };
    let mut s = Server::start("review", &cli);

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
