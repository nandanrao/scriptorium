//! The app's run driver: one conversation of the panel, and the runs on it.
//!
//! There is one run loop, `chat::host::run`, the CLI's too. The driver runs
//! it on a worker thread with:
//!
//! - a [`host::Tools`] that sends each tool call to the UI thread, where it
//!   runs (`exec::whole_script_gate` / `exec::run_call`) inside *one*
//!   `ScriptView` update over [`LiveScript`]; the worker blocks on the
//!   answer. A busy answer ([`CallDone::busy`]) is retried by `host::run`
//!   off the UI thread. Calls carry the thread file without its messages
//!   ([`slim`]), so a long thread isn't copied per call.
//! - a [`host::Ui`] behind the same channel: events, notices and saved
//!   threads go to the UI without waiting; the two questions (disclosure,
//!   whole-script read) block the worker until the writer answers.
//! - `Request::wait` [`THREAD_WAIT`]: a thread another program is running is
//!   refused after that short wait ("running in another program"); the wait
//!   covers this process's own worker that is still stopping (a script
//!   closed and opened again).
//!
//! The UI side is a GPUI task owned by [`Chat`] that drains the channel once
//! per frame, at most [`FRAME_BUDGET`] of work and [`FRAME_MESSAGES`]
//! messages at a time (so repaints coalesce and a flood can't stall the UI),
//! and makes every entity update from its own async context, one at a time:
//! the script view for tool calls, the chat for everything else, never one
//! nested in the other.
//!
//! Stop cancels the run's [`CancelToken`] and marks the turn stopped at once;
//! events that arrive after it are dropped, a pending question is answered
//! "no" (and the run records no answer: it was cancelled), a tool call not
//! yet run answers "not run". The thread as the run saved it shows when the
//! worker reports back.
//!
//! The disclosure is checked by the UI before a run starts ([`Chat::send`]
//! returns [`SendError::Disclose`]); if the run finds another one to show (the
//! thread moved to another account), it asks through [`Chat::question`].
//! Accepted disclosures are also kept in memory, so a settings folder that
//! can't be written doesn't lock the writer out.
//!
//! Runs go on while the panel is closed (the panel, and so its chat, live as
//! long as the script view); dropping the chat (another script opened, the
//! window closed) stops its run.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use gpui::{App, AsyncApp, Context, EventEmitter, Task, WeakEntity};
use scriptorium_agent::file::ThreadFile;
use scriptorium_agent::turn::{self, Limits, Outcome};
use scriptorium_agent::{AgentError, Event, Request as HttpRequest, Thread, ToolCall, Usage};
use scriptorium_chat::exec::{self, Added, CallDone, Gate, ReviewError};
use scriptorium_chat::host::{self, Env, Finished, HostError, Request, Which};
use scriptorium_chat::seen::{self, Seen};
use scriptorium_chat::store::{StoreError, ThreadStore};
use scriptorium_providers::shell::{CancelToken, Ended, Flow, Keys, Transport, config, transport_from_env};
use scriptorium_providers::{AccountsFile, Price};

use super::live::LiveScript;
pub use super::messages::ReviewRef;
pub use super::view_model::ErrorAction;
use super::view_model::{Stream, error_action};
use crate::script_view::ScriptView;

/// How often the UI drains the worker's channel: about once a frame.
const FRAME: Duration = Duration::from_millis(16);
/// The most work the UI does on a run's messages per frame.
const FRAME_BUDGET: Duration = Duration::from_millis(4);
/// The most messages the UI takes per frame.
const FRAME_MESSAGES: usize = 256;

/// How long a run waits for its thread's lock.
pub const THREAD_WAIT: Duration = Duration::from_millis(500);

/// A tool call's answer when the script view is gone.
const CLOSED: &str = "The script was closed.";

/// Shown when the thread is locked by another program.
pub const BUSY_THREAD: &str = "This thread is running in another program.";

/// What a run asks the writer, blocking until [`Chat::answer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    /// The first-use disclosure (`chat::model::disclosure_text`): send or not.
    Disclose(String),
    /// Before `read_script` sends a long script (`exec::whole_script_question`).
    WholeScript(String),
}

impl Question {
    pub fn text(&self) -> &str {
        match self {
            Question::Disclose(t) | Question::WholeScript(t) => t,
        }
    }
}

/// Why [`Chat::send`] / [`Chat::retry`] didn't start a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// A run is in progress: Send is Stop.
    Running,
    /// Nothing to send (a blank message, or a retry without a thread).
    Blank,
    /// Show this disclosure first; on Send, [`Chat::accept_disclosure`]`(key)`
    /// and send again. The draft stays in the composer.
    Disclose { key: String, text: String },
    /// No accounts file, no account, no key, a broken cassette…
    Setup(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Running => f.write_str("The agent is still working; stop it first."),
            SendError::Blank => f.write_str("Type a message to send."),
            SendError::Disclose { text, .. } => f.write_str(text),
            SendError::Setup(m) => f.write_str(m),
        }
    }
}

/// What the chat tells the panel (and the composer, U6).
#[derive(Debug, Clone, PartialEq)]
pub enum ChatEvent {
    /// The writer's message is in the saved thread: the draft can go.
    MessageSaved,
    /// The model refused and the message was taken out of the thread: put
    /// it back in the composer.
    Withdrawn(String),
    /// The run waits for the writer ([`Chat::question`]).
    Asking,
    /// The run is over. `say`: the line for the status bar when the panel is
    /// closed (an error, or the writer is needed); `None` for a plain finish.
    Ended { say: Option<String> },
}

/// How a run that didn't finish failed, said on the worker.
#[derive(Debug, Clone)]
struct RunFailed {
    text: String,
    action: Option<ErrorAction>,
    /// The writer said no to the disclosure: nothing to show.
    declined: bool,
}

impl RunFailed {
    fn new(text: impl Into<String>) -> Self {
        RunFailed { text: text.into(), action: None, declined: false }
    }

