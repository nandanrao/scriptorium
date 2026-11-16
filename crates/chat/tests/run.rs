//! Whole runs of `chat::host::run` on a small script in a temporary folder,
//! against replayed cassettes: the thread's own review created, re-used and
//! re-attached; locks; cancelling; disclosures; whole-script refusals; and a
//! cassette with exchanges left over failing the run. No network.

use std::path::{Path, PathBuf};
use std::time::Duration;

use scriptorium_agent::file::ThreadFile;
use scriptorium_agent::turn::Outcome;
use scriptorium_agent::{Event, ToolCall};
use scriptorium_chat::clock;
use scriptorium_chat::exec::{self, BUSY, CallDone, Direct, FileScript, Gate, ReviewError, WHOLE_SCRIPT_REFUSED};
use scriptorium_chat::host::{
    self, BUSY_WAITING, Env, FINISHED_KEY, Finished, HostError, Request, TOOLS_CHANGED, Tools, Ui, Which,
};
use scriptorium_chat::seen::Seen;
use scriptorium_chat::store::{StoreError, ThreadStore};
use scriptorium_providers::shell::{CancelToken, Ended, Flow, Keys, Replay, TimeScale, Transport};
use scriptorium_providers::{Account, AccountsFile, Price, Provider};
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
    created: Vec<String>,
    cancel_on_text: Option<CancelToken>,
    cancel_on_tool: Option<CancelToken>,
    /// Stop while the whole-script question is shown.
    cancel_on_ask: Option<CancelToken>,
    /// While the disclosure is shown, whether thread `.1` of script `.0` was
    /// free to lock.
    probe: Option<(PathBuf, String)>,
    free_while_disclosing: Vec<bool>,
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
    fn thread_created(&mut self, id: &str) {
        self.created.push(id.to_string());
    }
    fn disclose(&mut self, _text: &str) -> bool {
        self.disclosed += 1;
        if let Some((script, id)) = &self.probe {
            let free = ThreadStore::for_script(script).lock(id, Duration::ZERO).is_ok();
            self.free_while_disclosing.push(free);
        }
        self.accept
    }
    fn confirm_whole_script(&mut self, question: &str) -> bool {
        self.asked.push(question.to_string());
        if let Some(c) = &self.cancel_on_ask {
            c.cancel();
            return false;
        }
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

/// What a test run uses besides the request.
struct Opts<'a> {
    /// The clock's time scale: "0" waits nothing, "1" waits for real.
    scale: &'a str,
    /// The folder `seen.json` lives in.
    config: &'a str,
}

const OPTS: Opts = Opts { scale: "0", config: "config" };

/// Runs `req` over `transport` and `tools`.
fn run_on(
    f: &Folder,
    req: &Request,
    transport: &dyn Transport,
    tools: &mut dyn Tools,
    ui: &mut dyn Ui,
    cancel: &CancelToken,
    opts: &Opts,
) -> Result<Finished, HostError> {
    let accounts = accounts();
    let keys = Keys::in_memory(|_| Some("sk-test".into()));
    let mut seen = Seen::load(&f.0.join(opts.config).join("seen.json"));
    let mut clocks = clock::from_vars(Some("2026-11-15T09:30:00Z"), Some(opts.scale)).unwrap();
    let env = Env { accounts: &accounts, keys: &keys, seen: &mut seen, clocks: &mut clocks };
    host::run(transport, tools, req, env, ui, cancel)
}

fn replay(cassette: &str) -> Replay {
    Replay::from_text("test", cassette, TimeScale::INSTANT).unwrap()
}

fn file_tools(f: &Folder) -> Direct<FileScript> {
    Direct(FileScript::new(&f.script()).unwrap())
}

/// Runs `req` against the cassette `cassette` (JSON Lines text).
fn run_with(
    f: &Folder,
    req: &Request,
    cassette: &str,
    ui: &mut TestUi,
    cancel: &CancelToken,
) -> Result<Finished, HostError> {
    run_on(f, req, &replay(cassette), &mut file_tools(f), ui, cancel, &OPTS)
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
    assert_eq!(ui.created, ["t0001"]);
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
    // Tried for the whole wait (instant here), told once, then the model hears BUSY.
    assert_eq!(ui.notices.iter().filter(|n| *n == BUSY_WAITING).count(), 1);
    assert_eq!(ui.finished[0].result.content, BUSY);
    assert!(!ui.finished[0].busy);
    // The planned name was saved; nothing was created under it.
    assert_eq!(done.thread.review_path.as_deref(), Some("Tighten the smile.review.txt"));
    assert_eq!(done.thread.review_id, None);
}

