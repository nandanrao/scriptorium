//! `scriptorium chat`, `threads` and `accounts` as a program would run them:
//! the binary on a small script in a temporary folder, replies replayed from
//! cassettes (`SCRIPTORIUM_REPLAY`), a deterministic clock and thread ids
//! (`SCRIPTORIUM_TEST_CLOCK`), the config folder in the temporary folder, and
//! the OS keyring never touched (`SCRIPTORIUM_KEYRING=off`). No network.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use scriptorium_chat::store::ThreadStore;
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

const ACCOUNTS: &str = r#"{"version": 1, "default": "work",
  "accounts": [{"id": "work", "name": "Work", "provider": "anthropic", "model": "claude-opus-5-5"}]}"#;

/// A fresh folder holding `film.fdx` and a config folder with one account;
/// removed on drop.
struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        Folder::with_script(name, SCRIPT)
    }

    fn with_script(name: &str, script: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("scriptorium-cli-chat-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config/scriptorium")).unwrap();
        std::fs::write(dir.join("film.fdx"), script).unwrap();
        std::fs::write(dir.join("config/scriptorium/accounts.json"), ACCOUNTS).unwrap();
        Folder(dir.canonicalize().unwrap())
    }

    fn path(&self, p: &str) -> PathBuf {
        self.0.join(p)
    }

    fn read(&self, p: &str) -> String {
        std::fs::read_to_string(self.path(p)).unwrap_or_else(|e| panic!("{p}: {e}"))
    }

    /// Writes `exchanges` as a cassette and returns its path.
    fn cassette(&self, name: &str, exchanges: &[String]) -> PathBuf {
        let path = self.path(name);
        std::fs::write(&path, exchanges.join("\n") + "\n").unwrap();
        path
    }

    fn command(&self, args: &[&str], replay: Option<&Path>) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_scriptorium"));
        c.current_dir(&self.0)
            .args(args)
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("HOME", &self.0)
            .env("SCRIPTORIUM_KEYRING", "off")
            .env("SCRIPTORIUM_TEST_CLOCK", "2026-11-15T09:30:00Z")
            .env("SCRIPTORIUM_TEST_TIME_SCALE", "0")
            .env("SCRIPTORIUM_ANTHROPIC_KEY", "sk-test")
            .env_remove("SCRIPTORIUM_WORK_KEY")
            .env_remove("SCRIPTORIUM_REPLAY")
            .stdin(Stdio::null());
        if let Some(r) = replay {
            c.env("SCRIPTORIUM_REPLAY", r);
        }
        c
    }

    fn run(&self, args: &[&str], replay: Option<&Path>) -> Run {
        Run(self.command(args, replay).output().unwrap())
    }

    /// Runs with `stdin` piped in.
    fn run_with_stdin(&self, args: &[&str], stdin: &str) -> Run {
        use std::io::Write;
        let mut child = self
            .command(args, None)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        Run(child.wait_with_output().unwrap())
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Run(Output);

impl Run {
    fn code(&self) -> Option<i32> {
        self.0.status.code()
    }
    fn ok(&self) -> bool {
        self.0.status.success()
    }
    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.0.stdout).into_owned()
    }
    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.0.stderr).into_owned()
    }
    fn all(&self) -> String {
        self.stdout() + &self.stderr()
    }
    #[track_caller]
    fn expect_ok(self) -> Self {
        assert!(self.ok(), "failed:\n{}", self.all());
        self
    }
    #[track_caller]
    fn expect_err(self) -> Self {
        assert!(!self.ok(), "expected a failure:\n{}", self.all());
        self
    }
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout()).unwrap_or_else(|e| panic!("{e}: {}", self.all()))
    }
}

fn frame(event: &str, data: Value) -> [Value; 3] {
    [json!(format!("event: {event}")), data, json!("")]
}