    fn from_host(e: HostError) -> Self {
        match e {
            HostError::Store(StoreError::Busy | StoreError::RunningHere) => RunFailed::new(BUSY_THREAD),
            HostError::Declined => RunFailed { declined: true, ..RunFailed::new("Nothing was sent.") },
            HostError::Providers(e) => {
                RunFailed { action: Some(ErrorAction::ManageAccounts), ..RunFailed::new(capitalized(&e.to_string())) }
            }
            HostError::NoAccount => {
                RunFailed { action: Some(ErrorAction::ManageAccounts), ..RunFailed::new(capitalized(&e.to_string())) }
            }
            HostError::Transport(e) => RunFailed { action: error_action(&e), ..RunFailed::new(e.to_string()) },
            e => RunFailed::new(capitalized(&e.to_string())),
        }
    }
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// The thread file without its messages or system prompt: what a tool call
/// needs (ids, review, whole-script answer), cheap to send whatever the
/// thread's length.
fn slim(file: &ThreadFile) -> ThreadFile {
    ThreadFile {
        version: file.version,
        id: file.id.clone(),
        title: file.title.clone(),
        created: file.created.clone(),
        updated: file.updated.clone(),
        account: file.account.clone(),
        model: file.model.clone(),
        review_id: file.review_id.clone(),
        review_path: file.review_path.clone(),
        whole_script: file.whole_script,
        system: String::new(),
        tools_hash: file.tools_hash.clone(),
        thread: Thread::default(),
        extra: file.extra.clone(),
    }
}

/// From the worker to the UI thread.
enum ToUi {
    Event(Event),
    Waiting(u64),
    Busy(bool),
    ToolFinished(ToolCall, CallDone),
    Notice(String),
    Saved(Arc<ThreadFile>),
    Ask(Question, mpsc::Sender<bool>),
    Gate {
        file: Box<ThreadFile>,
        call: ToolCall,
        allow: bool,
        price: Option<Price>,
        reply: mpsc::Sender<Result<Gate, ReviewError>>,
    },
    Call {
        /// [`slim`].
        file: Box<ThreadFile>,
        call: ToolCall,
        /// The thread's review as the call left it (`None`: the view is
        /// gone), the result, and notices from opening the script.
        reply: mpsc::Sender<(Option<ReviewMark>, CallDone, Vec<String>)>,
    },
    Done(Result<Finished, RunFailed>),
}

/// What a tool call may change in the thread file: its review's id and path.
type ReviewMark = (Option<String>, Option<String>);

/// The worker's [`host::Ui`]: forwards to the UI thread.
struct WorkerUi {
    tx: mpsc::Sender<ToUi>,
    /// Disclosures the writer accepted in this session (by text), in case
    /// they couldn't be saved.
    accepted: Vec<String>,
}

impl WorkerUi {
    fn ask(&self, q: Question) -> bool {
        let (tx, rx) = mpsc::channel();
        self.tx.send(ToUi::Ask(q, tx)).is_ok() && rx.recv().unwrap_or(false)
    }
}

impl host::Ui for WorkerUi {
    fn event(&mut self, event: &Event) {
        let _ = self.tx.send(ToUi::Event(event.clone()));
    }
    fn waiting(&mut self, secs: u64) {
        let _ = self.tx.send(ToUi::Waiting(secs));
    }
    fn busy(&mut self, waiting: bool) {
        let _ = self.tx.send(ToUi::Busy(waiting));
    }
    fn tool_finished(&mut self, call: &ToolCall, done: &CallDone) {
        let _ = self.tx.send(ToUi::ToolFinished(call.clone(), done.clone()));
    }
    fn notice(&mut self, text: &str) {
        let _ = self.tx.send(ToUi::Notice(text.to_string()));
    }
    fn saved(&mut self, file: &ThreadFile) {
        // One copy per step, shared from here on.
        let _ = self.tx.send(ToUi::Saved(Arc::new(file.clone())));
    }
    fn disclose(&mut self, text: &str) -> bool {
        self.accepted.iter().any(|a| a == text) || self.ask(Question::Disclose(text.to_string()))
    }
    fn confirm_whole_script(&mut self, question: &str) -> bool {
        self.ask(Question::WholeScript(question.to_string()))
    }
}

/// The worker's [`host::Tools`]: each call runs on the UI thread.
struct WorkerTools {
    tx: mpsc::Sender<ToUi>,
    notes: Vec<String>,
}

impl host::Tools for WorkerTools {
    fn gate(
        &mut self,
        file: &ThreadFile,
        call: &ToolCall,
        allow: bool,
        price: Option<&Price>,
    ) -> Result<Gate, ReviewError> {
        let (reply, rx) = mpsc::channel();
        let msg = ToUi::Gate { file: Box::new(slim(file)), call: call.clone(), allow, price: price.copied(), reply };
        if self.tx.send(msg).is_err() {
            return Err(ReviewError::Failed(CLOSED.into()));
        }
        rx.recv().unwrap_or_else(|_| Err(ReviewError::Failed(CLOSED.into())))
    }

    fn call(&mut self, file: &mut ThreadFile, call: &ToolCall) -> CallDone {
        let (reply, rx) = mpsc::channel();
        if self.tx.send(ToUi::Call { file: Box::new(slim(file)), call: call.clone(), reply }).is_err() {
            return CallDone::new(call, CLOSED, true);
        }
        match rx.recv() {
            Ok((mark, done, notes)) => {
                if let Some((id, path)) = mark {
                    file.review_id = id;
                    file.review_path = path;
                }
                self.notes.extend(notes);
                done
            }
            Err(_) => CallDone::new(call, CLOSED, true),
        }
    }

    fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }
}

/// The process's transport: one for every run, so a replayed cassette is one
/// queue across runs.
static TRANSPORT: OnceLock<Result<Arc<dyn Transport>, AgentError>> = OnceLock::new();

fn shared_transport() -> Result<Arc<dyn Transport>, AgentError> {
    TRANSPORT.get_or_init(|| transport_from_env(None).map(Arc::from)).clone()
}

/// The end of the process's runs (the app quitting): the transport's own
/// [`Transport::finish`], which for a replayed cassette fails if exchanges
/// were never requested. Nothing to do if no run was started.
pub fn finish_transport() -> Result<(), AgentError> {
    match TRANSPORT.get() {
        Some(Ok(t)) => t.finish(),
        _ => Ok(()),
    }
}

/// The shared transport for one run. `finish` does nothing: `host::run`
/// calls it after every run, and a cassette for several runs has exchanges
/// left until the last. The real one runs when the app quits
/// ([`finish_transport`], registered by `agent_panel::init`).
struct Shared(Arc<dyn Transport>);

impl Transport for Shared {
    fn send(
        &self,
        req: &HttpRequest,
        cancel: &CancelToken,
        line: &mut dyn FnMut(&str) -> Flow,
    ) -> Result<Ended, AgentError> {
        self.0.send(req, cancel, line)
    }
}

/// What a worker needs, owned.
struct Job {
    script: PathBuf,
    which: Which,
    message: Option<String>,
    account: Option<String>,
    model: Option<String>,
    accounts: AccountsFile,
    seen: PathBuf,
    /// Disclosure texts accepted in this session.
    accepted: Vec<String>,
}

/// The worker thread's body: one `host::run`, then `Done`.
fn work(job: Job, transport: Arc<dyn Transport>, keys: Keys, tx: mpsc::Sender<ToUi>, cancel: CancelToken) {
    let result = (|| {
        let mut clocks = scriptorium_chat::clock::from_env().map_err(RunFailed::new)?;
        let mut seen = Seen::load(&job.seen);
        let req = Request {
            script: &job.script,
            which: job.which.clone(),
            message: job.message.as_deref(),
            account: job.account.as_deref(),
            model: job.model.as_deref(),
            allow_whole_script: false,
            wait: THREAD_WAIT,
            limits: Limits::default(),
        };
        let env = Env { accounts: &job.accounts, keys: &keys, seen: &mut seen, clocks: &mut clocks };
        let mut tools = WorkerTools { tx: tx.clone(), notes: vec![] };
        let mut ui = WorkerUi { tx: tx.clone(), accepted: job.accepted.clone() };
        host::run(&Shared(transport), &mut tools, &req, env, &mut ui, &cancel).map_err(RunFailed::from_host)
    })();
    let _ = tx.send(ToUi::Done(result));
}