#[test]
fn a_suggestion_waits_out_a_long_session_lock() {
    let f = Folder::new("session-long");
    let lock = scriptorium_fsio::SessionLock::for_script(&f.script()).unwrap();
    // Another program holds the session for a while (in real time), then lets go.
    let holder = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        drop(lock);
    });
    let mut ui = TestUi::new();
    let script = f.script();
    let req = Request::new(&script, Which::New, Some("Tighten the smile"));
    let opts = Opts { scale: "1", ..OPTS };
    let done = run_on(
        &f,
        &req,
        &replay(&cassette("own_review.jsonl")),
        &mut file_tools(&f),
        &mut ui,
        &CancelToken::new(),
        &opts,
    )
    .unwrap();
    holder.join().unwrap();
    assert_eq!(done.outcome, Outcome::Finished);
    assert!(ui.notices.iter().any(|n| n == BUSY_WAITING), "{:?}", ui.notices);
    assert!(f.read("Tighten the smile.review.txt").contains("Laura grins."));
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

/// The CLI's tools, except that `read_script` always asks first.
struct AskFirst(Direct<FileScript>);

impl Tools for AskFirst {
    fn gate(&mut self, _: &ThreadFile, call: &ToolCall, _: bool, _: Option<&Price>) -> Result<Gate, ReviewError> {
        Ok(if call.name == "read_script" { Gate::Ask { tokens: 30_000, cost: None } } else { Gate::Run })
    }
    fn call(&mut self, file: &mut ThreadFile, call: &ToolCall) -> CallDone {
        self.0.call(file, call)
    }
}

#[test]
fn stopping_at_the_whole_script_question_records_no_answer() {
    let f = Folder::new("whole-stop");
    let cassette = tools(json!({}), &[("toolu_1", "read_script", json!({}))]);
    let script = f.script();
    let req = Request::new(&script, Which::New, Some("Read it all"));
    let cancel = CancelToken::new();
    let mut ui = TestUi { cancel_on_ask: Some(cancel.clone()), ..TestUi::new() };
    let done = run_on(&f, &req, &replay(&cassette), &mut AskFirst(file_tools(&f)), &mut ui, &cancel, &OPTS).unwrap();
    assert_eq!(done.outcome, Outcome::Stopped);
    assert_eq!(ui.asked.len(), 1);
    // Cancelled is not "Not now": the next run asks again.
    assert_eq!(done.thread.whole_script, None);
    assert_eq!(exec::whole_script_tokens(&done.thread), None);
}

#[test]
fn a_blank_message_is_refused() {
    let f = Folder::new("blank");
    let mut ui = TestUi::new();
    assert_eq!(run(&f, Which::New, "  ", "", &mut ui).unwrap_err(), HostError::NothingToSend);
}

/// A transport that counts [`Transport::finish`] calls.
struct Counting {
    inner: Replay,
    finished: std::sync::atomic::AtomicUsize,
}

impl Transport for Counting {
    fn send(
        &self,
        req: &scriptorium_agent::Request,
        cancel: &CancelToken,
        line: &mut dyn FnMut(&str) -> Flow,
    ) -> Result<Ended, scriptorium_agent::AgentError> {
        self.inner.send(req, cancel, line)
    }
    fn finish(&self) -> Result<(), scriptorium_agent::AgentError> {
        self.finished.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.finish()
    }
}

#[test]
fn finish_runs_on_every_exit() {
    let f = Folder::new("finish");
    let script = f.script();
    let counting = |c: &str| Counting { inner: replay(c), finished: Default::default() };
    let count = |t: &Counting| t.finished.load(std::sync::atomic::Ordering::SeqCst);

    // Declined: nothing sent, the error is the run's, finish still called.
    let t = counting(&text(json!({}), "never sent"));
    let mut ui = TestUi { accept: false, ..TestUi::new() };
    let req = Request::new(&script, Which::New, Some("Hello"));
    let err = run_on(&f, &req, &t, &mut file_tools(&f), &mut ui, &CancelToken::new(), &OPTS).unwrap_err();
    assert_eq!(err, HostError::Declined);
    assert_eq!(count(&t), 1);

    // No such thread.
    let t = counting("");
    let req = Request::new(&script, Which::Thread("t0042".into()), Some("Hello"));
    assert!(run_on(&f, &req, &t, &mut file_tools(&f), &mut TestUi::new(), &CancelToken::new(), &OPTS).is_err());
    assert_eq!(count(&t), 1);

    // A blank message.
    let t = counting("");
    let req = Request::new(&script, Which::New, Some(" "));
    let err = run_on(&f, &req, &t, &mut file_tools(&f), &mut TestUi::new(), &CancelToken::new(), &OPTS).unwrap_err();
    assert_eq!(err, HostError::NothingToSend);
    assert_eq!(count(&t), 1);

    // Success.
    let t = counting(&text(json!({}), "Hi"));
    let req = Request::new(&script, Which::New, Some("Hello"));
    run_on(&f, &req, &t, &mut file_tools(&f), &mut TestUi::new(), &CancelToken::new(), &OPTS).unwrap();
    assert_eq!(count(&t), 1);
}