/// An Anthropic exchange replying `text`.
fn text_reply(expect: Value, text: &str) -> String {
    let mut lines = vec![];
    lines.extend(frame(
        "message_start",
        json!({"type": "message_start", "message": {"id": "m", "usage": {"input_tokens": 10, "output_tokens": 1}}}),
    ));
    lines.extend(frame(
        "content_block_start",
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
    ));
    lines.extend(frame(
        "content_block_delta",
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
    ));
    lines.extend(frame("content_block_stop", json!({"type": "content_block_stop", "index": 0})));
    lines.extend(frame(
        "message_delta",
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 5}}),
    ));
    lines.extend(frame("message_stop", json!({"type": "message_stop"})));
    serde_json::to_string(&json!({"expect": expect, "lines": lines})).unwrap()
}

/// An exchange the provider refuses with 400 (not retried).
fn bad_request() -> String {
    json!({"status": 400, "body": {"type": "error", "error": {"type": "invalid_request_error", "message": "something is off"}}})
        .to_string()
}

fn shared_cassette(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes").join(name)
}

/// Runs the first round of `own_review.jsonl`: thread t0001, its review created.
fn first_round(f: &Folder) -> Run {
    let cassette = shared_cassette("own_review.jsonl");
    f.run(&["chat", "film.fdx", "--yes", "Tighten the smile"], Some(&cassette)).expect_ok()
}

#[test]
fn a_chat_round_creates_the_threads_review() {
    let f = Folder::new("round");
    let run = first_round(&f);
    assert_eq!(run.stdout(), "One suggestion is waiting in your review.\n");
    let err = run.stderr();
    assert!(err.contains("note: replaying") && err.contains("SCRIPTORIUM_REPLAY"), "{err}");
    assert!(err.contains("What gets sent."), "the disclosure is shown: {err}");
    assert!(err.contains("▸ Proposed a change to ¶1"), "the tool row: {err}");
    assert!(err.contains("thread t0001 (Tighten the smile) ·"), "{err}");
    assert!(err.contains("suggestions are in Tighten the smile.review.txt"), "{err}");

    let review = f.read("Tighten the smile.review.txt");
    assert!(review.contains("Id: thread-t0001") && review.contains("Laura grins."), "{review}");
    let session: Value = serde_json::from_str(&f.read("film.scriptorium.json")).unwrap();
    assert_eq!(session["reviews"][0]["path"], "Tighten the smile.review.txt");

    // The existing review commands see it.
    let s = f.run(&["suggestions", "film.fdx", "--json"], None).expect_ok().json();
    assert_eq!(s.as_array().unwrap().len(), 1, "{s}");

    let show = f.run(&["threads", "film.fdx", "show", "t0001"], None).expect_ok().stdout();
    assert!(show.contains("review    Tighten the smile.review.txt (1 suggestion: 1 pending, 0 decided)"), "{show}");
    assert!(show.contains("You: Tighten the smile") && show.contains("Agent: One suggestion"), "{show}");
    let show = f.run(&["threads", "film.fdx", "show", "t0001", "--json"], None).expect_ok().json();
    assert_eq!(show["review_path"], "Tighten the smile.review.txt");
    assert_eq!(show["suggestions"]["pending"], 1);
    assert_eq!(show["suggestions"]["decided"], 0);
    assert_eq!(show["messages"][1]["tool_calls"][0]["name"], "suggest_change");

    // Deciding it shows in the counts.
    f.run(&["decide", "film.fdx", "1", "accept"], None).expect_ok();
    let show = f.run(&["threads", "film.fdx", "show", "t0001", "--json"], None).expect_ok().json();
    assert_eq!((show["suggestions"]["pending"].clone(), show["suggestions"]["decided"].clone()), (json!(0), json!(1)));

    let list = f.run(&["threads", "film.fdx", "--json"], None).expect_ok().json();
    assert_eq!(list[0]["id"], "t0001");
    assert_eq!(list[0]["model"], "claude-opus-5-5");
    let list = f.run(&["threads", "film.fdx"], None).expect_ok().stdout();
    assert!(list.contains("t0001  Tighten the smile") && list.contains("1 suggestion"), "{list}");
}