/// A change for the message view, in the order it happened. The panel
/// drains them ([`Chat::take_view_ops`]) after each batch and hands them to
/// the message view in one adapter (`AgentPanel::apply_view_ops`), so the
/// view's API is touched in one place only.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewOp {
    /// Show thread `id` (`""` for a new conversation not sent yet): another
    /// one (opened, new, created by its first save).
    SetThread {
        id: String,
        thread: Thread,
    },
    /// A step's thread was saved: it replaces the streaming rows.
    PushTurn(Thread),
    /// Streamed events, a frame's worth.
    StreamEvents(Vec<Event>),
    /// Any other change to the stream.
    Stream(StreamOp),
    RunActive(bool),
    /// The thread's review in the open script (link and label).
    Review(Option<ReviewRef>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamOp {
    Wait(u64),
    Fail(AgentError),
    FailText { text: String, action: Option<ErrorAction> },
    Stop,
    Clear,
}

impl StreamOp {
    /// Applies it to a view model stream.
    pub fn apply(self, stream: &mut Stream) {
        match self {
            StreamOp::Wait(secs) => stream.wait(secs),
            StreamOp::Fail(e) => stream.fail(&e),
            StreamOp::FailText { text, action } => stream.fail_with(text, action),
            StreamOp::Stop => stream.stop(),
            StreamOp::Clear => stream.clear(),
        }
    }
}

/// The run in progress.
struct ActiveRun {
    cancel: CancelToken,
    /// Stop was pressed: late events are dropped.
    stopped: bool,
    /// The thread's id once known (a new thread's after its first save).
    thread_id: Option<String>,
    /// The run pushes a message of the writer's; set once it's saved.
    message: bool,
    task: Option<Task<()>>,
}

impl Drop for ActiveRun {
    fn drop(&mut self) {
        // A chat dropped mid-run (another script opened): stop the worker.
        self.cancel.cancel();
    }
}

/// One conversation in the panel: the open thread and the run on it. It
/// holds no rows: what the message view shows goes to it as [`ViewOp`]s.
pub struct Chat {
    script: WeakEntity<ScriptView>,
    script_path: PathBuf,
    /// The open thread as last saved; `None` for a new conversation not
    /// sent yet. Shared with the worker's copy of that step.
    thread: Option<Arc<ThreadFile>>,
    /// The request in flight's usage so far (the usage line adds it).
    usage: Option<Usage>,
    /// Where each call's suggestions went, by call id (the tool row's link).
    added: HashMap<String, Added>,
    /// What the run had to say (older tools, a review file left out…) and
    /// refusals to send: sticky until the next run.
    notices: Vec<String>,
    /// A tool call is waiting for the script's files (transient).
    busy_files: bool,
    question: Option<(Question, mpsc::Sender<bool>)>,
    run: Option<ActiveRun>,
    /// The account and model for the next run (the model chip); `None`: the
    /// thread's, else the default account's.
    choice: Option<(String, Option<String>)>,
    /// Disclosures offered (key → text) and those accepted in this session.
    offered: HashMap<String, String>,
    accepted: HashMap<String, String>,
    ops: Vec<ViewOp>,
    review: Option<ReviewRef>,
    /// The thread id the message view was last given.
    view_id: Option<String>,
}

impl EventEmitter<ChatEvent> for Chat {}

#[allow(dead_code)] // The composer (U6) and thread list (U7) use the rest.
impl Chat {
    pub fn new(script: WeakEntity<ScriptView>, script_path: PathBuf) -> Self {
        Self {
            script,
            script_path,
            thread: None,
            usage: None,
            added: HashMap::new(),
            notices: vec![],
            busy_files: false,
            question: None,
            run: None,
            choice: None,
            offered: HashMap::new(),
            accepted: HashMap::new(),
            ops: vec![],
            review: None,
            view_id: None,
        }
    }

    // ---- what the views read ----------------------------------------------

    /// The open thread as last saved (`None`: a new conversation).
    pub fn thread(&self) -> Option<&ThreadFile> {
        self.thread.as_deref()
    }

    /// The header's title: the thread's, or "New conversation".
    pub fn title(&self) -> String {
        self.thread.as_ref().map_or_else(|| "New conversation".to_string(), |t| t.title.clone())
    }

    /// The message view's changes since last asked.
    pub fn take_view_ops(&mut self) -> Vec<ViewOp> {
        std::mem::take(&mut self.ops)
    }

    /// The thread's usage plus the request in flight's (U6's usage line).
    pub fn usage(&self) -> Usage {
        let mut total = self.thread.as_ref().map(|t| t.thread.usage()).unwrap_or_default();
        if let Some(u) = self.usage {
            total.add(u);
        }
        total
    }

    /// A run is in progress (Send shows as Stop).
    pub fn is_running(&self) -> bool {
        self.run.is_some()
    }

    /// Stop was pressed and the run hasn't reported back yet.
    pub fn is_stopping(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.stopped)
    }

    /// A tool call is waiting for the script's files, held by another
    /// program (transient: "The script's files are busy… waiting").
    pub fn waiting_for_files(&self) -> bool {
        self.busy_files
    }

    /// The thread this chat is running, if known: never probe it (U7), the
    /// probe would fail against this process's own lock.
    pub fn running_thread(&self) -> Option<&str> {
        self.run.as_ref().and_then(|r| r.thread_id.as_deref())
    }

    /// What the run is asking the writer, if anything ([`Self::answer`]).
    pub fn question(&self) -> Option<&Question> {
        self.question.as_ref().map(|(q, _)| q)
    }

    /// Sticky lines: the run's notices and the last refusal to send.
    pub fn notices(&self) -> &[String] {
        &self.notices
    }

    /// Where call `id`'s suggestions went: the review's id and the numbers
    /// (`ScriptView::show_suggestion` with the first `Some`).
    pub fn added(&self, call_id: &str) -> Option<&Added> {
        self.added.get(call_id)
    }

    /// The account and model chosen for the next run.
    pub fn choice(&self) -> Option<(&str, Option<&str>)> {
        self.choice.as_ref().map(|(a, m)| (a.as_str(), m.as_deref()))
    }

    /// The account id and model the next run will use (the model chip):
    /// the choice, else the thread's, else the default account's.
    pub fn effective_model(&self) -> Result<(String, String), String> {
        let accounts = load_accounts().map_err(|e| e.to_string())?;
        let (account, model) = self.chosen();
        let r =
            host::settle_account(&accounts, self.thread(), account, model).map_err(|e| capitalized(&e.to_string()))?;
        Ok((r.account, r.model))
    }

    // ---- what the panel does ----------------------------------------------

    /// Sends `message` on the open thread (a new one if none): the run starts
    /// unless an error says why not. The draft should stay until
    /// [`ChatEvent::MessageSaved`].
    pub fn send(&mut self, message: &str, cx: &mut Context<Self>) -> Result<(), SendError> {
        if message.trim().is_empty() {
            return Err(SendError::Blank);
        }
        self.start(Some(message.to_string()), cx)
    }

    /// Sends the thread again as it is ([Retry], [Continue]).
    pub fn retry(&mut self, cx: &mut Context<Self>) -> Result<(), SendError> {
        if self.thread.is_none() {
            return Err(SendError::Blank);
        }
        self.start(None, cx)
    }

    /// The writer accepted disclosure `key` ([`SendError::Disclose`]). It
    /// counts for this session even if it can't be saved (the error says
    /// why, for the log).
    pub fn accept_disclosure(&mut self, key: &str) -> Result<(), String> {
        let text = self.offered.get(key).cloned().unwrap_or_default();
        self.accepted.insert(key.to_string(), text);
        let path = seen::default_path().ok_or("There's no settings folder to remember that in.")?;
        Seen::load(&path).insert(key)
    }

    /// Stop: the run ends at once as far as the writer sees (the thread as
    /// saved shows when the worker reports back).
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(run) = &mut self.run else { return };
        if run.stopped {
            return;
        }
        crate::log::info("agent: stop");
        run.stopped = true;
        run.cancel.cancel();
        self.busy_files = false;
        self.ops.push(ViewOp::Stream(StreamOp::Stop));
        if let Some((_, reply)) = self.question.take() {
            let _ = reply.send(false);
        }
        cx.notify();
    }

    /// The writer's answer to [`Self::question`].
    pub fn answer(&mut self, yes: bool, cx: &mut Context<Self>) {
        if let Some((_, reply)) = self.question.take() {
            let _ = reply.send(yes);
            cx.notify();
        }
    }

    /// The account and model for the next run (stored in the thread by it).
    pub fn choose(&mut self, account: String, model: Option<String>, cx: &mut Context<Self>) {
        self.choice = Some((account, model));
        cx.notify();
    }

    /// Shows thread `id` (read unlocked). Not while a run is in progress.
    pub fn open_thread(&mut self, id: &str, cx: &mut Context<Self>) -> Result<(), String> {
        if self.run.is_some() {
            return Err("Stop the agent before switching conversations.".into());
        }
        let file = self.store().read(id).map_err(|e| e.to_string())?;
        self.reset(Some(Arc::new(file)), cx);
        Ok(())
    }

    /// A new conversation, created on its first send. Not while a run is in
    /// progress.
    pub fn new_thread(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        if self.run.is_some() {
            return Err("Stop the agent before starting a new conversation.".into());
        }
        self.reset(None, cx);
        Ok(())
    }

    /// Whether thread `id` could be run now (U7's probe when opening it):
    /// this chat's own run counts as running here.
    pub fn probe(&self, id: &str) -> Result<(), StoreError> {
        if self.running_thread() == Some(id) {
            return Err(StoreError::RunningHere);
        }
        self.store().probe(id)
    }

    /// A sticky line for the writer: for the composer's (U6's) refusals to
    /// send (blank, still running, no account…). Runs report through their
    /// own notices.
    pub fn notice(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.notices.push(text.into());
        cx.notify();
    }

    // ---- inside -------------------------------------------------------------

    fn store(&self) -> ThreadStore {
        ThreadStore::for_script(&self.script_path)
    }

    fn chosen(&self) -> (Option<&str>, Option<&str>) {
        match &self.choice {
            Some((a, m)) => (Some(a.as_str()), m.as_deref()),
            None => (None, None),
        }
    }

    fn reset(&mut self, thread: Option<Arc<ThreadFile>>, cx: &mut Context<Self>) {
        self.thread = thread;
        self.usage = None;
        self.added.clear();
        self.notices.clear();
        self.busy_files = false;
        // A choice was for the thread it was made on.
        self.choice = None;
        self.ops.push(ViewOp::Stream(StreamOp::Clear));
        self.show(false);
        self.sync_review(cx);
        cx.notify();
    }

    /// Gives the message view the saved thread. `turn`: a step was saved,
    /// so it replaces the streaming rows. Another thread id than the view
    /// has (a new thread's first save) switches its thread.
    fn show(&mut self, turn: bool) {
        let id = self.thread.as_ref().map_or_else(String::new, |t| t.id.clone());
        let thread = self.thread.as_ref().map(|t| t.thread.clone()).unwrap_or_default();
        if self.view_id.as_deref() != Some(id.as_str()) {
            self.view_id = Some(id.clone());
            if turn {
                self.ops.push(ViewOp::Stream(StreamOp::Clear));
            }
            self.ops.push(ViewOp::SetThread { id, thread });
        } else if turn {
            self.ops.push(ViewOp::PushTurn(thread));
        } else {
            self.ops.push(ViewOp::SetThread { id, thread });
        }
    }

    /// Tells the view when the thread's review appears, goes or is renamed.
    fn sync_review(&mut self, cx: &App) {
        let now = self.review_ref(cx);
        if now != self.review {
            self.review = now.clone();
            self.ops.push(ViewOp::Review(now));
        }
    }

    /// The thread's review in the open script, if attached.
    fn review_ref(&self, cx: &App) -> Option<ReviewRef> {
        let thread = self.thread.as_ref()?;
        let id = thread.review_id.clone().unwrap_or_else(|| scriptorium_chat::naming::review_id(&thread.id));
        let view = self.script.upgrade()?;
        let session = view.read(cx).session();
        session.reviews.iter().find(|r| r.id == id).map(|r| ReviewRef { id: r.id.clone(), name: r.name.clone() })
    }

    /// The accounts, and the disclosure check (the UI shows it before any
    /// run, so no run waits on it).
    /// Also the model's warnings (`Resolved::warnings`: no tools here, retiring…).
    fn ready(&mut self) -> Result<(AccountsFile, PathBuf, Vec<String>), SendError> {
        let accounts = load_accounts()?;
        let (account, model) = self.chosen();
        let resolved = host::settle_account(&accounts, self.thread(), account, model)
            .map_err(|e| SendError::Setup(capitalized(&e.to_string())))?;
        let seen_path = seen::default_path().ok_or_else(|| SendError::Setup("There's no settings folder.".into()))?;
        if let Some((key, text)) = host::disclosure(&Seen::load(&seen_path), &resolved.base_url, &self.script_path)
            && !self.accepted.contains_key(&key)
        {
            self.offered.insert(key.clone(), text.clone());
            return Err(SendError::Disclose { key, text });
        }
        let warnings = resolved.warnings();
        Ok((accounts, seen_path, warnings))
    }

    fn start(&mut self, message: Option<String>, cx: &mut Context<Self>) -> Result<(), SendError> {
        if self.run.is_some() {
            return Err(SendError::Running);
        }
        let (accounts, seen, warnings) = self.ready()?;
        let transport = shared_transport().map_err(|e| SendError::Setup(e.to_string()))?;
        let job = Job {
            script: self.script_path.clone(),
            which: self.which(),
            message,
            account: self.choice.as_ref().map(|(a, _)| a.clone()),
            model: self.choice.as_ref().and_then(|(_, m)| m.clone()),
            accounts,
            seen,
            accepted: self.accepted.values().cloned().collect(),
        };
        self.launch(job, transport, Keys::system(), cx)?;
        // The model may not use tools here, is retiring…: say so for this run.
        self.notices.extend(warnings);
        Ok(())
    }

    /// The open thread, or a new one.
    fn which(&self) -> Which {
        match &self.thread {
            Some(t) => Which::Thread(t.id.clone()),
            None => Which::New,
        }
    }

    /// Starts `job` on a worker thread and the UI side of it.
    fn launch(
        &mut self,
        job: Job,
        transport: Arc<dyn Transport>,
        keys: Keys,
        cx: &mut Context<Self>,
    ) -> Result<(), SendError> {
        let (which, message) = (job.which.clone(), job.message.is_some());
        let (tx, rx) = mpsc::channel();
        let cancel = CancelToken::new();
        let worker_cancel = cancel.clone();
        std::thread::Builder::new()
            .name("agent-run".into())
            .spawn(move || work(job, transport, keys, tx, worker_cancel))
            .map_err(|e| SendError::Setup(format!("Couldn't start the agent: {e}")))?;
        crate::log::info(&format!(
            "agent: run on {} ({})",
            match &which {
                Which::Thread(id) => id.as_str(),
                Which::New => "a new thread",
            },
            if message { "message" } else { "again" }
        ));
        self.begin(which, message, rx, cancel, cx);
        Ok(())
    }

    /// The UI side of a run whose worker reports on `rx`.
    fn begin(
        &mut self,
        which: Which,
        message: bool,
        rx: mpsc::Receiver<ToUi>,
        cancel: CancelToken,
        cx: &mut Context<Self>,
    ) {
        let script = self.script.clone();
        let task = cx.spawn(async move |this, cx| pump(rx, this, script, cx).await);
        self.notices.clear();
        self.usage = None;
        self.busy_files = false;
        self.ops.push(ViewOp::Stream(StreamOp::Clear));
        self.ops.push(ViewOp::RunActive(true));
        self.run = Some(ActiveRun {
            cancel,
            stopped: false,
            thread_id: match which {
                Which::Thread(id) => Some(id),
                Which::New => None,
            },
            message,
            task: Some(task),
        });
        cx.notify();
    }

    fn stopped(&self) -> bool {
        self.run.as_ref().is_none_or(|r| r.stopped)
    }

    /// A frame's worth of messages from the worker (no tool calls).
    fn on_messages(&mut self, batch: Vec<ToUi>, cx: &mut Context<Self>) {
        for msg in batch {
            self.on_message(msg, cx);
        }
        cx.notify();
    }

    fn on_message(&mut self, msg: ToUi, cx: &mut Context<Self>) {
        match msg {
            ToUi::Done(result) => self.finish(result, cx),
            // Dropped once stopped: the thread as saved comes with Done.
            _ if self.stopped() => {}
            ToUi::Event(e) => {
                if let Event::Usage(u) = &e {
                    self.usage = Some(*u);
                }
                match self.ops.last_mut() {
                    Some(ViewOp::StreamEvents(events)) => events.push(e),
                    _ => self.ops.push(ViewOp::StreamEvents(vec![e])),
                }
            }
            ToUi::Waiting(secs) => self.ops.push(ViewOp::Stream(StreamOp::Wait(secs))),
            ToUi::Busy(waiting) => self.busy_files = waiting,
            // The round's results show when the round is saved (one push per
            // round, not a copy of the thread per call); the link is kept now.
            ToUi::ToolFinished(call, done) => {
                if let Some(a) = done.added {
                    self.added.insert(call.id.clone(), a);
                }
            }
            ToUi::Notice(text) => self.notices.push(text),
            ToUi::Saved(file) => {
                let run = self.run.as_mut().expect("not stopped, so running");
                run.thread_id = Some(file.id.clone());
                let first = std::mem::take(&mut run.message);
                self.thread = Some(file);
                self.usage = None;
                self.show(true);
                self.sync_review(cx);
                if first {
                    cx.emit(ChatEvent::MessageSaved);
                }
            }
            ToUi::Ask(q, reply) => {
                crate::log::info(&format!("agent: asks {q:?}"));
                self.question = Some((q, reply));
                cx.emit(ChatEvent::Asking);
            }
            ToUi::Gate { .. } | ToUi::Call { .. } => unreachable!("tool calls run on the script view"),
        }
    }

    /// The run is over: show the thread as saved and how it ended.
    fn finish(&mut self, result: Result<Finished, RunFailed>, cx: &mut Context<Self>) {
        let mut run = self.run.take();
        // This runs inside the task: let it finish rather than cancel itself.
        if let Some(task) = run.as_mut().and_then(|r| r.task.take()) {
            task.detach();
        }
        self.usage = None;
        self.busy_files = false;
        if let Some((_, reply)) = self.question.take() {
            let _ = reply.send(false);
        }
        let mut fail = None;
        let say = match result {
            Ok(done) => {
                crate::log::info(&format!("agent: run ended: {:?}", done.outcome));
                self.thread = Some(Arc::new(done.thread));
                match done.outcome {
                    Outcome::Finished | Outcome::Stopped => None,
                    Outcome::Withdrawn(text) => {
                        cx.emit(ChatEvent::Withdrawn(text));
                        self.notices.push("The model declined that; your message is back in the composer.".into());
                        Some("The model declined your message.".to_string())
                    }
                    Outcome::Error(e) => {
                        crate::log::warn(&format!("agent: {e}"));
                        let say = format!("The agent stopped: {}", super::view_model::plain_error(&e));
                        fail = Some(StreamOp::Fail(e));
                        Some(say)
                    }
                    Outcome::RoundLimit => {
                        let text = format!(
                            "Stopped: the agent reached its limit of {} tool rounds.",
                            Limits::default().max_tool_rounds
                        );
                        fail = Some(StreamOp::FailText { text: text.clone(), action: Some(ErrorAction::Continue) });
                        Some(text)
                    }
                }
            }
            Err(f) => {
                crate::log::warn(&format!("agent: run failed: {}", f.text));
                // The thread may have been saved before the failure.
                let id = run.as_ref().and_then(|r| r.thread_id.clone());
                if let Some(file) = id.and_then(|id| self.store().read(&id).ok()) {
                    self.thread = Some(Arc::new(file));
                }
                (!f.declined).then(|| {
                    fail = Some(StreamOp::FailText { text: f.text.clone(), action: f.action });
                    f.text
                })
            }
        };
        self.show(true);
        self.ops.push(ViewOp::RunActive(false));
        self.ops.extend(fail.map(ViewOp::Stream));
        self.sync_review(cx);
        drop(run);
        cx.emit(ChatEvent::Ended { say });
        cx.notify();
    }
}