/// Tools that run the CLI's, but "crash" (panic) at call `crash_at` (0-based):
/// `after` the call ran, or before it.
struct Crashing {
    inner: Direct<FileScript>,
    calls: usize,
    crash_at: usize,
    after: bool,
    /// Whether the thread on disk had its review's name before each call.
    planned_on_disk: Vec<Option<String>>,
    script: PathBuf,
}

impl Tools for Crashing {
    fn gate(
        &mut self,
        file: &ThreadFile,
        call: &ToolCall,
        allow: bool,
        price: Option<&Price>,
    ) -> Result<Gate, ReviewError> {
        self.inner.gate(file, call, allow, price)
    }
    fn call(&mut self, file: &mut ThreadFile, call: &ToolCall) -> CallDone {
        let on_disk = ThreadStore::for_script(&self.script).read(&file.id).unwrap();
        self.planned_on_disk.push(on_disk.review_path);
        let n = self.calls;
        self.calls += 1;
        if n == self.crash_at && !self.after {
            panic!("crash before call {n}");
        }
        let done = self.inner.call(file, call);
        if n == self.crash_at {
            panic!("crash after call {n}");
        }
        done
    }
}

fn crashing(f: &Folder, crash_at: usize, after: bool) -> Crashing {
    Crashing { inner: file_tools(f), calls: 0, crash_at, after, planned_on_disk: vec![], script: f.script() }
}

/// Runs `req` with `tools`, which panics; returns once it has.
fn run_crashing(f: &Folder, req: &Request, cassette: &str, tools: &mut Crashing) {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_on(f, req, &replay(cassette), tools, &mut TestUi::new(), &CancelToken::new(), &OPTS)
    }));
    assert!(r.is_err(), "it should have crashed");
}

#[test]
fn a_crash_between_creating_the_review_and_saving_the_thread_makes_no_second_file() {
    let f = Folder::new("crash-review");
    let script = f.script();
    let first =
        tools(json!({}), &[("toolu_1", "suggest_change", json!({"paragraph_id": 1, "options": ["Laura grins."]}))]);
    let mut t = crashing(&f, 0, true);
    run_crashing(&f, &Request::new(&script, Which::New, Some("Tighten the smile")), &first, &mut t);
    // The name was saved in the thread before the file was created.
    assert_eq!(t.planned_on_disk, [Some("Tighten the smile.review.txt".to_string())]);
    let stored = ThreadStore::for_script(&script).read("t0001").unwrap();
    assert_eq!(stored.review_id, None, "the crash came before the thread recorded it");
    assert_eq!(f.reviews(), ["Tighten the smile.review.txt"]);
    // And the session was never told either.
    std::fs::remove_file(f.0.join("film.scriptorium.json")).unwrap();

    let mut ui = TestUi::new();
    let done = run(&f, Which::Thread("t0001".into()), "And Cesar's line", &second_change(), &mut ui).unwrap();
    assert_eq!(done.outcome, Outcome::Finished);
    assert_eq!(f.reviews(), ["Tighten the smile.review.txt"]);
    assert_eq!(done.thread.review_id.as_deref(), Some("thread-t0001"));
    let review = f.read("Tighten the smile.review.txt");
    assert!(review.contains("Laura grins.") && review.contains("going to town"), "{review}");
}