#[test]
fn the_disclosure_needs_yes_without_a_terminal() {
    let f = Folder::new("disclose");
    let cassette = f.cassette("c.jsonl", &[text_reply(json!({}), "Hi.")]);
    let run = f.run(&["chat", "film.fdx", "Hello"], Some(&cassette)).expect_err();
    assert!(run.stderr().contains("run again with --yes"), "{}", run.all());
    assert!(!f.path("film.threads").exists(), "nothing written");
    // Once accepted, it isn't asked again for this script and host.
    f.run(&["chat", "film.fdx", "--yes", "Hello"], Some(&cassette)).expect_ok();
    let again = f.cassette("d.jsonl", &[text_reply(json!({"last_user": "Again"}), "Yes.")]);
    let run = f.run(&["chat", "film.fdx", "--thread", "t0001", "Again"], Some(&again)).expect_ok();
    assert_eq!(run.stdout(), "Yes.\n");
}

#[test]
fn a_failed_run_is_retried() {
    let f = Folder::new("retry");
    let fails = f.cassette("fails.jsonl", &[bad_request()]);
    let run = f.run(&["chat", "film.fdx", "--yes", "Tighten the smile"], Some(&fails)).expect_err();
    let err = run.stderr();
    assert!(err.contains("something is off") && err.contains("--thread t0001 --retry"), "{err}");

    let works = f.cassette("works.jsonl", &[text_reply(json!({"last_user": "Tighten the smile"}), "Done now.")]);
    let run = f.run(&["chat", "film.fdx", "--thread", "t0001", "--retry", "--json"], Some(&works)).expect_ok();
    let out = run.json();
    assert_eq!(out["thread"], "t0001");
    assert_eq!(out["created"], false);
    assert_eq!(out["text"], "Done now.");
    // The message was sent once: the thread has one user message.
    let show = f.run(&["threads", "film.fdx", "show", "t0001", "--json"], None).expect_ok().json();
    let users = show["messages"].as_array().unwrap().iter().filter(|m| m["role"] == "user").count();
    assert_eq!(users, 1, "{show}");

    // --retry takes no message, and needs a thread.
    f.run(&["chat", "film.fdx", "--retry"], Some(&works)).expect_err();
    f.run(&["chat", "film.fdx", "--thread", "t0001", "--retry", "more"], Some(&works)).expect_err();
}

#[test]
fn a_thread_running_elsewhere_is_refused() {
    let f = Folder::new("busy");
    first_round(&f);
    let held = ThreadStore::for_script(&f.path("film.fdx")).lock("t0001", Duration::ZERO).unwrap();
    let cassette = f.cassette("c.jsonl", &[text_reply(json!({}), "Hi.")]);
    let start = Instant::now();
    let run = f.run(&["chat", "film.fdx", "--thread", "t0001", "More"], Some(&cassette)).expect_err();
    assert!(start.elapsed() >= Duration::from_secs(2), "waited CLI_WAIT first");
    assert!(run.stderr().contains("running in another program"), "{}", run.all());
    assert_eq!(run.code(), Some(3), "busy");
    let run = f.run(&["threads", "film.fdx", "remove", "t0001"], None).expect_err();
    assert!(run.stderr().contains("running in another program"), "{}", run.all());
    assert_eq!(run.code(), Some(3));
    // As JSON: the busy outcome, and the same command to try again.
    let run = f.run(&["chat", "film.fdx", "--thread", "t0001", "--json", "More"], Some(&cassette)).expect_err();
    let out = run.json();
    assert_eq!((out["outcome"].clone(), out["thread"].clone()), (json!("busy"), json!("t0001")));
    assert_eq!(out["error"]["kind"], "busy");
    assert_eq!(out["error"]["retryable"], true);
    assert_eq!(out["retry"], "scriptorium chat film.fdx --thread t0001 --json More");
    let run = f.run(&["threads", "film.fdx", "set-model", "t0001", "work", "claude-haiku-4-5"], None).expect_err();
    assert!(run.stderr().contains("running in another program"), "{}", run.all());
    // Reading isn't refused.
    f.run(&["threads", "film.fdx", "show", "t0001"], None).expect_ok();
    drop(held);

    let run = f.run(&["threads", "film.fdx", "set-model", "t0001", "work", "claude-haiku-4-5", "--json"], None);
    assert_eq!(run.expect_ok().json()["model"], "claude-haiku-4-5");
    let run = f.run(&["threads", "film.fdx", "remove", "t0001"], None).expect_ok();
    assert!(run.stdout().contains("its review Tighten the smile.review.txt stays"), "{}", run.all());
    assert!(f.path("Tighten the smile.review.txt").exists());
    assert!(f.path("film.threads/t0001.lock").exists(), "lock files stay");
    assert_eq!(f.run(&["threads", "film.fdx", "--json"], None).expect_ok().json(), json!([]));
}