/// The accounts file as it is now.
fn load_accounts() -> Result<AccountsFile, SendError> {
    let path =
        config::default_path().ok_or_else(|| SendError::Setup("There's no settings folder for AI accounts.".into()))?;
    config::load(&path).map_err(|e| SendError::Setup(capitalized(&e.to_string())))
}

/// The UI side of a run: drains the worker's channel about once a frame,
/// within [`FRAME_BUDGET`] and [`FRAME_MESSAGES`]. Tool calls update the
/// script view; the other messages go to the chat in one update per batch
/// (between tool calls). Each update is made from here, one at a time.
async fn pump(rx: mpsc::Receiver<ToUi>, chat: WeakEntity<Chat>, script: WeakEntity<ScriptView>, cx: &mut AsyncApp) {
    loop {
        let started = Instant::now();
        let mut gone = false;
        let mut batch = vec![];
        let mut taken = 0;
        while taken < FRAME_MESSAGES && started.elapsed() < FRAME_BUDGET {
            let msg = match rx.try_recv() {
                Ok(m) => m,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    gone = true;
                    break;
                }
            };
            taken += 1;
            let done = matches!(msg, ToUi::Done(_));
            if matches!(msg, ToUi::Call { .. } | ToUi::Gate { .. }) {
                // Everything before the call shows first.
                if flush(&mut batch, &chat, cx).is_err() || run_tool(msg, &chat, &script, cx).is_err() {
                    return; // The chat is gone; its run was cancelled with it.
                }
                continue;
            }
            batch.push(msg);
            if done {
                let _ = flush(&mut batch, &chat, cx);
                return;
            }
        }
        if flush(&mut batch, &chat, cx).is_err() {
            return;
        }
        if gone {
            // The worker ended without saying how (it panicked).
            let failed = RunFailed::new("The agent stopped unexpectedly; see the log.");
            chat.update(cx, |c, cx| c.finish(Err(failed), cx)).ok();
            return;
        }
        cx.background_executor().timer(FRAME).await;
    }
}