#[test]
fn a_review_file_found_by_its_id_is_reattached() {
    let f = Folder::new("crash-by-id");
    let mut ui = TestUi::new();
    run(&f, Which::New, "Tighten the smile", &cassette("own_review.jsonl"), &mut ui).unwrap();
    // The file was renamed, the session lost, and the thread forgot its path.
    std::fs::rename(f.0.join("Tighten the smile.review.txt"), f.0.join("renamed.review.txt")).unwrap();
    std::fs::remove_file(f.0.join("film.scriptorium.json")).unwrap();
    let threads = ThreadStore::for_script(&f.script());
    let (lock, mut file) = threads.lock_and_read("t0001", Duration::ZERO).unwrap();
    file.review_path = None;
    file.review_id = None;
    threads.write(&lock, &file).unwrap();
    drop(lock);

    let mut ui = TestUi::new();
    let done = run(&f, Which::Thread("t0001".into()), "And Cesar's line", &second_change(), &mut ui).unwrap();
    assert_eq!(f.reviews(), ["renamed.review.txt"]);
    assert_eq!(done.thread.review_path.as_deref(), Some("renamed.review.txt"));
    assert!(f.read("renamed.review.txt").contains("going to town"));
}

#[test]
fn a_crash_mid_round_never_runs_a_finished_call_again() {
    let f = Folder::new("crash-round");
    let script = f.script();
    let round = tools(
        json!({}),
        &[
            ("toolu_1", "suggest_change", json!({"paragraph_id": 1, "options": ["Laura grins."]})),
            ("toolu_2", "suggest_change", json!({"paragraph_id": 3, "options": ["I am going to town tomorrow."]})),
        ],
    );
    let mut t = crashing(&f, 1, false);
    run_crashing(&f, &Request::new(&script, Which::New, Some("Tighten")), &round, &mut t);
    let stored = ThreadStore::for_script(&script).read("t0001").unwrap();
    let saved = stored.extra[FINISHED_KEY].as_array().unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0]["id"], "toolu_1");

    // Sent again: the first call keeps its result, the second is cancelled,
    // and nothing runs twice.
    let again = text(
        json!({"results": [
            {"id": "toolu_1", "contains": "added suggestion #1"},
            {"id": "toolu_2", "contains": "Cancelled"}
        ]}),
        "Picked up.",
    );
    let req = Request::new(&script, Which::Thread("t0001".into()), None);
    let mut ui = TestUi::new();
    let done = run_with(&f, &req, &again, &mut ui, &CancelToken::new()).unwrap();
    assert_eq!(done.outcome, Outcome::Finished);
    assert!(ui.finished.is_empty());
    assert!(!done.thread.extra.contains_key(FINISHED_KEY));
    let review = f.read("Tighten.review.txt");
    assert_eq!(review.matches("Laura grins.").count(), 1, "{review}");
    assert!(!review.contains("going to town"), "{review}");
}

#[test]
fn a_review_changed_on_disk_between_calls_is_read_again() {
    let f = Folder::new("recheck");
    let mut ui = TestUi::new();
    run(&f, Which::New, "Tighten the smile", &cassette("own_review.jsonl"), &mut ui).unwrap();
    // Another program adds a suggestion on Cesar's line to the file.
    let path = f.0.join("Tighten the smile.review.txt");
    let more = format!(
        "{}\n---\nPARA: 3\nBEFORE: I am go to the town tomorrow.\nAFTER: I go to town tomorrow.\n",
        f.read("Tighten the smile.review.txt")
    );
    std::fs::write(&path, more).unwrap();
    let c = [
        tools(
            json!({}),
            &[("toolu_2", "suggest_change", json!({"paragraph_id": 3, "options": ["I am going to town tomorrow."]}))],
        ),
        text(result("toolu_2", "this conversation's review"), "It's there already."),
    ]
    .join("\n");
    let mut ui = TestUi::new();
    run(&f, Which::Thread("t0001".into()), "And Cesar's line", &c, &mut ui).unwrap();
    // Refused against what's on disk; the other program's suggestion is kept.
    assert!(ui.finished[0].result.is_error, "{}", ui.finished[0].result.content);
    let review = f.read("Tighten the smile.review.txt");
    assert!(review.contains("I go to town tomorrow.") && !review.contains("going to town"), "{review}");
}