#[test]
fn leftover_exchanges_fail_the_run() {
    let f = Folder::new("leftover");
    let cassette = f.cassette("c.jsonl", &[text_reply(json!({}), "Hi."), text_reply(json!({}), "Never sent.")]);
    let run = f.run(&["chat", "film.fdx", "--yes", "Hello"], Some(&cassette)).expect_err();
    assert!(run.stderr().contains("1 of 2 exchange(s) never requested"), "{}", run.all());
    // The thread was saved first.
    let show = f.run(&["threads", "film.fdx", "show", "t0001", "--json"], None).expect_ok().json();
    assert_eq!(show["messages"][1]["text"], "Hi.");
    // As JSON: the new thread's id is known though the run failed.
    let f = Folder::new("leftover-json");
    let cassette = f.cassette("c.jsonl", &[text_reply(json!({}), "Hi."), text_reply(json!({}), "Never sent.")]);
    let out = f.run(&["chat", "film.fdx", "--yes", "--json", "Hello"], Some(&cassette)).expect_err().json();
    assert_eq!(out["outcome"], "error");
    assert_eq!(out["error"]["kind"], "transport");
    assert_eq!(out["thread"], "t0001");
    assert_eq!(out["text"], "Hi.");
}

#[test]
fn record_and_replay_together_are_refused() {
    let f = Folder::new("record");
    let cassette = f.cassette("c.jsonl", &[text_reply(json!({}), "Hi.")]);
    let run = f.run(&["chat", "film.fdx", "--yes", "--record", "out.jsonl", "Hello"], Some(&cassette)).expect_err();
    assert!(run.stderr().contains("can't record while replaying"), "{}", run.all());
}

