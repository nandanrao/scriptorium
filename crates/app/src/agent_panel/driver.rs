//! The app's run driver: one conversation of the panel, and the runs on it.
//!
//! There is one run loop, `chat::host::run`, the CLI's too. The driver runs
//! it on a worker thread with:
//!
//! - a [`host::Tools`] that sends each tool call to the UI thread, where it
//!   runs (`exec::whole_script_gate` / `exec::run_call`) inside *one*
//!   `ScriptView` update over [`LiveScript`]; the worker blocks on the
//!   answer. A busy answer ([`CallDone::busy`]) is retried by `host::run`
//!   off the UI thread.
//! - a [`host::Ui`] behind the same channel: events, notices and saved
//!   threads go to the UI without waiting; the two questions (disclosure,
//!   whole-script read) block the worker until the writer answers.
//! - `Request::wait` zero: a thread another program is running is refused at
//!   once ("running in another program").
//!
//! The UI side is a GPUI task owned by [`Chat`] that drains the channel once
//! per frame (so repaints coalesce), and makes every entity update from its
//! own async context, one at a time: the script view for tool calls, the
//! chat for everything else, never one nested in the other.
//!
//! Stop cancels the run's [`CancelToken`] and marks the turn stopped at once;
//! events that arrive after it are dropped, a pending question is answered
//! "no", a tool call not yet run answers "not run". The thread as the run
//! saved it shows when the worker reports back.
//!
//! The disclosure is checked by the UI before a run starts ([`Chat::send`]
//! returns [`SendError::Disclose`]); if the run finds another one to show (the
//! thread moved to another account), it asks through [`Chat::question`].
//!
//! Runs go on while the panel is closed (the panel, and so its chat, live as
//! long as the script view); dropping the chat (another script opened, the
//! window closed) stops its run.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use gpui::{App, AsyncApp, Context, EventEmitter, Task, WeakEntity};
use scriptorium_agent::file::ThreadFile;
use scriptorium_agent::turn::{self, Limits, Outcome};
use scriptorium_agent::{
    AgentError, Block, Event, Message, Request as HttpRequest, Role, Thread, ToolCall, ToolResult, Usage,
};
use scriptorium_chat::exec::{self, Added, CallDone, Gate, ReviewError};
use scriptorium_chat::host::{self, Env, Finished, HostError, Request, Which};
use scriptorium_chat::seen::{self, Seen};
use scriptorium_chat::store::{StoreError, ThreadStore};
use scriptorium_providers::shell::{CancelToken, Ended, Flow, Keys, Transport, config, transport_from_env};
use scriptorium_providers::{AccountsFile, Price};

use super::live::LiveScript;
pub use super::messages::ReviewRef;
use super::view_model::Stream;
use crate::script_view::ScriptView;

/// How often the UI drains the worker's channel: about once a frame.
const FRAME: Duration = Duration::from_millis(16);

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
    retryable: bool,
    /// The writer said no to the disclosure: nothing to show.
    declined: bool,
}

impl RunFailed {
    fn new(text: impl Into<String>) -> Self {
        RunFailed { text: text.into(), retryable: false, declined: false }
    }