#[test]
fn a_review_file_argument_is_refused_and_nothing_is_written() {
    let f = Folder::new("named");
    let outside = f.script().parent().unwrap().parent().unwrap().join("planted.review.txt");
    let c = [
        tools(
            json!({}),
            &[
                (
                    "toolu_1",
                    "suggest_change",
                    json!({"paragraph_id": 1, "options": ["Laura grins."], "review_file": "notes.review.txt"}),
                ),
                (
                    "toolu_2",
                    "suggest_change",
                    json!({"paragraph_id": 1, "options": ["Laura grins."], "review_file": outside.to_string_lossy()}),
                ),
            ],
        ),
        text(json!({}), "Done."),
    ]
    .join("\n");
    let mut ui = TestUi::new();
    let done = run(&f, Which::New, "Tighten", &c, &mut ui).unwrap();
    for r in &ui.finished {
        assert!(r.result.is_error && r.result.content.contains("review_file"), "{}", r.result.content);
    }
    assert_eq!(ui.finished.len(), 2);
    assert!(f.reviews().is_empty(), "{:?}", f.reviews());
    assert!(!outside.exists());
    assert_eq!(done.thread.review_id, None);
}

#[test]
fn the_tools_changed_notice_is_shown_once() {
    let f = Folder::new("tools-changed");
    let mut ui = TestUi::new();
    run(&f, Which::New, "Hello", &text(json!({}), "Hi"), &mut ui).unwrap();
    assert!(!ui.notices.iter().any(|n| n == TOOLS_CHANGED));
    let threads = ThreadStore::for_script(&f.script());
    let (lock, mut file) = threads.lock_and_read("t0001", Duration::ZERO).unwrap();
    file.tools_hash = "0000000000000000".into();
    threads.write(&lock, &file).unwrap();
    drop(lock);

    let mut ui = TestUi::new();
    let done = run(&f, Which::Thread("t0001".into()), "Again", &text(json!({}), "Hi"), &mut ui).unwrap();
    assert_eq!(ui.notices.iter().filter(|n| *n == TOOLS_CHANGED).count(), 1, "{:?}", ui.notices);
    assert_ne!(done.thread.tools_hash, "0000000000000000");
    let mut ui = TestUi::new();
    run(&f, Which::Thread("t0001".into()), "Once more", &text(json!({}), "Hi"), &mut ui).unwrap();
    assert!(!ui.notices.iter().any(|n| n == TOOLS_CHANGED));
}

#[test]
fn the_disclosure_is_shown_once_and_remembered() {
    let f = Folder::new("seen");
    let mut ui = TestUi::new();
    run(&f, Which::New, "Hello", &text(json!({}), "Hi"), &mut ui).unwrap();
    assert_eq!(ui.disclosed, 1);
    // Its key is in seen.json, read afresh.
    let seen = Seen::load(&f.0.join("config").join("seen.json"));
    let base = accounts().resolve("work", None).unwrap().base_url;
    assert_eq!(host::disclosure(&seen, &base, &f.script()), None);
    assert!(f.read("config/seen.json").contains("api.anthropic.com"), "{}", f.read("config/seen.json"));
    let mut ui = TestUi::new();
    run(&f, Which::Thread("t0001".into()), "Again", &text(json!({}), "Hi"), &mut ui).unwrap();
    assert_eq!(ui.disclosed, 0);
}

#[test]
fn the_disclosure_is_asked_without_holding_the_thread() {
    let f = Folder::new("unlocked");
    let mut ui = TestUi::new();
    run(&f, Which::New, "Hello", &text(json!({}), "Hi"), &mut ui).unwrap();
    // Another user (another seen.json) opens the same thread.
    let mut ui = TestUi { probe: Some((f.script(), "t0001".into())), ..TestUi::new() };
    let script = f.script();
    let req = Request::new(&script, Which::Thread("t0001".into()), Some("Again"));
    let opts = Opts { config: "other-user", ..OPTS };
    run_on(&f, &req, &replay(&text(json!({}), "Hi")), &mut file_tools(&f), &mut ui, &CancelToken::new(), &opts)
        .unwrap();
    assert_eq!(ui.free_while_disclosing, [true]);
}