#[test]
fn accounts_add_list_remove() {
    let f = Folder::new("accounts");
    std::fs::remove_file(f.path("config/scriptorium/accounts.json")).unwrap();
    let list = f.run(&["accounts", "list"], None).expect_ok().stdout();
    assert!(list.starts_with("no accounts yet"), "{list}");

    f.run(&["accounts", "add", "--provider", "anthropic"], None).expect_ok();
    let run = f.run(
        &[
            "accounts",
            "add",
            "local",
            "--provider",
            "custom",
            "--base-url",
            "http://localhost:11434/v1",
            "--protocol",
            "openai_chat",
            "--model",
            "llama3",
            "--default",
        ],
        None,
    );
    run.expect_ok();
    // Same id twice is refused.
    f.run(&["accounts", "add", "local", "--provider", "openai"], None).expect_err();
    f.run(&["accounts", "add", "--provider", "nope"], None).expect_err();

    let list = f.run(&["accounts", "--json"], None).expect_ok().json();
    let ids: Vec<&str> = list.as_array().unwrap().iter().map(|a| a["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["anthropic", "local"]);
    assert_eq!(list[0]["key"], json!({"status": "env", "var": "SCRIPTORIUM_ANTHROPIC_KEY"}));
    assert_eq!(list[1]["default"], true);
    assert_eq!(list[1]["key"]["status"], "not_needed");
    let text = f.run(&["accounts", "list"], None).expect_ok().stdout();
    assert!(text.contains("* local") && text.contains("key: from SCRIPTORIUM_ANTHROPIC_KEY"), "{text}");
    assert!(!text.contains("sk-test"), "keys are never printed: {text}");

    // Plain http to another machine works, with the providers' warning.
    let run = f.run(
        &[
            "accounts",
            "add",
            "lan",
            "--provider",
            "custom",
            "--base-url",
            "http://example.com/v1",
            "--protocol",
            "anthropic",
        ],
        None,
    );
    assert!(run.expect_ok().stderr().contains("is plain http to another machine"));
    f.run(&["accounts", "remove", "lan"], None).expect_ok();

    f.run(&["accounts", "default", "anthropic"], None).expect_ok();
    f.run(&["accounts", "remove", "local"], None).expect_ok();
    f.run(&["accounts", "remove", "local"], None).expect_err();
    let file: Value = serde_json::from_str(&f.read("config/scriptorium/accounts.json")).unwrap();
    assert_eq!(file["default"], "anthropic");
    assert_eq!(file["accounts"].as_array().unwrap().len(), 1);
}

#[test]
fn keys_come_from_stdin_and_with_keyring_off_nothing_is_pretended() {
    let f = Folder::new("keys");
    // A key on the command line isn't an option.
    let run = f.run(&["accounts", "key", "set", "work", "sk-secret"], None).expect_err();
    assert_eq!(run.code(), Some(2));
    // SCRIPTORIUM_KEYRING=off: setting or deleting a key is refused, naming
    // the variable to use instead, before stdin is read.
    let run = f.run_with_stdin(&["accounts", "key", "set", "work"], "sk-secret\n").expect_err();
    assert!(
        run.stderr().contains("SCRIPTORIUM_KEYRING=off") && run.stderr().contains("SCRIPTORIUM_WORK_KEY"),
        "{}",
        run.all()
    );
    assert!(!run.all().contains("sk-secret"));
    let run = f.run(&["accounts", "key", "delete", "work"], None).expect_err();
    assert!(run.stderr().contains("nothing was stored or deleted"), "{}", run.all());
    let run = f.run_with_stdin(&["accounts", "add", "two", "--provider", "openai", "--key-stdin"], "sk-secret\n");
    run.expect_err();
    assert!(!f.read("config/scriptorium/accounts.json").contains("two"), "nothing added");
    let status = f.run(&["accounts", "key", "status", "work", "--json"], None).expect_ok().json();
    assert_eq!(status["key"]["status"], "env");
    let run = f.run(&["accounts", "remove", "work"], None).expect_ok();
    assert!(run.stdout().contains("keyring not touched (SCRIPTORIUM_KEYRING=off)"), "{}", run.all());
    assert!(!f.read("config/scriptorium/accounts.json").contains("sk-"));
}

#[test]
fn accounts_check_lists_models() {
    let f = Folder::new("check");
    let body = r#"{"data": [{"id": "claude-opus-5-5"}, {"id": "claude-haiku-4-5"}]}"#;
    let ok = f.cassette("ok.jsonl", &[json!({"expect": {"path": "/models"}, "lines": [body]}).to_string()]);
    let run = f.run(&["accounts", "check", "work", "--json"], Some(&ok)).expect_ok();
    assert_eq!(run.json()["models"], json!(["claude-haiku-4-5", "claude-opus-5-5"]));
    let refused = json!({"status": 401, "body": {"type": "error", "error": {"type": "authentication_error", "message": "invalid x-api-key"}}});
    let bad = f.cassette("bad.jsonl", &[refused.to_string()]);
    let run = f.run(&["accounts", "check", "work"], Some(&bad)).expect_err();
    assert!(run.stderr().contains("work:"), "{}", run.all());
}

/// An Anthropic exchange asking for tool calls `(id, name, input)`.
fn tool_reply(expect: Value, calls: &[(&str, &str, Value)]) -> String {
    let mut lines = vec![];
    lines.extend(frame(
        "message_start",
        json!({"type": "message_start", "message": {"id": "m", "usage": {"input_tokens": 10, "output_tokens": 1}}}),
    ));
    for (i, (id, name, input)) in calls.iter().enumerate() {
        lines.extend(frame(
            "content_block_start",
            json!({"type": "content_block_start", "index": i,
                   "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}}),
        ));
        lines.extend(frame(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": i,
                   "delta": {"type": "input_json_delta", "partial_json": input.to_string()}}),
        ));
        lines.extend(frame("content_block_stop", json!({"type": "content_block_stop", "index": i})));
    }
    lines.extend(frame(
        "message_delta",
        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 5}}),
    ));
    lines.extend(frame("message_stop", json!({"type": "message_stop"})));
    serde_json::to_string(&json!({"expect": expect, "lines": lines})).unwrap()
}

