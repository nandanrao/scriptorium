//! Whole runs of `chat::host::run` on a small script in a temporary folder,
//! against replayed cassettes: the thread's own review created, re-used and
//! re-attached; locks; cancelling; disclosures; whole-script refusals; and a
//! cassette with exchanges left over failing the run. No network.

use std::path::{Path, PathBuf};
use std::time::Duration;

use scriptorium_agent::turn::Outcome;
use scriptorium_agent::{Event, ToolCall};
use scriptorium_chat::clock;
use scriptorium_chat::exec::{CallDone, FileScript, WHOLE_SCRIPT_REFUSED};
use scriptorium_chat::host::{self, Env, Finished, HostError, Request, Ui, Which};
use scriptorium_chat::seen::Seen;
use scriptorium_chat::store::{StoreError, ThreadStore};
use scriptorium_providers::shell::{CancelToken, Keys, Replay, TimeScale};
use scriptorium_providers::{Account, AccountsFile, Provider};
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

/// A fresh folder holding `film.fdx`; removed on drop.
struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("scriptorium-chat-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
        Folder(dir.canonicalize().unwrap())
    }
    fn script(&self) -> PathBuf {
        self.0.join("film.fdx")
    }
    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.0.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
    }
    fn reviews(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.0)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.ends_with(".review.txt"))
            .collect();
        names.sort();
        names
    }
    fn session(&self) -> Value {
        serde_json::from_str(&self.read("film.scriptorium.json")).unwrap()
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An Anthropic exchange: `expect`, then the streamed `lines`.
fn exchange(expect: Value, lines: Vec<Value>) -> String {
    serde_json::to_string(&json!({ "expect": expect, "lines": lines })).unwrap()
}

fn frame(event: &str, data: Value) -> [Value; 3] {
    [json!(format!("event: {event}")), data, json!("")]
}

fn wrap(blocks: Vec<Value>, stop: &str) -> Vec<Value> {
    let mut lines = vec![];
    lines.extend(frame(
        "message_start",
        json!({"type": "message_start", "message": {"id": "m", "usage": {"input_tokens": 10, "output_tokens": 1}}}),
    ));
    lines.extend(blocks);
    lines.extend(frame(
        "message_delta",
        json!({"type": "message_delta", "delta": {"stop_reason": stop}, "usage": {"output_tokens": 5}}),
    ));
    lines.extend(frame("message_stop", json!({"type": "message_stop"})));
    lines
}

/// A reply asking for tool calls `(id, name, input)`.
fn tools(expect: Value, calls: &[(&str, &str, Value)]) -> String {
    let mut blocks = vec![];
    for (i, (id, name, input)) in calls.iter().enumerate() {
        blocks.extend(frame(
            "content_block_start",
            json!({"type": "content_block_start", "index": i,
                   "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}}),
        ));
        blocks.extend(frame(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": i,
                   "delta": {"type": "input_json_delta", "partial_json": input.to_string()}}),
        ));
        blocks.extend(frame("content_block_stop", json!({"type": "content_block_stop", "index": i})));
    }
    exchange(expect, wrap(blocks, "tool_use"))
}

/// A reply of text.
fn text(expect: Value, text: &str) -> String {
    let mut blocks = vec![];
    blocks.extend(frame(
        "content_block_start",
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
    ));
    blocks.extend(frame(
        "content_block_delta",
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
    ));
    blocks.extend(frame("content_block_stop", json!({"type": "content_block_stop", "index": 0})));
    exchange(expect, wrap(blocks, "end_turn"))
}

fn result(id: &str, contains: &str) -> Value {
    json!({"results": [{"id": id, "contains": contains}]})
}

#[derive(Default)]
struct TestUi {
    accept: bool,
    allow_whole: bool,
    disclosed: usize,
    texts: Vec<String>,
    notices: Vec<String>,
    asked: Vec<String>,
    finished: Vec<CallDone>,
    cancel_on_text: Option<CancelToken>,
    cancel_on_tool: Option<CancelToken>,
}

impl TestUi {
    fn new() -> Self {
        TestUi { accept: true, ..Default::default() }
    }
}

impl Ui for TestUi {
    fn event(&mut self, event: &Event) {
        if let Event::Text { text, .. } = event {
            self.texts.push(text.clone());
            if let Some(c) = &self.cancel_on_text {
                c.cancel();
            }
        }
    }
    fn tool_started(&mut self, _call: &ToolCall) {
        if let Some(c) = &self.cancel_on_tool {
            c.cancel();
        }
    }
    fn tool_finished(&mut self, _call: &ToolCall, done: &CallDone) {
        self.finished.push(done.clone());
    }
    fn notice(&mut self, text: &str) {
        self.notices.push(text.to_string());
    }
    fn disclose(&mut self, _text: &str) -> bool {
        self.disclosed += 1;
        self.accept
    }
    fn confirm_whole_script(&mut self, question: &str) -> bool {
        self.asked.push(question.to_string());
        self.allow_whole
    }
}

fn accounts() -> AccountsFile {
    let mut account = Account::new("work", "Work", Provider::Anthropic);
    account.model = Some("claude-opus-5-5".into());
    let mut file = AccountsFile::default();
    file.upsert(account).unwrap();
    file
}

/// Runs `req` against the cassette `cassette` (JSON Lines text).
fn run_with(
    f: &Folder,
    req: &Request,
    cassette: &str,
    ui: &mut TestUi,
    cancel: &CancelToken,
) -> Result<Finished, HostError> {
    let replay = Replay::from_text("test", cassette, TimeScale::INSTANT).unwrap();
    let accounts = accounts();
    let keys = Keys::in_memory(|_| Some("sk-test".into()));
    let mut seen = Seen::load(&f.0.join("config").join("seen.json"));
    let mut clocks = clock::from_vars(Some("2026-11-15T09:30:00Z"), Some("0")).unwrap();
    let mut script = FileScript::new(&f.script()).unwrap().with_lock_wait(Duration::ZERO);
    let env = Env { accounts: &accounts, keys: &keys, seen: &mut seen, clocks: &mut clocks };
    host::run(&replay, &mut script, req, env, ui, cancel)
}

fn run(f: &Folder, which: Which, message: &str, cassette: &str, ui: &mut TestUi) -> Result<Finished, HostError> {
    let script = f.script();
    let req = Request::new(&script, which, Some(message));
    run_with(f, &req, cassette, ui, &CancelToken::new())
}

fn cassette(name: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes").join(name)).unwrap()
}

#[test]
fn a_tool_round_creates_the_threads_review() {
    let f = Folder::new("own");
    let mut ui = TestUi::new();
    let done = run(&f, Which::New, "Tighten the smile", &cassette("own_review.jsonl"), &mut ui).unwrap();
    assert_eq!(done.outcome, Outcome::Finished);
    assert!(done.created);
    assert_eq!(done.thread.id, "t0001");
    assert_eq!(done.thread.title, "Tighten the smile");
    assert_eq!(done.thread.review_id.as_deref(), Some("thread-t0001"));
    assert_eq!(done.thread.review_path.as_deref(), Some("Tighten the smile.review.txt"));
    assert_eq!(ui.disclosed, 1);
    assert_eq!(ui.texts.concat(), "One suggestion is waiting in your review.");
    let added = ui.finished[0].added.clone().unwrap();
    assert_eq!(added.review_id, "thread-t0001");
    assert_eq!(added.numbers, [Some(1)]);

    let review = f.read("Tighten the smile.review.txt");
    assert!(review.contains("Id: thread-t0001"), "{review}");
    assert!(review.contains("Laura grins."), "{review}");
    assert_eq!(f.reviews(), ["Tighten the smile.review.txt"]);
    let session = f.session();
    assert_eq!(session["reviews"][0]["path"], "Tighten the smile.review.txt", "{session}");

    // The thread file on disk is the one returned.
    let stored = ThreadStore::for_script(&f.script()).read("t0001").unwrap();
    assert_eq!(stored, done.thread);
    assert_eq!(stored.updated, "2026-11-15T09:30:00Z");
}

/// One more change on the dialogue line, in thread t0001.
fn second_change() -> String {
    [
        tools(
            json!({"last_user": "And Cesar"}),
            &[("toolu_2", "suggest_change", json!({"paragraph_id": 3, "options": ["I am going to town tomorrow."]}))],
        ),
        text(result("toolu_2", "added suggestion #2"), "Done."),
    ]
    .join("\n")
}

#[test]
fn a_second_run_adds_to_the_same_review() {
    let f = Folder::new("rerun");
    let mut ui = TestUi::new();
    run(&f, Which::New, "Tighten the smile", &cassette("own_review.jsonl"), &mut ui).unwrap();
    // The disclosure isn't shown again; the list defaults to the thread's review.
    let mut ui = TestUi::new();
    let c = [
        tools(json!({"last_user": "And Cesar"}), &[("toolu_l", "list_suggestions", json!({}))]),
        tools(
            result("toolu_l", "Laura grins."),
            &[("toolu_2", "suggest_change", json!({"paragraph_id": 3, "options": ["I am going to town tomorrow."]}))],
        ),
        text(result("toolu_2", "added suggestion #2"), "Done."),
    ]
    .join("\n");
    let done = run(&f, Which::Thread("t0001".into()), "And Cesar's line", &c, &mut ui).unwrap();
    assert_eq!(done.outcome, Outcome::Finished);
    assert!(!done.created);
    assert_eq!(ui.disclosed, 0);
    assert_eq!(f.reviews(), ["Tighten the smile.review.txt"]);
    let review = f.read("Tighten the smile.review.txt");
    assert!(review.contains("Laura grins.") && review.contains("going to town"), "{review}");
    assert_eq!(done.thread.thread.messages.iter().filter(|m| m.text().contains("Cesar")).count(), 1);
}

#[test]
fn a_detached_review_is_reattached_not_duplicated() {
    let f = Folder::new("detach");
    let mut ui = TestUi::new();
    run(&f, Which::New, "Tighten the smile", &cassette("own_review.jsonl"), &mut ui).unwrap();
    // Detach it: no session lists it any more.
    std::fs::remove_file(f.0.join("film.scriptorium.json")).unwrap();
    let mut ui = TestUi::new();
    let done = run(&f, Which::Thread("t0001".into()), "And Cesar's line", &second_change(), &mut ui).unwrap();
    assert_eq!(done.outcome, Outcome::Finished);
    assert_eq!(f.reviews(), ["Tighten the smile.review.txt"]);
    let review = f.read("Tighten the smile.review.txt");
    assert!(review.contains("Laura grins.") && review.contains("going to town"), "{review}");
    assert_eq!(f.session()["reviews"][0]["path"], "Tighten the smile.review.txt");
}

#[test]
fn a_review_file_deleted_is_made_again_and_names_dont_clash() {
    let f = Folder::new("names");
    // Another file already has the title's name: the thread's review is "… 2".
    std::fs::write(f.0.join("Tighten the smile.review.txt"), "not ours").unwrap();
    let mut ui = TestUi::new();
    let done = run(&f, Which::New, "Tighten the smile", &cassette("own_review.jsonl"), &mut ui).unwrap();
    assert_eq!(done.thread.review_path.as_deref(), Some("Tighten the smile 2.review.txt"));
    assert_eq!(f.read("Tighten the smile.review.txt"), "not ours");
    assert!(f.read("Tighten the smile 2.review.txt").contains("Id: thread-t0001"));
}

#[test]
fn a_thread_running_elsewhere_is_refused() {
    let f = Folder::new("busy");
    let mut ui = TestUi::new();
    run(&f, Which::New, "Tighten the smile", &cassette("own_review.jsonl"), &mut ui).unwrap();
    let threads = ThreadStore::for_script(&f.script());
    let held = threads.lock("t0001", Duration::ZERO).unwrap();
    let mut ui = TestUi::new();
    let script = f.script();
    let mut req = Request::new(&script, Which::Thread("t0001".into()), Some("again"));
    req.wait = Duration::from_millis(50);
    let err = run_with(&f, &req, "", &mut ui, &CancelToken::new()).unwrap_err();
    assert_eq!(err, HostError::Store(StoreError::Busy));
    drop(held);
    // Nothing was added to the thread.
    assert!(!threads.read("t0001").unwrap().thread.messages.iter().any(|m| m.text() == "again"));
}

#[test]
fn a_busy_session_is_a_tool_result_not_a_crash() {
    let f = Folder::new("session-busy");
    let lock = scriptorium_fsio::SessionLock::for_script(&f.script()).unwrap();
    let c = [
        tools(json!({}), &[("toolu_1", "suggest_change", json!({"paragraph_id": 1, "options": ["Laura grins."]}))]),
        text(result("toolu_1", "busy in another program"), "I'll wait."),
    ]
    .join("\n");
    let mut ui = TestUi::new();
    let done = run(&f, Which::New, "Tighten the smile", &c, &mut ui).unwrap();
    drop(lock);
    assert_eq!(done.outcome, Outcome::Finished);
    assert!(f.reviews().is_empty());
    assert_eq!(done.thread.review_path, None);
}

#[test]
fn cancelling_mid_stream_keeps_what_arrived() {
    let f = Folder::new("cancel-stream");
    let cancel = CancelToken::new();
    let mut ui = TestUi { cancel_on_text: Some(cancel.clone()), ..TestUi::new() };
    let script = f.script();
    let req = Request::new(&script, Which::New, Some("Hello"));
    let done = run_with(&f, &req, &text(json!({}), "Hi there"), &mut ui, &cancel).unwrap();
    assert_eq!(done.outcome, Outcome::Stopped);
    let stored = ThreadStore::for_script(&f.script()).read(&done.thread.id).unwrap();
    assert_eq!(stored, done.thread);
}

#[test]
fn cancelling_during_tools_answers_the_rest_as_cancelled() {
    let f = Folder::new("cancel-tools");
    let cancel = CancelToken::new();
    let mut ui = TestUi { cancel_on_tool: Some(cancel.clone()), ..TestUi::new() };
    let c = tools(
        json!({}),
        &[
            ("toolu_1", "suggest_change", json!({"paragraph_id": 1, "options": ["Laura grins."]})),
            ("toolu_2", "suggest_change", json!({"paragraph_id": 3, "options": ["I am going to town tomorrow."]})),
        ],
    );
    let script = f.script();
    let req = Request::new(&script, Which::New, Some("Tighten"));
    let done = run_with(&f, &req, &c, &mut ui, &cancel).unwrap();
    assert_eq!(done.outcome, Outcome::Stopped);
    // The first call ran and its suggestion stays; the second never ran.
    assert_eq!(ui.finished.len(), 1);
    let review = f.read("Tighten.review.txt");
    assert!(review.contains("Laura grins.") && !review.contains("going to town"), "{review}");
    let json = done.thread.to_json();
    assert!(json.contains(scriptorium_agent::turn::CANCELLED), "{json}");
}

#[test]
fn exchanges_left_over_fail_the_run() {
    let f = Folder::new("leftover");
    let mut ui = TestUi::new();
    let c = [text(json!({}), "Hi"), text(json!({}), "never asked for")].join("\n");
    let err = run(&f, Which::New, "Hello", &c, &mut ui).unwrap_err();
    let HostError::Transport(e) = err else { panic!("{err:?}") };
    assert!(e.to_string().contains("never requested"), "{e}");
    // The thread was saved before the check.
    assert_eq!(ThreadStore::for_script(&f.script()).list().unwrap().len(), 1);
}

#[test]
fn a_declined_disclosure_sends_and_writes_nothing() {
    let f = Folder::new("declined");
    let mut ui = TestUi { accept: false, ..TestUi::new() };
    let err = run(&f, Which::New, "Hello", "", &mut ui).unwrap_err();
    assert_eq!(err, HostError::Declined);
    assert!(!ThreadStore::for_script(&f.script()).dir().exists());
    // Asked again next time.
    let mut ui = TestUi::new();
    run(&f, Which::New, "Hello", &text(json!({}), "Hi"), &mut ui).unwrap();
    assert_eq!(ui.disclosed, 1);
}

#[test]
fn a_stored_not_now_refuses_read_script_unless_allowed() {
    let f = Folder::new("whole");
    let mut ui = TestUi::new();
    let first = run(&f, Which::New, "Hello", &text(json!({}), "Hi"), &mut ui).unwrap();
    // The writer said "Not now" earlier in this thread.
    let threads = ThreadStore::for_script(&f.script());
    let (lock, mut file) = threads.lock_and_read(&first.thread.id, Duration::ZERO).unwrap();
    file.whole_script = Some(false);
    threads.write(&lock, &file).unwrap();
    drop(lock);

    let refused = [
        tools(json!({}), &[("toolu_1", "read_script", json!({}))]),
        text(result("toolu_1", "chose not to send the whole script"), "OK."),
    ]
    .join("\n");
    let mut ui = TestUi::new();
    run(&f, Which::Thread(first.thread.id.clone()), "Read it all", &refused, &mut ui).unwrap();
    assert_eq!(ui.finished[0].result.content, WHOLE_SCRIPT_REFUSED);
    assert!(ui.asked.is_empty());

    // --allow-whole-script overrides it for the run, and stores nothing.
    let allowed =
        [tools(json!({}), &[("toolu_2", "read_script", json!({}))]), text(result("toolu_2", "Laura smiles."), "Read.")]
            .join("\n");
    let script = f.script();
    let mut req = Request::new(&script, Which::Thread(first.thread.id.clone()), Some("Read it all"));
    req.allow_whole_script = true;
    let mut ui = TestUi::new();
    let done = run_with(&f, &req, &allowed, &mut ui, &CancelToken::new()).unwrap();
    assert_eq!(done.thread.whole_script, Some(false));
}

#[test]
fn a_blank_message_is_refused() {
    let f = Folder::new("blank");
    let mut ui = TestUi::new();
    assert_eq!(run(&f, Which::New, "  ", "", &mut ui).unwrap_err(), HostError::NothingToSend);
}