/// Hands `batch` to the chat. Err: the chat is gone.
fn flush(batch: &mut Vec<ToUi>, chat: &WeakEntity<Chat>, cx: &mut AsyncApp) -> Result<(), ()> {
    if batch.is_empty() {
        return Ok(());
    }
    let batch = std::mem::take(batch);
    chat.update(cx, |c, cx| c.on_messages(batch, cx)).map_err(drop)
}

/// Runs one tool call (or whole-script check) on the script view and answers
/// the worker. Err: the chat is gone.
fn run_tool(msg: ToUi, chat: &WeakEntity<Chat>, script: &WeakEntity<ScriptView>, cx: &mut AsyncApp) -> Result<(), ()> {
    let stopped = chat.read_with(cx, |c, _| c.stopped()).map_err(drop)?;
    match msg {
        ToUi::Call { file, call, reply } => {
            let answer = if stopped {
                (None, CallDone::new(&call, turn::CANCELLED, true), vec![])
            } else {
                let ran = script.update(cx, |view, cx| {
                    let mut file = *file;
                    let mut live = LiveScript::new(view, cx);
                    let done = exec::run_call(&mut live, &mut file, &call);
                    let notes = exec::Script::take_notes(&mut live);
                    (Some((file.review_id, file.review_path)), done, notes)
                });
                ran.unwrap_or_else(|_| (None, CallDone::new(&call, CLOSED, true), vec![]))
            };
            let _ = reply.send(answer);
        }
        ToUi::Gate { file, call, allow, price, reply } => {
            let answer = if stopped {
                Err(ReviewError::Failed(turn::CANCELLED.into()))
            } else {
                script
                    .update(cx, |view, cx| {
                        exec::whole_script_gate(
                            &mut LiveScript::new(view, cx),
                            &file,
                            &call.name,
                            allow,
                            price.as_ref(),
                        )
                    })
                    .unwrap_or_else(|_| Err(ReviewError::Failed(CLOSED.into())))
            };
            let _ = reply.send(answer);
        }
        _ => unreachable!("only tool calls"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! The worker's side of the channel, with the UI thread played by the
    //! test: tool calls answered over the CLI's file store, questions
    //! answered, Stop. The GPUI side is covered by the Sway scenarios.

    use super::*;
    use scriptorium_chat::exec::{Direct, FileScript};
    use scriptorium_providers::shell::{Replay, TimeScale};
    use scriptorium_providers::{Account, Provider};

    const SCRIPT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Scene Heading"><Text>EXT. FARM HOUSE - NIGHT</Text></Paragraph>
    <Paragraph Type="Action"><Text>Laura smiles.</Text></Paragraph>
    <Paragraph Type="Character"><Text>CESAR</Text></Paragraph>
    <Paragraph Type="Dialogue"><Text>I am go to the town tomorrow.</Text></Paragraph>
  </Content>
</FinalDraft>
"#;
    const OWN_REVIEW: &str = include_str!("../../tests/cassettes/own_review.jsonl");

    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("scriptorium-driver-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
            Folder(dir.canonicalize().unwrap())
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn job(f: &Folder, message: &str) -> Job {
        let mut account = Account::new("work", "Work", Provider::Anthropic);
        account.model = Some("claude-opus-5-5".into());
        let mut accounts = AccountsFile::default();
        accounts.upsert(account).unwrap();
        Job {
            script: f.0.join("film.fdx"),
            which: Which::New,
            message: Some(message.into()),
            account: None,
            model: None,
            accounts,
            seen: f.0.join("seen.json"),
            accepted: vec![],
        }
    }

    /// What the test's "UI thread" saw.
    #[derive(Default)]
    struct Seen {
        texts: String,
        asked: Vec<Question>,
        saved: usize,
        finished: Vec<CallDone>,
        done: Option<Result<Finished, RunFailed>>,
    }

    /// Runs a job on a worker and plays the UI thread until Done:
    /// tool calls over the CLI's store, every question answered `yes`.
    /// `on_text` runs on each text event (to stop the run).
    fn drive(f: &Folder, job: Job, cassette: &str, cancel: CancelToken, mut on_text: impl FnMut()) -> Seen {
        let transport: Arc<dyn Transport> = Arc::new(Replay::from_text("t", cassette, TimeScale::new(1.0)).unwrap());
        let (tx, rx) = mpsc::channel();
        let keys = Keys::in_memory(|_| Some("sk-test".into()));
        let worker = {
            let (cancel, transport) = (cancel.clone(), transport.clone());
            std::thread::spawn(move || work(job, transport, keys, tx, cancel))
        };
        let mut tools = Direct(FileScript::new(&f.0.join("film.fdx")).unwrap());
        let mut seen = Seen::default();
        for msg in rx {
            match msg {
                ToUi::Event(Event::Text { text, .. }) => {
                    seen.texts.push_str(&text);
                    on_text();
                }
                ToUi::Event(_) | ToUi::Waiting(_) | ToUi::Busy(_) | ToUi::Notice(_) => {}
                ToUi::ToolFinished(_, done) => seen.finished.push(done),
                ToUi::Saved(_) => seen.saved += 1,
                ToUi::Ask(q, reply) => {
                    seen.asked.push(q);
                    reply.send(true).unwrap();
                }
                ToUi::Gate { file, call, allow, price, reply } => {
                    reply.send(host::Tools::gate(&mut tools, &file, &call, allow, price.as_ref())).unwrap();
                }
                ToUi::Call { mut file, call, reply } => {
                    // Calls carry the thread without its messages.
                    assert!(file.thread.messages.is_empty() && file.system.is_empty());
                    let done = host::Tools::call(&mut tools, &mut file, &call);
                    reply.send((Some((file.review_id, file.review_path)), done, vec![])).unwrap();
                }
                ToUi::Done(r) => seen.done = Some(r),
            }
        }
        worker.join().unwrap();
        // Every exchange of the cassette was used.
        transport.finish().unwrap();
        seen
    }

    #[test]
    fn a_round_through_the_channel_adds_to_the_threads_review() {
        let f = Folder::new("round");
        let seen = drive(&f, job(&f, "Tighten the smile"), OWN_REVIEW, CancelToken::new(), || {});
        let done = seen.done.unwrap().unwrap_or_else(|e| panic!("{}", e.text));
        assert_eq!(done.outcome, Outcome::Finished);
        // Asked first: nothing was in seen.json.
        assert!(matches!(seen.asked.as_slice(), [Question::Disclose(_)]), "{:?}", seen.asked);
        assert_eq!(seen.texts, "One suggestion is waiting in your review.");
        // The message, the reply with the call, the results, the answer, the end.
        assert!(seen.saved >= 4, "{}", seen.saved);
        let added = seen.finished[0].added.as_ref().expect("added");
        assert_eq!(added.review_id, format!("thread-{}", done.thread.id));
        let review = std::fs::read_to_string(f.0.join("Tighten the smile.review.txt")).unwrap();
        assert!(review.contains("Laura grins."), "{review}");
        assert_eq!(done.thread.review_path.as_deref(), Some("Tighten the smile.review.txt"));
    }

    #[test]
    fn stop_mid_stream_ends_the_run_stopped() {
        let f = Folder::new("stop");
        // One reply that streams a text then would go on, slowly enough
        // (20 ms a line) that the stop lands mid-stream.
        let cassette = r#"{"delay_ms": 20, "lines": [
            "event: message_start",
            {"type": "message_start", "message": {"id": "m", "usage": {"input_tokens": 1, "output_tokens": 1}}}, "",
            "event: content_block_start",
            {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}, "",
            "event: content_block_delta",
            {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Half"}}, "",
            "event: content_block_delta",
            {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": " more"}}, ""
        ]}"#;
        let cancel = CancelToken::new();
        let c = cancel.clone();
        let seen = drive(&f, job(&f, "Hello"), cassette, cancel, move || c.cancel());
        let done = seen.done.unwrap().unwrap_or_else(|e| panic!("{}", e.text));
        assert_eq!(done.outcome, Outcome::Stopped);
        // Events race the cancel (the UI drops late ones); the partial reply
        // is kept in the thread.
        assert!(seen.texts.starts_with("Half"), "{}", seen.texts);
        let last = done.thread.thread.messages.last().unwrap();
        assert!(last.text().starts_with("Half"), "{}", last.text());
    }

    #[test]
    fn a_busy_thread_is_refused_at_once() {
        let f = Folder::new("busy");
        let seen = drive(&f, job(&f, "Tighten the smile"), OWN_REVIEW, CancelToken::new(), || {});
        let id = seen.done.unwrap().unwrap().thread.id;
        // Another program holds the thread.
        let _held = ThreadStore::for_script(&f.0.join("film.fdx")).lock(&id, Duration::ZERO).unwrap();
        let mut again = job(&f, "More");
        again.which = Which::Thread(id);
        let started = std::time::Instant::now();
        let seen = drive(&f, again, "", CancelToken::new(), || {});
        let failed = seen.done.unwrap().unwrap_err();
        assert_eq!(failed.text, BUSY_THREAD);
        // Refused after the short wait for this process's own stopping runs.
        assert!(started.elapsed() >= THREAD_WAIT && started.elapsed() < THREAD_WAIT * 4);
    }

    #[test]
    fn host_errors_read_for_the_writer() {
        assert_eq!(RunFailed::from_host(HostError::Store(StoreError::Busy)).text, BUSY_THREAD);
        assert!(RunFailed::from_host(HostError::Declined).declined);
        assert_eq!(RunFailed::from_host(HostError::NothingToSend).text, "Type a message to send");
    }

    // ---- over a live ScriptView (gpui::TestAppContext) ----------------------

    use gpui::{AppContext as _, Entity, TestAppContext, WindowHandle};
    use scriptorium_chat::exec::EDITING_LINE;

    /// A script view of `f`'s script and a chat on it.
    fn live(cx: &mut TestAppContext, f: &Folder) -> (WindowHandle<ScriptView>, Entity<ScriptView>, Entity<Chat>) {
        cx.update(gpui_base::init);
        let path = f.0.join("film.fdx");
        let opened = crate::script_view::Opened::load(&path, &[]).unwrap();
        let window = cx.add_window(|window, cx| ScriptView::new(opened, window, cx));
        let view = window.root(cx).unwrap();
        let weak = view.downgrade();
        let chat = cx.new(|_| Chat::new(weak, path));
        (window, view, chat)
    }

    /// Runs `job` on a real worker over the live view, replaying `cassette`;
    /// returns once the run is over (or panics after a while).
    fn run_live(cx: &mut TestAppContext, chat: &Entity<Chat>, job: Job, cassette: &str) -> Arc<dyn Transport> {
        let transport: Arc<dyn Transport> = Arc::new(Replay::from_text("t", cassette, TimeScale::new(1.0)).unwrap());
        let keys = Keys::in_memory(|_| Some("sk-test".into()));
        let t = transport.clone();
        chat.update(cx, |c, cx| c.launch(job, t, keys, cx)).unwrap();
        settle(cx, |cx| chat.read_with(cx, |c, _| !c.is_running()));
        transport
    }

    /// Lets the UI side run (fake time) and real worker threads go on until `done`.
    fn settle(cx: &mut TestAppContext, done: impl Fn(&mut TestAppContext) -> bool) {
        for _ in 0..3000 {
            cx.executor().advance_clock(FRAME);
            cx.run_until_parked();
            if done(cx) {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("the run didn't end");
    }

    /// The disclosure for `f`'s script on the test account, as already shown.
    fn seen_disclosure(f: &Folder) {
        let job = job(f, "x");
        let resolved = host::settle_account(&job.accounts, None, None, None).unwrap();
        let mut seen = scriptorium_chat::seen::Seen::load(&job.seen);
        if let Some((key, _)) = host::disclosure(&seen, &resolved.base_url, &job.script) {
            seen.insert(&key).unwrap();
        }
    }

    /// The result of the thread's tool call `id`.
    fn result_of(chat: &Entity<Chat>, cx: &mut TestAppContext, id: &str) -> String {
        chat.read_with(cx, |c, _| {
            c.thread()
                .unwrap()
                .thread
                .messages
                .iter()
                .flat_map(|m| &m.blocks)
                .find_map(|b| match b {
                    scriptorium_agent::Block::ToolResult(r) if r.id == id => Some(r.content.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no result for {id}"))
        })
    }

    /// One `suggest_change` round on `para`, then an answer.
    fn suggest_cassette(message: &str, id: &str, para: u64, option: &str) -> String {
        let reply = OWN_REVIEW.replacen("\"Tighten\"", &format!("{message:?}"), 1).replace("toolu_1", id).replace(
            r#"\"paragraph_id\": 1, \"options\": [\"Laura grins.\"]"#,
            &format!(r#"\"paragraph_id\": {para}, \"options\": [\"{option}\"]"#),
        );
        // The second exchange checks only the call id; any result will do.
        reply.replace(r#""contains": "added suggestion #1""#, r#""contains": """#)
    }

    #[gpui::test]
    fn a_round_on_the_live_view_waits_out_a_busy_session_lock(cx: &mut TestAppContext) {
        let f = Folder::new("live-busy");
        seen_disclosure(&f);
        let (_w, view, chat) = live(cx, &f);
        // Another program holds the session lock for a moment.
        let lock_path = scriptorium_fsio::SessionLock::path_for(&f.0.join("film.fdx"));
        let held = scriptorium_fsio::SessionLock::acquire(&lock_path, Duration::ZERO).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            drop(held);
        });
        let started = std::time::Instant::now();
        let transport = run_live(cx, &chat, job(&f, "Tighten the smile"), OWN_REVIEW);
        // The suggestion waited for the lock (retried off the UI thread).
        assert!(started.elapsed() >= Duration::from_millis(500));
        release.join().unwrap();
        transport.finish().unwrap();
        assert!(result_of(&chat, cx, "toolu_1").starts_with("added suggestion #1"));
        // The live view has the thread's review, written by the agent; the
        // session file is the view's to save.
        let id = chat.read_with(cx, |c, _| c.thread().unwrap().review_id.clone().unwrap());
        assert!(view.read_with(cx, |v, _| v.session().reviews.iter().any(|r| r.id == id)));
        assert!(std::fs::read_to_string(f.0.join("Tighten the smile.review.txt")).unwrap().contains("Laura grins."));
        assert!(!chat.read_with(cx, |c, _| c.waiting_for_files()));
    }

    #[gpui::test]
    fn a_proposal_on_the_line_being_rewritten_is_refused_on_the_live_view(cx: &mut TestAppContext) {
        let f = Folder::new("live-editing");
        seen_disclosure(&f);
        let (window, view, chat) = live(cx, &f);
        run_live(cx, &chat, job(&f, "Tighten the smile"), OWN_REVIEW);
        let (thread_id, review_id) =
            chat.read_with(cx, |c, _| (c.thread().unwrap().id.clone(), c.thread().unwrap().review_id.clone().unwrap()));
        // The writer starts rewriting suggestion #1 (on ¶1)…
        window.update(cx, |v, window, cx| v.rewrite_for_test(&review_id, 1, window, cx)).unwrap();
        assert!(view.read_with(cx, |v, _| v.is_rewriting()));
        // …and an agent change meanwhile leaves the rewrite alone.
        view.update(cx, |v, cx| v.after_agent_change(cx));
        assert!(view.read_with(cx, |v, _| v.is_rewriting()));
        // Another conversation's agent proposes on that line, into its own
        // review (so it isn't a duplicate there).
        let _ = thread_id;
        chat.update(cx, |c, cx| c.new_thread(cx)).unwrap();
        run_live(cx, &chat, job(&f, "Once more"), &suggest_cassette("Once more", "toolu_9", 1, "Laura beams."));
        let result = result_of(&chat, cx, "toolu_9");
        assert!(result.contains(EDITING_LINE), "{result}");
        assert!(view.read_with(cx, |v, _| v.is_rewriting()));
        let other = std::fs::read_to_string(f.0.join("Once more.review.txt")).unwrap_or_default();
        assert!(!other.contains("Laura beams."), "{other}");
    }

    /// A run whose "worker" is the test: the chat's UI side over `rx`'s sender.
    fn fake_run(cx: &mut TestAppContext, chat: &Entity<Chat>) -> (mpsc::Sender<ToUi>, CancelToken) {
        let (tx, rx) = mpsc::channel();
        let cancel = CancelToken::new();
        let c = cancel.clone();
        chat.update(cx, |chat, cx| chat.begin(Which::New, true, rx, c, cx));
        (tx, cancel)
    }

    fn call(id: &str, para: u64) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "suggest_change".into(),
            input: serde_json::json!({"paragraph_id": para, "options": ["Laura grins."]}),
            input_error: None,
            raw_input: None,
            extra: None,
        }
    }

    fn thread_file(f: &Folder) -> ThreadFile {
        let j = job(f, "Hello");
        let resolved = host::settle_account(&j.accounts, None, None, None).unwrap();
        host::new_thread("t0001", "Hello", "film.fdx", &resolved, "2026-11-15T09:30:00Z")
    }

    #[gpui::test]
    fn a_call_queued_when_stop_is_pressed_never_runs(cx: &mut TestAppContext) {
        let f = Folder::new("live-stop-call");
        let (_w, view, chat) = live(cx, &f);
        let (tx, cancel) = fake_run(cx, &chat);
        let (reply, answer) = mpsc::channel();
        tx.send(ToUi::Call { file: Box::new(thread_file(&f)), call: call("c1", 1), reply }).unwrap();
        chat.update(cx, |c, cx| c.stop(cx));
        assert!(cancel.is_cancelled());
        cx.executor().advance_clock(FRAME);
        cx.run_until_parked();
        let (mark, done, _) = answer.try_recv().expect("answered");
        assert_eq!((mark, done.result.content.as_str()), (None, turn::CANCELLED));
        assert!(view.read_with(cx, |v, _| v.session().reviews.is_empty()));
        assert!(!f.0.join("Hello.review.txt").exists());
    }

    #[gpui::test]
    fn dropping_the_chat_answers_a_pending_question_and_call(cx: &mut TestAppContext) {
        let f = Folder::new("live-drop");
        let (_w, _view, chat) = live(cx, &f);
        let (tx, cancel) = fake_run(cx, &chat);
        let (q_reply, q_answer) = mpsc::channel();
        tx.send(ToUi::Ask(Question::WholeScript("Read it all?".into()), q_reply)).unwrap();
        cx.executor().advance_clock(FRAME);
        cx.run_until_parked();
        assert!(chat.read_with(cx, |c, _| c.question().is_some()));
        // A call arrives that the UI hasn't taken yet; then the chat goes.
        let (c_reply, c_answer) = mpsc::channel();
        tx.send(ToUi::Call { file: Box::new(thread_file(&f)), call: call("c1", 1), reply: c_reply }).unwrap();
        drop(chat);
        cx.update(|_| {});
        cx.run_until_parked();
        cx.executor().advance_clock(FRAME);
        cx.run_until_parked();
        // The worker's waits end: no answer is "no", no result is "closed".
        assert!(q_answer.recv_timeout(Duration::from_secs(1)).is_err());
        assert!(c_answer.recv_timeout(Duration::from_secs(1)).is_err());
        assert!(cancel.is_cancelled());
    }

    #[gpui::test]
    fn events_after_stop_are_dropped(cx: &mut TestAppContext) {
        let f = Folder::new("live-late");
        let (_w, _view, chat) = live(cx, &f);
        let (tx, _cancel) = fake_run(cx, &chat);
        tx.send(ToUi::Event(Event::Text { index: 0, text: "Half".into() })).unwrap();
        cx.executor().advance_clock(FRAME);
        cx.run_until_parked();
        chat.update(cx, |c, cx| c.stop(cx));
        chat.update(cx, |c, _| c.take_view_ops());
        let mut file = thread_file(&f);
        tx.send(ToUi::Event(Event::Text { index: 0, text: " more".into() })).unwrap();
        tx.send(ToUi::Busy(true)).unwrap();
        tx.send(ToUi::Saved(Arc::new(file.clone()))).unwrap();
        cx.executor().advance_clock(FRAME);
        cx.run_until_parked();
        let ops = chat.update(cx, |c, _| c.take_view_ops());
        assert!(ops.is_empty(), "{ops:?}");
        assert!(chat.read_with(cx, |c, _| c.is_stopping() && !c.waiting_for_files() && c.thread().is_none()));
        // The run reports back: the thread as saved shows, and it's over.
        assert!(file.thread.push_user("Hello"));
        let done = Finished { thread: file, outcome: Outcome::Stopped, created: true };
        tx.send(ToUi::Done(Ok(done))).unwrap();
        cx.executor().advance_clock(FRAME);
        cx.run_until_parked();
        assert!(chat.read_with(cx, |c, _| !c.is_running() && c.thread().is_some()));
        let ops = chat.update(cx, |c, _| c.take_view_ops());
        assert!(ops.iter().any(|o| matches!(o, ViewOp::SetThread { id, .. } if id == "t0001")), "{ops:?}");
        assert!(ops.contains(&ViewOp::RunActive(false)));
    }
}