/// A reply that streams `words` words slowly (`delay_ms` before each line).
fn slow_reply(words: usize, delay_ms: u64) -> String {
    let mut lines = vec![];
    lines.extend(frame(
        "message_start",
        json!({"type": "message_start", "message": {"id": "m", "usage": {"input_tokens": 10, "output_tokens": 1}}}),
    ));
    lines.extend(frame(
        "content_block_start",
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
    ));
    for _ in 0..words {
        lines.extend(frame(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "word "}}),
        ));
    }
    serde_json::to_string(&json!({"delay_ms": delay_ms, "lines": lines})).unwrap()
}

/// Starts `chat` on `cassette` in real time, sends it SIGINT after `after`,
/// and waits for it.
#[cfg(unix)]
fn interrupted(f: &Folder, args: &[&str], cassette: &Path, after: Duration) -> Run {
    let child = f
        .command(args, Some(cassette))
        // Real time, so the reply is still on its way when Ctrl+C comes.
        .env("SCRIPTORIUM_TEST_TIME_SCALE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(after);
    let killed = Command::new("kill").args(["-INT", &child.id().to_string()]).status().unwrap();
    assert!(killed.success());
    Run(child.wait_with_output().unwrap())
}

#[cfg(unix)]
#[test]
fn ctrl_c_stops_and_retry_carries_on() {
    let f = Folder::new("stop");
    // Stopped before any of the reply arrived: --retry sends it again.
    let late = f.cassette("late.jsonl", &[slow_reply(3, 4000)]);
    let run =
        interrupted(&f, &["chat", "film.fdx", "--yes", "--json", "Hello there"], &late, Duration::from_millis(1000));
    let run = run.expect_err();
    assert_eq!(run.code(), Some(130), "{}", run.all());
    let out = run.json();
    assert_eq!(out["outcome"], "stopped");
    assert_eq!(out["thread"], "t0001");
    assert_eq!(out["error"]["kind"], "stopped");
    assert_eq!(out["retry"], "scriptorium chat film.fdx --thread t0001 --retry --json");
    let works = f.cassette("works.jsonl", &[text_reply(json!({"last_user": "Hello there"}), "All done.")]);
    let out = f.run(&["chat", "film.fdx", "--thread", "t0001", "--retry", "--json"], Some(&works)).expect_ok().json();
    assert_eq!(out["outcome"], "finished");
    assert_eq!((out["error"].clone(), out["retry"].clone()), (Value::Null, Value::Null));
    assert_eq!(out["text"], "All done.");

    // Stopped mid-reply: the partial reply is kept, and there is nothing to
    // retry; a new message carries on.
    let slow = f.cassette("slow.jsonl", &[slow_reply(100, 50)]);
    let args = ["chat", "film.fdx", "--thread", "t0001", "--json", "Tell me more"];
    let run = interrupted(&f, &args, &slow, Duration::from_millis(1500)).expect_err();
    assert_eq!(run.code(), Some(130), "{}", run.all());
    let out = run.json();
    assert_eq!(out["outcome"], "stopped");
    assert!(out["text"].as_str().unwrap().starts_with("word"), "{out}");
    assert_eq!(out["retry"], Value::Null);
    assert_eq!(out["error"]["retryable"], false);
    let next = f.cassette("next.jsonl", &[text_reply(json!({"last_user": "Go on"}), "Going on.")]);
    f.run(&["chat", "film.fdx", "--thread", "t0001", "Go on"], Some(&next)).expect_ok();
}

#[test]
fn the_round_limit_ends_the_run() {
    let f = Folder::new("rounds");
    let rounds: Vec<String> =
        (0..26).map(|i| tool_reply(json!({}), &[(&format!("toolu_{i}"), "script_info", json!({}))])).collect();
    let cassette = f.cassette("c.jsonl", &rounds);
    let run = f.run(&["chat", "film.fdx", "--yes", "--json", "Look around"], Some(&cassette)).expect_err();
    assert_eq!(run.code(), Some(1), "{}", run.all());
    let out = run.json();
    assert_eq!(out["outcome"], "round_limit", "{out}");
    assert_eq!(out["error"]["kind"], "round_limit");
    assert_eq!(out["retry"], "scriptorium chat film.fdx --thread t0001 --retry --json");
}

#[test]
fn a_long_script_isnt_sent_whole_without_asking() {
    let mut script = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<FinalDraft DocumentType=\"Script\" Version=\"5\">\n<Content>\n",
    );
    script.push_str("<Paragraph Type=\"Scene Heading\"><Text>INT. LONG HALL - DAY</Text></Paragraph>\n");
    for i in 0..3000 {
        script.push_str(&format!(
            "<Paragraph Type=\"Action\"><Text>Line {i}: the corridor keeps going and going, nobody knows where.</Text></Paragraph>\n"
        ));
    }
    script.push_str("</Content>\n</FinalDraft>\n");
    let f = Folder::with_script("whole", &script);
    let refused = [
        tool_reply(json!({}), &[("toolu_1", "read_script", json!({}))]),
        text_reply(
            json!({"results": [{"id": "toolu_1", "contains": "The writer chose not to send the whole script"}]}),
            "I'll read scenes instead.",
        ),
    ];
    let cassette = f.cassette("c.jsonl", &refused);
    // No terminal to ask on: refused, and the model is told.
    let run = f.run(&["chat", "film.fdx", "--yes", "Read it all"], Some(&cassette)).expect_ok();
    assert!(run.stderr().contains("run with --allow-whole-script"), "{}", run.all());
    let show = f.run(&["threads", "film.fdx", "show", "t0001", "--json"], None).expect_ok().json();
    assert_eq!(show["whole_script"], false);

    // --allow-whole-script overrides the stored answer for that run.
    let allowed = [
        tool_reply(json!({}), &[("toolu_2", "read_script", json!({}))]),
        text_reply(json!({"results": [{"id": "toolu_2", "contains": "LONG HALL"}]}), "Read it."),
    ];
    let cassette = f.cassette("d.jsonl", &allowed);
    let args = ["chat", "film.fdx", "--thread", "t0001", "--allow-whole-script", "Now read it"];
    f.run(&args, Some(&cassette)).expect_ok();
}