    fn from_host(e: HostError) -> Self {
        match e {
            HostError::Store(StoreError::Busy | StoreError::RunningHere) => RunFailed::new(BUSY_THREAD),
            HostError::Declined => RunFailed { declined: true, ..RunFailed::new("Nothing was sent.") },
            HostError::Transport(e) => RunFailed {
                retryable: matches!(e, AgentError::Api { retryable: true, .. }),
                ..RunFailed::new(e.to_string())
            },
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

/// From the worker to the UI thread.
enum ToUi {
    Event(Event),
    Waiting(u64),
    ToolFinished(ToolCall, CallDone),
    Notice(String),
    Saved(Box<ThreadFile>),
    Ask(Question, mpsc::Sender<bool>),
    Gate {
        file: Box<ThreadFile>,
        call: ToolCall,
        allow: bool,
        price: Option<Price>,
        reply: mpsc::Sender<Result<Gate, ReviewError>>,
    },
    Call {
        file: Box<ThreadFile>,
        call: ToolCall,
        /// The thread as the call left it (`None`: unchanged), the result,
        /// and notices from opening the script.
        reply: mpsc::Sender<(Option<Box<ThreadFile>>, CallDone, Vec<String>)>,
    },
    Done(Result<Finished, RunFailed>),
}

/// The worker's [`host::Ui`]: forwards to the UI thread.
struct WorkerUi(mpsc::Sender<ToUi>);

impl WorkerUi {
    fn ask(&self, q: Question) -> bool {
        let (tx, rx) = mpsc::channel();
        self.0.send(ToUi::Ask(q, tx)).is_ok() && rx.recv().unwrap_or(false)
    }
}

impl host::Ui for WorkerUi {
    fn event(&mut self, event: &Event) {
        let _ = self.0.send(ToUi::Event(event.clone()));
    }
    fn waiting(&mut self, secs: u64) {
        let _ = self.0.send(ToUi::Waiting(secs));
    }
    fn tool_finished(&mut self, call: &ToolCall, done: &CallDone) {
        let _ = self.0.send(ToUi::ToolFinished(call.clone(), done.clone()));
    }
    fn notice(&mut self, text: &str) {
        let _ = self.0.send(ToUi::Notice(text.to_string()));
    }
    fn saved(&mut self, file: &ThreadFile) {
        let _ = self.0.send(ToUi::Saved(Box::new(file.clone())));
    }
    fn disclose(&mut self, text: &str) -> bool {
        self.ask(Question::Disclose(text.to_string()))
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
        let msg = ToUi::Gate { file: Box::new(file.clone()), call: call.clone(), allow, price: price.copied(), reply };
        if self.tx.send(msg).is_err() {
            return Err(ReviewError::Failed(CLOSED.into()));
        }
        rx.recv().unwrap_or_else(|_| Err(ReviewError::Failed(CLOSED.into())))
    }

    fn call(&mut self, file: &mut ThreadFile, call: &ToolCall) -> CallDone {
        let (reply, rx) = mpsc::channel();
        if self.tx.send(ToUi::Call { file: Box::new(file.clone()), call: call.clone(), reply }).is_err() {
            return CallDone::new(call, CLOSED, true);
        }
        match rx.recv() {
            Ok((changed, done, notes)) => {
                if let Some(f) = changed {
                    *file = *f;
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
/// queue across runs (as one `scriptorium chat` per exchange group would be).
static TRANSPORT: OnceLock<Result<Arc<dyn Transport>, AgentError>> = OnceLock::new();

fn shared_transport() -> Result<Arc<dyn Transport>, AgentError> {
    TRANSPORT.get_or_init(|| transport_from_env(None).map(Arc::from)).clone()
}

/// The shared transport for one run. `finish` does nothing: `host::run`
/// calls it after every run, and a cassette for several runs has exchanges
/// left until the last (a replay left unfinished says so on stderr at exit).
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
            wait: Duration::ZERO,
            limits: Limits::default(),
        };
        let env = Env { accounts: &job.accounts, keys: &keys, seen: &mut seen, clocks: &mut clocks };
        let mut tools = WorkerTools { tx: tx.clone(), notes: vec![] };
        let mut ui = WorkerUi(tx.clone());
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
    /// one (opened, new, created by its first save), or the same one grown
    /// (a tool's result during a round).
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
    FailText { text: String, retryable: bool },
    Stop,
    Clear,
}

impl StreamOp {
    /// Applies it to a view model stream.
    pub fn apply(self, stream: &mut Stream) {
        match self {
            StreamOp::Wait(secs) => stream.wait(secs),
            StreamOp::Fail(e) => stream.fail(&e),
            StreamOp::FailText { text, retryable } => stream.fail_with(text, retryable),
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
    /// The open thread; `None` for a new conversation not sent yet.
    thread: Option<ThreadFile>,
    /// Results of the round in progress, shown before they are in the thread.
    round: Vec<ToolResult>,
    /// The request in flight's usage so far (the usage line adds it).
    usage: Option<Usage>,
    /// Where each call's suggestions went, by call id (the tool row's link).
    added: HashMap<String, Added>,
    /// What the run had to say (older tools, busy files…), this run only.
    notices: Vec<String>,
    question: Option<(Question, mpsc::Sender<bool>)>,
    run: Option<ActiveRun>,
    /// The account and model for the next run (the model chip); `None`: the
    /// thread's, else the default account's.
    choice: Option<(String, Option<String>)>,
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
            round: vec![],
            usage: None,
            added: HashMap::new(),
            notices: vec![],
            question: None,
            run: None,
            choice: None,
            ops: vec![],
            review: None,
            view_id: None,
        }
    }

    // ---- what the views read ----------------------------------------------

    /// The open thread as last saved (`None`: a new conversation).
    pub fn thread(&self) -> Option<&ThreadFile> {
        self.thread.as_ref()
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

    /// The thread this chat is running, if known: never probe it (U7), the
    /// probe would fail against this process's own lock.
    pub fn running_thread(&self) -> Option<&str> {
        self.run.as_ref().and_then(|r| r.thread_id.as_deref())
    }

    /// What the run is asking the writer, if anything ([`Self::answer`]).
    pub fn question(&self) -> Option<&Question> {
        self.question.as_ref().map(|(q, _)| q)
    }

    /// The run's notices, and the last refusal to send.
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

    /// The writer accepted disclosure `key` ([`SendError::Disclose`]).
    pub fn accept_disclosure(&mut self, key: &str) -> Result<(), String> {
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
        self.reset(Some(file), cx);
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

    /// A line for the writer, e.g. why a send was refused.
    pub fn notice(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.notices.push(text.into());
        cx.notify();
    }

    // ---- inside -------------------------------------------------------------

    fn store(&self) -> ThreadStore {
        ThreadStore::for_script(&self.script_path)
    }

    fn reset(&mut self, thread: Option<ThreadFile>, cx: &mut Context<Self>) {
        self.thread = thread;
        self.round.clear();
        self.usage = None;
        self.added.clear();
        self.notices.clear();
        self.ops.push(ViewOp::Stream(StreamOp::Clear));
        self.show(false);
        self.sync_review(cx);
        cx.notify();
    }

    /// Gives the message view the thread as shown. `turn`: a step was saved,
    /// so it replaces the streaming rows. Another thread id than the view
    /// has (a new thread's first save) switches its thread.
    fn show(&mut self, turn: bool) {
        let id = self.thread.as_ref().map_or_else(String::new, |t| t.id.clone());
        let thread = self.shown();
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

    /// The thread as shown: the saved one plus the round's results so far.
    fn shown(&self) -> Thread {
        shown(self.thread.as_ref(), &self.round)
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
    fn ready(&self) -> Result<(AccountsFile, PathBuf), SendError> {
        let path = config::default_path()
            .ok_or_else(|| SendError::Setup("There's no settings folder for AI accounts.".into()))?;
        let accounts = config::load(&path).map_err(|e| SendError::Setup(capitalized(&e.to_string())))?;
        let (account, model) = match &self.choice {
            Some((a, m)) => (Some(a.as_str()), m.as_deref()),
            None => (None, None),
        };
        let resolved = host::settle_account(&accounts, self.thread.as_ref(), account, model)
            .map_err(|e| SendError::Setup(capitalized(&e.to_string())))?;
        let seen_path = seen::default_path().ok_or_else(|| SendError::Setup("There's no settings folder.".into()))?;
        if let Some((key, text)) = host::disclosure(&Seen::load(&seen_path), &resolved.base_url, &self.script_path) {
            return Err(SendError::Disclose { key, text });
        }
        Ok((accounts, seen_path))
    }

    fn start(&mut self, message: Option<String>, cx: &mut Context<Self>) -> Result<(), SendError> {
        if self.run.is_some() {
            return Err(SendError::Running);
        }
        let (accounts, seen) = self.ready()?;
        let transport = shared_transport().map_err(|e| SendError::Setup(e.to_string()))?;
        let which = match &self.thread {
            Some(t) => Which::Thread(t.id.clone()),
            None => Which::New,
        };
        let job = Job {
            script: self.script_path.clone(),
            which: which.clone(),
            message: message.clone(),
            account: self.choice.as_ref().map(|(a, _)| a.clone()),
            model: self.choice.as_ref().and_then(|(_, m)| m.clone()),
            accounts,
            seen,
        };
        let (tx, rx) = mpsc::channel();
        let cancel = CancelToken::new();
        let worker_cancel = cancel.clone();
        std::thread::Builder::new()
            .name("agent-run".into())
            .spawn(move || work(job, transport, Keys::system(), tx, worker_cancel))
            .map_err(|e| SendError::Setup(format!("Couldn't start the agent: {e}")))?;
        crate::log::info(&format!(
            "agent: run on {} ({})",
            match &which {
                Which::Thread(id) => id.as_str(),
                Which::New => "a new thread",
            },
            if message.is_some() { "message" } else { "again" }
        ));
        let script = self.script.clone();
        let task = cx.spawn(async move |this, cx| pump(rx, this, script, cx).await);
        self.notices.clear();
        self.usage = None;
        self.ops.push(ViewOp::Stream(StreamOp::Clear));
        self.ops.push(ViewOp::RunActive(true));
        self.run = Some(ActiveRun {
            cancel,
            stopped: false,
            thread_id: match which {
                Which::Thread(id) => Some(id),
                Which::New => None,
            },
            message: message.is_some(),
            task: Some(task),
        });
        cx.notify();
        Ok(())
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
            ToUi::ToolFinished(call, done) => {
                if let Some(a) = done.added {
                    self.added.insert(call.id.clone(), a);
                }
                self.round.push(done.result);
                self.show(false);
                self.sync_review(cx);
            }
            ToUi::Notice(text) => self.notices.push(text),
            ToUi::Saved(file) => {
                let run = self.run.as_mut().expect("not stopped, so running");
                run.thread_id = Some(file.id.clone());
                let first = std::mem::take(&mut run.message);
                self.thread = Some(*file);
                self.round.clear();
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
        self.round.clear();
        self.usage = None;
        if let Some((_, reply)) = self.question.take() {
            let _ = reply.send(false);
        }
        let mut fail = None;
        let say = match result {
            Ok(done) => {
                crate::log::info(&format!("agent: run ended: {:?}", done.outcome));
                self.thread = Some(done.thread);
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
                        fail = Some(StreamOp::FailText { text: text.clone(), retryable: true });
                        Some(text)
                    }
                }
            }
            Err(f) => {
                crate::log::warn(&format!("agent: run failed: {}", f.text));
                // The thread may have been saved before the failure.
                let id = run.as_ref().and_then(|r| r.thread_id.clone());
                if let Some(file) = id.and_then(|id| self.store().read(&id).ok()) {
                    self.thread = Some(file);
                }
                (!f.declined).then(|| {
                    fail = Some(StreamOp::FailText { text: f.text.clone(), retryable: f.retryable });
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

/// `thread` plus the results of the round in progress, as one more message
/// (the one the round will push).
fn shown(thread: Option<&ThreadFile>, round: &[ToolResult]) -> Thread {
    let mut shown = thread.map(|t| t.thread.clone()).unwrap_or_default();
    if !round.is_empty() {
        shown.messages.push(Message {
            role: Role::User,
            blocks: round.iter().cloned().map(Block::ToolResult).collect(),
            origin: None,
            usage: None,
        });
    }
    shown
}

/// The UI side of a run: drains the worker's channel about once a frame.
/// Tool calls update the script view; the other messages go to the chat in
/// one update per frame (between tool calls). Each update is made from here,
/// one at a time.
async fn pump(rx: mpsc::Receiver<ToUi>, chat: WeakEntity<Chat>, script: WeakEntity<ScriptView>, cx: &mut AsyncApp) {
    loop {
        let mut gone = false;
        let mut batch = vec![];
        loop {
            let msg = match rx.try_recv() {
                Ok(m) => m,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    gone = true;
                    break;
                }
            };
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
                    (Some(Box::new(file)), done, notes)
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
            let cancel = cancel.clone();
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
                ToUi::Event(_) | ToUi::Waiting(_) | ToUi::Notice(_) => {}
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
                    let done = host::Tools::call(&mut tools, &mut file, &call);
                    reply.send((Some(file), done, vec![])).unwrap();
                }
                ToUi::Done(r) => seen.done = Some(r),
            }
        }
        worker.join().unwrap();
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
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn the_shown_thread_has_the_rounds_results_as_one_more_message() {
        let r = ToolResult { id: "a".into(), content: "ok".into(), is_error: false };
        assert!(shown(None, &[]).messages.is_empty());
        let t = shown(None, std::slice::from_ref(&r));
        assert_eq!(t.messages.len(), 1);
        assert_eq!(t.messages[0].role, Role::User);
        assert_eq!(t.messages[0].blocks, [Block::ToolResult(r)]);
    }

    #[test]
    fn host_errors_read_for_the_writer() {
        assert_eq!(RunFailed::from_host(HostError::Store(StoreError::Busy)).text, BUSY_THREAD);
        assert!(RunFailed::from_host(HostError::Declined).declined);
        assert_eq!(RunFailed::from_host(HostError::NothingToSend).text, "Type a message to send");
    }
}