/// The app's shape: `host::run` on a worker thread, the Ui and the tools
/// behind channels, the tool calls run on the "UI thread" (this one), which
/// owns the script.
#[test]
fn run_works_on_a_worker_with_ui_and_tools_behind_channels() {
    use std::sync::mpsc::{Receiver, Sender, channel};

    enum ToUi {
        Notice(String),
        Disclose(String, Sender<bool>),
        Gate(ThreadFile, ToolCall, Sender<Result<Gate, ReviewError>>),
        Call(ThreadFile, ToolCall, Sender<(ThreadFile, CallDone)>),
        Done(Result<Finished, HostError>),
    }
    struct ChannelUi(Sender<ToUi>);
    impl Ui for ChannelUi {
        fn notice(&mut self, text: &str) {
            let _ = self.0.send(ToUi::Notice(text.into()));
        }
        fn disclose(&mut self, text: &str) -> bool {
            let (tx, rx) = channel();
            self.0.send(ToUi::Disclose(text.into(), tx)).unwrap();
            rx.recv().unwrap()
        }
        fn confirm_whole_script(&mut self, q: &str) -> bool {
            panic!("no read_script here: {q}")
        }
    }
    struct ChannelTools(Sender<ToUi>);
    impl Tools for ChannelTools {
        fn gate(
            &mut self,
            file: &ThreadFile,
            call: &ToolCall,
            _allow: bool,
            _price: Option<&Price>,
        ) -> Result<Gate, ReviewError> {
            let (tx, rx) = channel();
            self.0.send(ToUi::Gate(file.clone(), call.clone(), tx)).unwrap();
            rx.recv().unwrap()
        }
        fn call(&mut self, file: &mut ThreadFile, call: &ToolCall) -> CallDone {
            let (tx, rx) = channel();
            self.0.send(ToUi::Call(file.clone(), call.clone(), tx)).unwrap();
            let (updated, done) = rx.recv().unwrap();
            *file = updated;
            done
        }
    }

    let f = Folder::new("worker");
    let (tx, rx): (Sender<ToUi>, Receiver<ToUi>) = channel();
    let script = f.script();
    let dir = f.0.clone();
    let worker = std::thread::spawn(move || {
        let f = Folder(dir);
        let mut req = Request::new(&script, Which::New, Some("Tighten the smile"));
        req.wait = Duration::ZERO;
        let r = run_on(
            &f,
            &req,
            &replay(&cassette("own_review.jsonl")),
            &mut ChannelTools(tx.clone()),
            &mut ChannelUi(tx.clone()),
            &CancelToken::new(),
            &OPTS,
        );
        tx.send(ToUi::Done(r)).unwrap();
        std::mem::forget(f); // the test thread's Folder removes it
    });
    // The UI thread: owns the script, answers the questions, runs the calls.
    let mut script = FileScript::new(&f.script()).unwrap();
    let mut disclosed = 0;
    let done = loop {
        match rx.recv().unwrap() {
            ToUi::Notice(n) => assert!(!n.is_empty()),
            ToUi::Disclose(text, answer) => {
                assert!(text.contains("film.fdx"), "{text}");
                disclosed += 1;
                answer.send(true).unwrap();
            }
            ToUi::Gate(file, call, answer) => {
                answer.send(exec::whole_script_gate(&mut script, &file, &call.name, false, None)).unwrap();
            }
            ToUi::Call(mut file, call, answer) => {
                let done = exec::run_call(&mut script, &mut file, &call);
                answer.send((file, done)).unwrap();
            }
            ToUi::Done(r) => break r.unwrap(),
        }
    };
    worker.join().unwrap();
    assert_eq!(disclosed, 1);
    assert_eq!(done.outcome, Outcome::Finished);
    assert_eq!(done.thread.review_id.as_deref(), Some("thread-t0001"));
    assert!(f.read("Tighten the smile.review.txt").contains("Laura grins."));
}

#[test]
fn set_whole_script_overrides_a_stored_no() {
    let f = Folder::new("override");
    let mut ui = TestUi::new();
    run(&f, Which::New, "Hello", &text(json!({}), "Hi"), &mut ui).unwrap();
    let threads = ThreadStore::for_script(&f.script());
    let (lock, mut file) = threads.lock_and_read("t0001", Duration::ZERO).unwrap();
    exec::record_whole_script(&mut file, Some(false), Some(10));
    threads.write(&lock, &file).unwrap();
    drop(lock);
    let file = host::set_whole_script(&threads, "t0001", Some(true), Duration::ZERO).unwrap();
    assert_eq!(file.whole_script, Some(true));
    assert_eq!(exec::whole_script_tokens(&file), Some(10));
    assert_eq!(threads.read("t0001").unwrap().whole_script, Some(true));
}

#[test]
fn disclosure_names_the_provider() {
    assert_eq!(host::provider_label("api.anthropic.com"), "Anthropic (api.anthropic.com)");
    assert_eq!(host::provider_label("openrouter.ai"), "OpenRouter (openrouter.ai)");
    assert_eq!(host::provider_label("localhost:8080"), "localhost:8080");
}