#[test]
fn a_refusal_takes_the_message_back() {
    let f = Folder::new("refusal");
    let mut lines = vec![];
    lines.extend(frame(
        "message_start",
        json!({"type": "message_start", "message": {"id": "m", "usage": {"input_tokens": 10, "output_tokens": 1}}}),
    ));
    lines.extend(frame(
        "message_delta",
        json!({"type": "message_delta", "delta": {"stop_reason": "refusal"}, "usage": {"output_tokens": 0}}),
    ));
    lines.extend(frame("message_stop", json!({"type": "message_stop"})));
    let cassette = f.cassette("c.jsonl", &[json!({"lines": lines}).to_string()]);
    let run = f.run(&["chat", "film.fdx", "--yes", "--json", "Something odd"], Some(&cassette)).expect_err();
    assert_eq!(run.code(), Some(1), "{}", run.all());
    let out = run.json();
    assert_eq!(out["outcome"], "withdrawn", "{out}");
    assert_eq!(out["error"]["kind"], "refused");
    assert!(out["error"]["message"].as_str().unwrap().contains("Something odd"), "{out}");
    assert_eq!(out["retry"], Value::Null);
}

#[test]
fn each_failure_has_its_json_outcome_and_exit_status() {
    let f = Folder::new("outcomes");
    let hi = f.cassette("hi.jsonl", &[text_reply(json!({}), "Hi.")]);
    // Declined: nobody to ask, no --yes. The retry adds --yes.
    let run = f.run(&["chat", "film.fdx", "--json", "it's me"], Some(&hi)).expect_err();
    assert_eq!(run.code(), Some(4));
    let out = run.json();
    assert_eq!(out["outcome"], "declined");
    assert_eq!(out["thread"], Value::Null);
    assert_eq!(out["retry"], "scriptorium chat film.fdx --json 'it'\\''s me' --yes");

    // A provider error: the thread is saved, and --retry is the way on.
    let fails = f.cassette("fails.jsonl", &[bad_request()]);
    let run = f.run(&["chat", "film.fdx", "--yes", "--json", "Go"], Some(&fails)).expect_err();
    assert_eq!(run.code(), Some(1));
    let out = run.json();
    assert_eq!(out["outcome"], "error");
    assert_eq!(out["thread"], "t0001");
    assert_eq!(out["error"]["kind"], "bad_request");
    assert_eq!(out["error"]["retryable"], false);
    assert_eq!(out["retry"], "scriptorium chat film.fdx --thread t0001 --retry --json");
    assert_eq!(out["usage"]["input_tokens"], 0);
    assert_eq!(out["usage"]["cost_complete"], true);

    // Wrong arguments.
    let run = f.run(&["chat", "film.fdx", "--json"], None).expect_err();
    assert_eq!(run.code(), Some(2));
    assert_eq!(run.json()["error"]["kind"], "usage");
    assert_eq!(f.run(&["threads", "film.fdx", "frobnicate"], None).expect_err().code(), Some(2));

    // Finished, with structured usage.
    let run = f.run(&["chat", "film.fdx", "--json", "Hello"], Some(&hi)).expect_ok();
    let out = run.json();
    assert_eq!(out["outcome"], "finished");
    assert_eq!(out["usage"]["input_tokens"], 10);
    assert_eq!(out["usage"]["output_tokens"], 5);
    assert!(out["usage"]["cost_usd"].as_f64().unwrap() > 0.0, "{out}");
    let list = f.run(&["threads", "film.fdx", "--json"], None).expect_ok().json();
    let row = list.as_array().unwrap().iter().find(|r| r["id"] == out["thread"]).unwrap().clone();
    assert_eq!(row["usage"]["output_tokens"], 5);
    assert_eq!(row["title"], "Hello");
}

