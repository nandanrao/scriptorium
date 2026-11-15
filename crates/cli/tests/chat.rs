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
        let dir = std::env::temp_dir().join(format!("scriptorium-cli-chat-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config/scriptorium")).unwrap();
        std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
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
    let run = f.run(&["threads", "film.fdx", "delete", "t0001"], None).expect_err();
    assert!(run.stderr().contains("running in another program"), "{}", run.all());
    let run = f.run(&["threads", "film.fdx", "set-model", "t0001", "work", "claude-haiku-4-5"], None).expect_err();
    assert!(run.stderr().contains("running in another program"), "{}", run.all());
    // Reading isn't refused.
    f.run(&["threads", "film.fdx", "show", "t0001"], None).expect_ok();
    drop(held);

    let run = f.run(&["threads", "film.fdx", "set-model", "t0001", "work", "claude-haiku-4-5", "--json"], None);
    assert_eq!(run.expect_ok().json()["model"], "claude-haiku-4-5");
    let run = f.run(&["threads", "film.fdx", "delete", "t0001"], None).expect_ok();
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
fn keys_come_from_stdin_and_never_show() {
    let f = Folder::new("keys");
    // A key on the command line isn't an option.
    f.run(&["accounts", "key", "set", "work", "sk-secret"], None).expect_err();
    // With no keyring, setting one says which variable to use instead.
    let run = f.run_with_stdin(&["accounts", "key", "set", "work"], "sk-secret\n").expect_err();
    assert!(run.stderr().contains("SCRIPTORIUM_WORK_KEY"), "{}", run.all());
    assert!(!run.all().contains("sk-secret"));
    let run = f.run_with_stdin(&["accounts", "key", "set", "work"], "\n").expect_err();
    assert!(run.stderr().contains("no key on stdin"), "{}", run.all());
    let status = f.run(&["accounts", "key", "status", "work", "--json"], None).expect_ok().json();
    assert_eq!(status["key"]["status"], "env");
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
