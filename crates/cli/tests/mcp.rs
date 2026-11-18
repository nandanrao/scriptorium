//! `scriptorium mcp` driven over stdio as an MCP client would: handshake,
//! list tools, then a review round trip on a small script in a temporary
//! folder. With no app (no socket in the test's own runtime folder) every
//! call runs on the files; a fake app (`fsio::link::Server` in this process)
//! checks the routes over the link.

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
    /// Short runtime/home folders (under /tmp) holding the agent socket.
    short: PathBuf,
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
        Self::start_with(name, script_sub, Some(cwd_sub))
    }

    /// The script in `dir/<script_sub>`, the server running in
    /// `dir/<cwd_sub>`, or in `/` (as agents start it) when `None`.
    fn start_with(name: &str, script_sub: &str, cwd_sub: Option<&str>) -> Self {
        let dir = std::env::temp_dir().join(format!("scriptorium-mcp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(script_sub)).unwrap();
        std::fs::write(dir.join(script_sub).join("film.fdx"), SCRIPT).unwrap();
        let cwd = match cwd_sub {
            Some(sub) => {
                std::fs::create_dir_all(dir.join(sub)).unwrap();
                dir.join(sub)
            }
            None => PathBuf::from("/"),
        };
        // Its own runtime and home folders, so no real app is reached; and
        // as little environment as Claude Desktop passes.
        // Short, under /tmp: macOS's TMPDIR is too long for a socket path.
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let short = PathBuf::from(format!(
            "/tmp/scm.{}.{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&short);
        std::fs::create_dir_all(short.join("run")).unwrap();
        std::fs::create_dir_all(short.join("home")).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_scriptorium"))
            .arg("mcp")
            .current_dir(cwd)
            .env_clear()
            .env("HOME", short.join("home"))
            .env("XDG_RUNTIME_DIR", short.join("run"))
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self { child, stdin, stdout, next_id: 1, dir, short }
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
        let _ = std::fs::remove_dir_all(&self.short);
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
fn the_versions_read_tools_are_read_only_and_read_the_pair() {
    let mut s = Server::start("versions");
    let tools = s.request("tools/list", json!({}));
    for name in ["list_versions", "read_rows"] {
        let t = tools["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == name).unwrap();
        assert_eq!(t["annotations"]["readOnlyHint"], true, "{name}");
        assert!(t["inputSchema"]["properties"]["script"].is_object(), "{name}");
        assert!(!t["inputSchema"]["required"].as_array().unwrap().contains(&json!("script")), "{name}");
    }
    let en = s.dir.join("film.fdx").to_string_lossy().into_owned();
    assert_eq!(
        serde_json::from_str::<Value>(&s.ok("list_versions", json!({ "script": en }))).unwrap()["pair"],
        Value::Null
    );
    scriptorium_cli::run(&["version", "new", &en, "--lang", "es", "--from", "en"], &mut vec![]).unwrap();
    let v: Value = serde_json::from_str(&s.ok("list_versions", json!({ "script": en }))).unwrap();
    let es = v["pair"]["version"].as_str().unwrap().to_string();
    assert!(Path::new(&es).is_absolute() && es.ends_with("film.es.fdx"), "{es}");
    let rows: Value = serde_json::from_str(&s.ok("read_rows", json!({ "script": es, "needs": true }))).unwrap();
    let rows = &rows["rows"];
    assert_eq!(rows.as_array().unwrap().len(), 3, "heading, action, dialogue: {rows}");
    assert_eq!(rows[1]["other"][0]["text"], "Laura smiles.");
    assert!(!s.dir.join("film.es.scriptorium.json").exists(), "reading wrote no session");
}

#[test]
fn new_version_then_rows_suggested_on_the_version_and_written_only_with_an_output() {
    let mut s = Server::start("new-version");
    let tools = s.request("tools/list", json!({}));
    let t = tools["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "new_version").unwrap();
    assert_eq!(t["annotations"]["readOnlyHint"], false);
    assert_eq!(t["inputSchema"]["required"], json!(["language", "original_language"]));
    let en = s.dir.join("film.fdx").to_string_lossy().into_owned();
    let args = json!({ "script": en, "language": "es", "original_language": "en" });
    let out = s.ok("new_version", args.clone());
    let es = out.lines().find_map(|l| l.strip_prefix("The version: ")).expect(&out).to_string();
    assert!(Path::new(&es).is_absolute() && es.ends_with("film.es.fdx"), "{out}");
    let again = s.ok("new_version", args);
    assert!(again.contains("was already made") && again.contains(&format!("The version: {es}")), "{again}");

    let rows: Value = serde_json::from_str(&s.ok("read_rows", json!({ "script": es, "needs": true }))).unwrap();
    let row = &rows["rows"][1];
    let batch = json!({
        "script": es,
        "review_file": "Translation from English.review.txt",
        "suggestions": [{ "row": row["row"], "paragraph_id": row["this"][0]["id"], "options": ["Laura sonríe."] }],
    });
    let added: Value = serde_json::from_str(&s.ok("suggest_changes", batch)).unwrap();
    assert_eq!(added["added"], 1, "{added}");
    let reviews = s.ok("list_reviews", json!({ "script": es }));
    assert!(reviews.contains("Translation from English.review.txt"), "{reviews}");

    let (text, err) = s.call("write_script", json!({ "script": es }));
    assert!(err && text.contains("Write .fdx") && text.contains("give `output`"), "{text}");
    s.ok("write_script", json!({ "script": es, "output": "copy.fdx" }));
    assert!(s.dir.join("copy.fdx").is_file());
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
    // `~` is the server's home folder.
    assert!(!s.dir.join("home/scriptorium-mcp-test-never.review.txt").exists());

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

// ---- the link: a fake app in this process ------------------------------------

use scriptorium_fsio::link::proto::{Done, Hello, Reply, Route};

/// What the fake app does with one request.
enum Act {
    /// `taken` (naming the script), then `done` with this text.
    Done(&'static str),
    /// `route: files`.
    Files,
    /// `taken` (naming the script), then hang up.
    TakenThenHangUp,
    /// `taken`, then a `done` for another request: a protocol break.
    WrongId,
    /// `taken`, then `route: files`: a protocol break.
    TakenThenRoute,
}

impl Server {
    fn script_path(&self) -> String {
        self.dir.join("film.fdx").to_string_lossy().into_owned()
    }

    /// A fake app on this server's socket: for each of `conns`, it accepts
    /// one connection, answers its hello with that hello, and its requests
    /// with the acts in turn, then hangs up. After the last it closes the
    /// socket. The thread returns the calls it got (name and arguments).
    fn fake_app(&self, conns: Vec<(Hello, Vec<Act>)>) -> std::thread::JoinHandle<Vec<(String, Value)>> {
        // Where the connector looks: from HOME on macOS (it has no
        // XDG_RUNTIME_DIR), from XDG_RUNTIME_DIR elsewhere.
        let socket = if cfg!(target_os = "macos") {
            scriptorium_fsio::link::proto::socket_path(&self.short.join("home"))
        } else {
            self.short.join("run/scriptorium/agent.sock")
        };
        let server = scriptorium_fsio::link::Server::bind_at(&socket).unwrap();
        let script = self.script_path();
        std::thread::spawn(move || {
            let mut got = vec![];
            for (hello, acts) in conns {
                let mut conn = server.accept().unwrap();
                if conn.handshake(&hello).unwrap().is_none() {
                    continue;
                }
                for act in acts {
                    let req = conn.next_request().unwrap().unwrap();
                    got.push((req.call.name.clone(), req.call.arguments.clone()));
                    let taken = Reply::Taken { script: Some(script.clone()) };
                    match act {
                        Act::Done(text) => {
                            conn.reply(req.id, &taken).unwrap();
                            conn.reply(req.id, &Reply::Done(Done { text: text.into(), is_error: false })).unwrap();
                        }
                        Act::Files => conn.reply(req.id, &Reply::Route(Route::Files)).unwrap(),
                        Act::TakenThenHangUp => {
                            conn.reply(req.id, &taken).unwrap();
                            break;
                        }
                        Act::WrongId => {
                            conn.reply(req.id, &taken).unwrap();
                            let done = Reply::Done(Done { text: "x".into(), is_error: false });
                            conn.reply(req.id + 100, &done).unwrap();
                            break;
                        }
                        Act::TakenThenRoute => {
                            conn.reply(req.id, &taken).unwrap();
                            conn.reply(req.id, &Reply::Route(Route::Files)).unwrap();
                            break;
                        }
                    }
                }
            }
            got
        })
    }
}

#[test]
fn calls_go_to_the_app_when_it_answers() {
    let mut s = Server::start("routed");
    let app = s.fake_app(vec![(Hello::this_build(), vec![Act::Done("a"), Act::Done("b"), Act::Done("c")])]);
    // No script: the app uses its open one.
    assert_eq!(s.ok("read_scene", json!({ "scene": 1 })), "a");
    // A relative script reaches the app absolute, from the connector's folder.
    assert_eq!(s.ok("read_scene", json!({ "script": "film.fdx", "scene": 1 })), "b");
    // guide goes to the app while it is up.
    assert_eq!(s.ok("guide", json!({ "topic": "review" })), "c");
    let got = app.join().unwrap();
    assert_eq!(got[0], ("read_scene".to_string(), json!({ "scene": 1 })));
    // Compared resolved: macOS's temp folder is behind a link (/var -> /private/var).
    let sent = std::fs::canonicalize(got[1].1["script"].as_str().unwrap()).unwrap();
    assert_eq!(sent, std::fs::canonicalize(s.script_path()).unwrap());
    assert_eq!(got[2].0, "guide");
    // The app is gone: where_is_writer answers plainly, and a call with no
    // script says how to go on.
    let text = s.ok("where_is_writer", json!({}));
    assert!(text.contains("Scriptorium isn't open"), "{text}");
    let (text, err) = s.call("read_scene", json!({ "scene": 1 }));
    assert!(err && text.contains("Scriptorium isn't open") && text.contains("`script`"), "{text}");
    // guide still works without the app.
    assert!(s.ok("guide", json!({ "topic": "translate" })).len() > 100);
}

#[test]
fn route_files_runs_the_call_on_the_files() {
    let mut s = Server::start("route-files");
    let app = s.fake_app(vec![(Hello::this_build(), vec![Act::Files, Act::Files])]);
    let script = s.script_path();
    let scene = s.ok("read_scene", json!({ "script": script, "scene": 1 }));
    assert!(scene.contains("FARM HOUSE"), "{scene}");
    // Sent to the files with no script: nothing is open in the app.
    let (text, err) = s.call("read_scene", json!({ "scene": 1 }));
    assert!(err && text.contains("no script open"), "{text}");
    app.join().unwrap();
}

#[test]
fn another_version_of_the_app_is_told_once_and_the_files_used() {
    let mut s = Server::start("mismatch");
    let other = Hello { version: "0.0.1-other".into(), ..Hello::this_build() };
    let app = s.fake_app(vec![(other.clone(), vec![]), (other.clone(), vec![]), (other, vec![])]);
    let script = s.script_path();
    let first = s.ok("read_scene", json!({ "script": script, "scene": 1 }));
    assert!(first.contains("FARM HOUSE"), "{first}");
    assert!(first.contains("Scriptorium 0.0.1-other is open but this connection is from"), "{first}");
    let second = s.ok("read_scene", json!({ "script": script, "scene": 1 }));
    assert!(second.contains("FARM HOUSE") && !second.contains("0.0.1-other"), "{second}");
    // Something is open: never "isn't open".
    let text = s.ok("where_is_writer", json!({}));
    assert!(text.contains("a different version") && !text.contains("isn't open"), "{text}");
    app.join().unwrap();
}

#[test]
fn the_app_gone_between_calls_means_not_delivered() {
    let mut s = Server::start("gone-between");
    // It hangs up after one call and takes the next connection.
    let app =
        s.fake_app(vec![(Hello::this_build(), vec![Act::Done("a")]), (Hello::this_build(), vec![Act::Done("b")])]);
    assert_eq!(s.ok("read_scene", json!({ "scene": 1 })), "a");
    // Not delivered on the old link: sent again on a new one.
    assert_eq!(
        s.ok("suggest_change", json!({ "review_file": "x.review.txt", "paragraph_id": 3, "options": ["y"] })),
        "b"
    );
    app.join().unwrap();
    // Now it is gone for good: a writing call runs on the files, with no
    // "maybe applied".
    let script = s.script_path();
    let out = s.ok(
        "suggest_change",
        json!({ "script": script, "review_file": "pass.review.txt", "paragraph_id": 3,
                "options": ["I am going to town tomorrow."] }),
    );
    assert!(out.contains("#1") && !out.contains("may have been applied"), "{out}");
    assert!(s.dir.join("pass.review.txt").is_file());
}

#[test]
fn the_app_gone_after_taking_a_read_runs_it_on_the_files() {
    let mut s = Server::start("gone-read");
    let app = s.fake_app(vec![(Hello::this_build(), vec![Act::TakenThenHangUp])]);
    // No script given: the one the app's `taken` named.
    let scene = s.ok("read_scene", json!({ "scene": 1 }));
    assert!(scene.contains("FARM HOUSE"), "{scene}");
    app.join().unwrap();
}

#[test]
fn the_app_gone_after_taking_a_write_never_runs_it_twice() {
    let mut s = Server::start("gone-write");
    let app = s.fake_app(vec![(Hello::this_build(), vec![Act::TakenThenHangUp])]);
    let script = s.script_path();
    let (text, err) = s.call(
        "suggest_change",
        json!({ "script": script, "review_file": "pass.review.txt", "paragraph_id": 3, "options": ["x y"] }),
    );
    assert!(err && text.contains("may have been applied") && text.contains("list_suggestions"), "{text}");
    assert!(!s.dir.join("pass.review.txt").exists());
    app.join().unwrap();
    // The next call uses the files.
    let scene = s.ok("read_scene", json!({ "script": script, "scene": 1 }));
    assert!(scene.contains("FARM HOUSE"), "{scene}");
}

#[test]
fn the_files_route_waits_for_a_held_lock() {
    let mut s = Server::start("lock-wait");
    let script = s.dir.join("film.fdx").canonicalize().unwrap();
    let lock = scriptorium_fsio::SessionLock::acquire(
        &scriptorium_fsio::SessionLock::path_for(&script),
        std::time::Duration::ZERO,
    )
    .unwrap();
    let held = std::time::Duration::from_millis(800);
    let release = std::thread::spawn(move || {
        std::thread::sleep(held);
        drop(lock);
    });
    let start = std::time::Instant::now();
    let out = s.ok(
        "suggest_change",
        json!({ "script": script.to_string_lossy(), "review_file": "pass.review.txt", "paragraph_id": 3,
                "options": ["I am going to town tomorrow."] }),
    );
    assert!(out.contains("#1"), "{out}");
    assert!(start.elapsed() >= held, "it waited for the lock");
    release.join().unwrap();
}

#[test]
fn started_in_the_root_folder_with_no_app() {
    let mut s = Server::start_with("root", "", None);
    let init = s.request("initialize", json!({ "protocolVersion": "2099-01-01" }));
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    assert!(init["result"]["instructions"].as_str().unwrap().contains("where_is_writer"));
    assert!(init["result"]["capabilities"]["prompts"].is_object());
    let script = s.script_path();
    let scene = s.ok("read_scene", json!({ "script": script, "scene": 1 }));
    assert!(scene.contains("FARM HOUSE"), "{scene}");
    // Unknown tools are protocol errors; a script that isn't a .fdx a tool error.
    assert_eq!(s.request("tools/call", json!({ "name": "nope", "arguments": {} }))["error"]["code"], -32602);
    let (text, err) = s.call("read_scene", json!({ "script": "/nowhere/x.fdx", "scene": 1 }));
    assert!(err && text.contains("x.fdx"), "{text}");
}

#[test]
fn prompts_are_listed_and_fetched() {
    let mut s = Server::start("prompts");
    let list = s.request("prompts/list", json!({}));
    let names: Vec<&str> =
        list["result"]["prompts"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["review_pass", "translate"]);
    let got = s.request("prompts/get", json!({ "name": "translate", "arguments": { "language": "Spanish" } }));
    let messages = got["result"]["messages"].as_array().unwrap();
    assert!(messages[1]["content"]["text"].as_str().unwrap().contains("Spanish"), "{got}");
    assert_eq!(s.request("prompts/get", json!({ "name": "nope" }))["error"]["code"], -32602);
}

#[test]
fn a_protocol_break_on_a_write_is_maybe_applied() {
    let mut s = Server::start("garbled");
    let ok = || Hello::this_build();
    let app = s.fake_app(vec![(ok(), vec![Act::WrongId]), (ok(), vec![Act::TakenThenRoute])]);
    let script = s.script_path();
    for _ in 0..2 {
        let (text, err) = s.call(
            "suggest_change",
            json!({ "script": script, "review_file": "pass.review.txt", "paragraph_id": 3, "options": ["x y"] }),
        );
        assert!(err && text.contains("may have been applied"), "{text}");
    }
    assert!(!s.dir.join("pass.review.txt").exists());
    app.join().unwrap();
}

#[test]
fn a_stale_link_then_another_version_tells_once() {
    let mut s = Server::start("stale-mismatch");
    let other = Hello { version: "0.0.1-other".into(), ..Hello::this_build() };
    let app = s.fake_app(vec![(Hello::this_build(), vec![Act::Done("a")]), (other.clone(), vec![]), (other, vec![])]);
    assert_eq!(s.ok("read_scene", json!({ "scene": 1 })), "a");
    let script = s.script_path();
    // The old link is gone; the new one is another version.
    let first = s.ok("read_scene", json!({ "script": script, "scene": 1 }));
    assert!(first.contains("FARM HOUSE") && first.contains("0.0.1-other is open"), "{first}");
    let (text, err) = s.call("read_scene", json!({ "scene": 1 }));
    assert!(err && text.contains("a different version") && !text.contains("0.0.1-other is open"), "{text}");
    app.join().unwrap();
}

#[test]
fn a_line_that_isnt_utf8_is_a_parse_error_and_the_server_goes_on() {
    let mut s = Server::start("utf8");
    s.stdin.write_all(b"{\"id\": 1, \"method\": \"ping\xff\"}\n").unwrap();
    s.stdin.flush().unwrap();
    let mut line = String::new();
    s.stdout.read_line(&mut line).unwrap();
    let reply: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(reply["error"]["code"], -32700, "{reply}");
    s.next_id = 2;
    assert_eq!(s.request("ping", json!({}))["result"], json!({}));
}

#[test]
fn mcp_takes_no_arguments() {
    let out = Command::new(env!("CARGO_BIN_EXE_scriptorium")).args(["mcp", "--x"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("`scriptorium mcp` takes no arguments"));
    assert!(out.stdout.is_empty());
}