#[test]
fn the_message_can_come_from_stdin() {
    let f = Folder::new("stdin");
    let cassette = f.cassette("c.jsonl", &[text_reply(json!({"last_user": "two lines\nof message"}), "Read.")]);
    let mut c = f.command(&["chat", "film.fdx", "--yes", "-"], Some(&cassette));
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(b"two lines\nof message\n").unwrap();
    }
    let run = Run(child.wait_with_output().unwrap()).expect_ok();
    assert_eq!(run.stdout(), "Read.\n");
}

#[test]
fn accounts_set_and_the_providers_default_model() {
    let f = Folder::new("set");
    std::fs::write(
        f.path("config/scriptorium/accounts.json"),
        r#"{"version": 1, "accounts": [{"id": "work", "provider": "anthropic"}]}"#,
    )
    .unwrap();
    // No model in the file: the provider's default is used.
    let list = f.run(&["accounts", "--json"], None).expect_ok().json();
    assert_eq!(list[0]["model"], "claude-opus-5-5");
    let hi = f.cassette("hi.jsonl", &[text_reply(json!({"model": "claude-opus-5-5"}), "Hi.")]);
    f.run(&["chat", "film.fdx", "--yes", "Hello"], Some(&hi)).expect_ok();

    let run = f.run(
        &["accounts", "set", "work", "--name", "Office", "--model", "claude-haiku-4-5", "--effort", "low", "--json"],
        None,
    );
    let out = run.expect_ok().json();
    assert_eq!(
        (out["name"].clone(), out["model"].clone(), out["effort"].clone()),
        (json!("Office"), json!("claude-haiku-4-5"), json!("low"))
    );
    f.run(&["accounts", "set", "work"], None).expect_err();
    let run = f.run(&["accounts", "set", "work", "--base-url", "http://x"], None).expect_err();
    assert_eq!(run.code(), Some(2));
    // A warning only where an account is set up or tested.
    let add = [
        "accounts",
        "add",
        "lan",
        "--provider",
        "custom",
        "--protocol",
        "openai_chat",
        "--base-url",
        "https://example.com/v1",
        "--model",
        "m",
    ];
    f.run(&add, None).expect_ok();
    let run = f.run(&["accounts", "set", "lan", "--base-url", "http://example.com/v1"], None).expect_ok();
    assert!(run.stderr().contains("plain http"), "{}", run.all());
    let run = f.run(&["accounts", "list"], None).expect_ok();
    assert!(!run.stderr().contains("plain http"), "{}", run.all());
}
