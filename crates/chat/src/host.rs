//! The blocking run loop: one run of a thread, from the writer's message to
//! the model's last reply. [`run`] is *the* loop, for both programs:
//! `scriptorium chat` calls it with [`exec::Direct`]`(FileScript)`; the app
//! calls it on a worker thread, with a [`Tools`] that sends each call to the
//! UI thread (where [`exec::run_call`] runs over the open script view inside
//! `update`) and blocks on the reply, a [`Ui`] behind a channel, and
//! [`Request::wait`] zero. The step helpers ([`settle_account`],
//! [`disclosure`], [`new_thread`], [`prepare`]) stay public for the panel's
//! own displays, not for a second loop.
//!
//! Order of a run:
//!
//! 1. The thread is read *unlocked* and the account and model settled (the
//!    request's, else the thread's, else the default account's).
//! 2. The disclosure, once per provider host and script ([`disclosure`]):
//!    [`Ui::disclose`] must accept it, or nothing is sent or written. No
//!    lock is held while the writer reads it.
//! 3. Only now is an existing thread locked (waiting [`Request::wait`]) and
//!    read again, so turns another program added are kept, and the account
//!    settled again (if it changed to one needing a disclosure, the lock is
//!    dropped and step 2 repeats); if another program holds it,
//!    [`HostError::Store`] with [`StoreError::Busy`]. A new thread is
//!    created (and locked). Results of calls that finished before a crash
//!    ([`FINISHED_KEY`]) are given back to their calls ([`turn::resume`]);
//!    the message is pushed.
//! 4. The loop over [`Run`]: each request through [`receive`] with the run's
//!    [`CancelToken`]; each tool call through [`Tools`] (whole-script check,
//!    then the call; a busy call is tried again every [`exec::BUSY_RETRY`]
//!    for up to [`exec::BUSY_FOR`]); retry waits scaled by the clock. The
//!    thread file is written after every step and after *every tool call*
//!    (with the results so far), so a crash loses at most one step and never
//!    runs a finished call again. Before a call that may create the thread's
//!    review, its planned name is saved in the thread
//!    ([`exec::plan_review_path`]).
//! 5. [`Transport::finish`], on every exit, errors included: a replayed
//!    cassette with exchanges left over fails a run that otherwise succeeded
//!    ([`HostError::Transport`]), after the thread was saved.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use scriptorium_agent::file::{ThreadFile, ToolsCheck, VERSION, check_tools, tools_hash};
use scriptorium_agent::turn::{self, Action, Input, Limits, Outcome, Run};
use scriptorium_agent::{AgentError, Event, Thread, ToolCall, ToolResult};
use scriptorium_providers::shell::{CancelToken, Keys, TimeScale, Transport, receive};
use scriptorium_providers::{AccountsFile, Price, PriceBook, ProvidersError, Resolved};

use crate::clock::{Clock, Clocks};
use crate::exec::{self, CallDone, Gate, ReviewError};
use crate::lock::CLI_WAIT;
use crate::model::{disclosure_key, disclosure_text, host_of};
use crate::naming;
use crate::seen::Seen;
use crate::store::{StoreError, ThreadStore};

/// Which thread to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Which {
    /// A new thread (its title from the message).
    New,
    /// Thread `id`.
    Thread(String),
}

/// What to run.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    /// The script's path.
    pub script: &'a Path,
    pub which: Which,
    /// The writer's message; `None` to send the thread again as it is (after
    /// an error, or a run cut short: stranded calls are answered as
    /// cancelled first).
    pub message: Option<&'a str>,
    /// The account id to run on: `None` for the thread's, else the default.
    pub account: Option<&'a str>,
    /// The model: `None` for the thread's (on its account), else the account's default.
    pub model: Option<&'a str>,
    /// The writer allowed reading the whole script for this run
    /// (`--allow-whole-script`): no confirm, even over a stored "Not now".
    pub allow_whole_script: bool,
    /// How long to wait for the thread's lock: [`CLI_WAIT`] for the CLI,
    /// zero for the app (it shows a busy thread at once).
    pub wait: Duration,
    pub limits: Limits,
}

impl<'a> Request<'a> {
    /// A run with the CLI's defaults.
    pub fn new(script: &'a Path, which: Which, message: Option<&'a str>) -> Self {
        Request {
            script,
            which,
            message,
            account: None,
            model: None,
            allow_whole_script: false,
            wait: CLI_WAIT,
            limits: Limits::default(),
        }
    }
}

/// What the program shows and asks while a thread runs. Everything but the
/// two questions has a do-nothing default.
///
/// Called from the thread [`run`] runs on, so it works behind a channel: the
/// app's `Ui` forwards each call to its UI thread (events and notices
/// without waiting; the two questions block until the writer answers).
pub trait Ui {
    /// A streamed event (text, thinking, tool-call progress, usage).
    fn event(&mut self, _event: &Event) {}
    /// A request failed and will be retried in `secs` seconds (before scaling).
    fn waiting(&mut self, _secs: u64) {}
    fn tool_started(&mut self, _call: &ToolCall) {}
    fn tool_finished(&mut self, _call: &ToolCall, _done: &exec::CallDone) {}
    /// Something to tell the writer (older tools, a review file left out…).
    fn notice(&mut self, _text: &str) {}
    /// A call is waiting for the script's files (`true`, once, when it
    /// starts waiting), or stopped waiting (`false`: it ran, or gave up).
    /// Transient: the CLI says [`BUSY_WAITING`] once as a notice; the app
    /// shows a line while it lasts.
    fn busy(&mut self, waiting: bool) {
        if waiting {
            self.notice(BUSY_WAITING);
        }
    }
    /// The thread was saved after a step: the writer's message pushed, a
    /// reply pushed, a round's results pushed, a retry begun, the run over.
    /// Not called after each tool call (results so far go to
    /// [`Self::tool_finished`]). The app shows the thread from this.
    fn saved(&mut self, _file: &ThreadFile) {}
    /// A new thread ([`Which::New`]) was created and first saved as `id`.
    /// Called once, before anything is sent, so a run that then fails
    /// ([`HostError`] carries no id) still names its thread.
    fn thread_created(&mut self, _id: &str) {}
    /// The first-use disclosure ([`disclosure_text`]): send (true) or not.
    /// The CLI: true with `--yes`, else asks on a terminal, else false.
    fn disclose(&mut self, text: &str) -> bool;
    /// Before `read_script` sends a long script
    /// ([`exec::whole_script_question`]): send (true) or not. The answer is
    /// stored in the thread.
    fn confirm_whole_script(&mut self, question: &str) -> bool;
}

/// Runs the agent's tool calls for [`run`]. Called from the thread `run` runs
/// on, one call at a time, in order.
///
/// - The CLI: [`exec::Direct`]`(`[`exec::FileScript`]`)`.
/// - The app: sends each request to the UI thread, which runs
///   [`exec::whole_script_gate`] / [`exec::run_call`] over a `LiveScript`
///   borrowing its `&mut ScriptView` inside `update`, sends back the result
///   (and the updated [`ThreadFile`]), and blocks on it. A view that can't
///   take the call now answers [`ReviewError::Busy`] / [`CallDone::busy`];
///   `run` retries off the UI thread.
pub trait Tools {
    /// The whole-script rule for `call` ([`exec::whole_script_gate`]).
    fn gate(
        &mut self,
        file: &ThreadFile,
        call: &ToolCall,
        allow: bool,
        price: Option<&Price>,
    ) -> Result<Gate, ReviewError>;
    /// Run `call` for thread `file` ([`exec::run_call`]); may record the
    /// thread's review in `file`.
    fn call(&mut self, file: &mut ThreadFile, call: &ToolCall) -> CallDone;
    /// Notices from opening the script, each once.
    fn take_notes(&mut self) -> Vec<String> {
        vec![]
    }
}

/// Where a run's settings come from.
pub struct Env<'a> {
    pub accounts: &'a AccountsFile,
    pub keys: &'a Keys,
    /// Disclosures already shown ([`crate::seen`]).
    pub seen: &'a mut Seen,
    pub clocks: &'a mut Clocks,
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
    /// The thread as saved.
    pub thread: ThreadFile,
    pub outcome: Outcome,
    /// The thread was created by this run.
    pub created: bool,
}

/// Why a run didn't happen, or failed outside the agent loop (errors inside
/// it end the run with [`Outcome::Error`]).
#[derive(Debug, Clone, PartialEq)]
pub enum HostError {
    /// The thread store: busy in another program ([`StoreError::Busy`]), not
    /// found, unreadable…
    Store(StoreError),
    /// No such account, no model, no key.
    Providers(ProvidersError),
    /// No account given and no default one.
    NoAccount,
    /// The writer didn't accept the disclosure: nothing was sent or written.
    Declined,
    /// Nothing to send: an empty message, or a new thread without one.
    NothingToSend,
    /// The transport's [`Transport::finish`] failed (a cassette with
    /// exchanges left over). The thread was saved first.
    Transport(AgentError),
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostError::Store(e) => write!(f, "{e}"),
            HostError::Providers(e) => write!(f, "{e}"),
            HostError::NoAccount => {
                write!(f, "no account chosen: add one with `scriptorium accounts add`, or pick one")
            }
            HostError::Declined => write!(f, "nothing was sent"),
            HostError::NothingToSend => write!(f, "type a message to send"),
            HostError::Transport(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for HostError {}

impl From<StoreError> for HostError {
    fn from(e: StoreError) -> Self {
        HostError::Store(e)
    }
}

impl From<ProvidersError> for HostError {
    fn from(e: ProvidersError) -> Self {
        HostError::Providers(e)
    }
}

/// The account and model a run uses: `account` (the request's), else the
/// thread's, else the default account; `model` (the request's), else the
/// thread's when it stays on the thread's account, else the account's
/// default.
pub fn settle_account(
    accounts: &AccountsFile,
    thread: Option<&ThreadFile>,
    account: Option<&str>,
    model: Option<&str>,
) -> Result<Resolved, HostError> {
    let id = match account.or(thread.map(|t| t.account.as_str()).filter(|a| !a.is_empty())) {
        Some(id) => id.to_string(),
        None => accounts.default_account().ok_or(HostError::NoAccount)?.id.clone(),
    };
    let model = model.or_else(|| thread.filter(|t| t.account == id).map(|t| t.model.as_str()));
    Ok(accounts.resolve(&id, model)?)
}

/// The disclosure to show before sending to `base_url` about `script`, or
/// `None` if `seen` has it (or the URL has no host). Accepted, record it
/// with [`Seen::insert`] under the returned key.
pub fn disclosure(seen: &Seen, base_url: &str, script: &Path) -> Option<(String, String)> {
    let canonical = script.canonicalize().unwrap_or_else(|_| script.to_path_buf());
    let key = disclosure_key(base_url, &canonical.to_string_lossy())?;
    if seen.contains(&key) {
        return None;
    }
    let host = host_of(base_url)?;
    let name = file_name(script);
    let folder = ThreadStore::for_script(script).dir().file_name().map(|n| n.to_string_lossy().into_owned());
    Some((key, disclosure_text(&provider_label(&host), &name, &format!("{}/", folder.unwrap_or_default()))))
}

/// "Anthropic (api.anthropic.com)" for a preset provider's host; another
/// host as it is.
pub fn provider_label(host: &str) -> String {
    match scriptorium_providers::PRESETS.iter().find(|p| host_of(p.base_url).as_deref() == Some(host)) {
        Some(p) => format!("{} ({host})", p.name),
        None => host.to_string(),
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// A new thread file for `id`, started with `message`: titled from it, with
/// the built-in system prompt for `script_name` and the current tools.
pub fn new_thread(id: &str, message: &str, script_name: &str, resolved: &Resolved, now: &str) -> ThreadFile {
    ThreadFile {
        version: VERSION,
        id: id.to_string(),
        title: naming::thread_title(message),
        created: now.to_string(),
        updated: now.to_string(),
        account: resolved.account.clone(),
        model: resolved.model.clone(),
        review_id: None,
        review_path: None,
        whole_script: None,
        system: scriptorium_tools::prompt::system(script_name),
        tools_hash: tools_hash(&scriptorium_tools::catalogue()),
        thread: Thread::default(),
        extra: Default::default(),
    }
}

/// Readies a locked, freshly read thread for a run on `resolved`: records the
/// account and model, and if the tools changed since the thread started,
/// returns the notice to show (and records the current tools, so it's shown
/// once).
pub fn prepare(file: &mut ThreadFile, resolved: &Resolved) -> Option<&'static str> {
    file.account = resolved.account.clone();
    file.model = resolved.model.clone();
    let tools = scriptorium_tools::catalogue();
    (check_tools(file, &tools) == ToolsCheck::Changed).then(|| {
        file.tools_hash = tools_hash(&tools);
        TOOLS_CHANGED
    })
}

/// Shown when a thread's tools changed since it started.
pub const TOOLS_CHANGED: &str =
    "This conversation was started with older tools; the model's earlier reasoning won't be replayed.";

/// The key in [`ThreadFile::extra`] holding the results of a round's calls
/// that have finished, saved after each one; removed when the round's
/// results go into the thread. A thread read with it set crashed (or was
/// cut short) mid-round: [`run`] gives the results back to their calls
/// ([`turn::resume`]) so they aren't run again.
pub const FINISHED_KEY: &str = "finished_results";

/// Takes the results [`FINISHED_KEY`] holds out of `file`.
pub fn take_finished(file: &mut ThreadFile) -> Vec<ToolResult> {
    file.extra.remove(FINISHED_KEY).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

fn set_finished(file: &mut ThreadFile, results: &[ToolResult]) {
    let v = serde_json::to_value(results).expect("tool results always serialize");
    file.extra.insert(FINISHED_KEY.into(), v);
}

/// Shown once while a call waits for the script's files.
pub const BUSY_WAITING: &str = "The script's files are busy in another program; waiting…";

/// Changes thread `id`'s stored whole-script answer between runs (locks the
/// thread, waiting `wait`): the app's "Allow" on a refused `read_script`
/// passes `Some(true)`, "Ask again" `None`. During a run the answer comes
/// through [`Ui::confirm_whole_script`], or [`Request::allow_whole_script`]
/// for one run.
pub fn set_whole_script(
    threads: &ThreadStore,
    id: &str,
    answer: Option<bool>,
    wait: Duration,
) -> Result<ThreadFile, StoreError> {
    let (lock, mut file) = threads.lock_and_read(id, wait)?;
    let tokens = exec::whole_script_tokens(&file);
    exec::record_whole_script(&mut file, answer, tokens);
    threads.write(&lock, &file)?;
    Ok(file)
}

/// Runs one turn of a thread (see the module docs). `tools` runs the calls
/// ([`exec::Direct`]`(FileScript)` for the CLI). `cancel` stops the run: the
/// request in flight is dropped, a retry or busy wait ends, tools not yet
/// run are answered as cancelled. [`Transport::finish`] runs whatever the
/// result.
pub fn run(
    transport: &dyn Transport,
    tools: &mut dyn Tools,
    req: &Request,
    env: Env,
    ui: &mut dyn Ui,
    cancel: &CancelToken,
) -> Result<Finished, HostError> {
    let result = run_inner(transport, tools, req, env, ui, cancel);
    let finished = transport.finish();
    match (result, finished) {
        (Err(e), _) => Err(e),
        (Ok(_), Err(e)) => Err(HostError::Transport(e)),
        (Ok(done), Ok(())) => Ok(done),
    }
}

/// The key for `resolved`'s account, failing if it needs one and has none.
fn key_for(env: &Env, resolved: &Resolved) -> Result<Option<String>, HostError> {
    let account = env.accounts.get(&resolved.account).expect("settle_account found it");
    let key = env.keys.get(account).map(|(k, _)| k);
    if resolved.needs_key && key.is_none() {
        return Err(ProvidersError::MissingKey { account: resolved.account.clone() }.into());
    }
    Ok(key)
}

/// The disclosure for `resolved` still to show: not in `seen` and not
/// accepted earlier in this run.
fn pending_disclosure(
    seen: &Seen,
    accepted: &[String],
    resolved: &Resolved,
    script: &Path,
) -> Option<(String, String)> {
    disclosure(seen, &resolved.base_url, script).filter(|(k, _)| !accepted.contains(k))
}

fn run_inner(
    transport: &dyn Transport,
    tools: &mut dyn Tools,
    req: &Request,
    env: Env,
    ui: &mut dyn Ui,
    cancel: &CancelToken,
) -> Result<Finished, HostError> {
    let threads = ThreadStore::for_script(req.script);
    let message = req.message.filter(|m| !m.trim().is_empty());
    if req.message.is_some() && message.is_none() || req.which == Which::New && message.is_none() {
        return Err(HostError::NothingToSend);
    }
    // 1–3. The account and disclosure unlocked, then the thread locked.
    let mut accepted: Vec<String> = vec![];
    let (existing, resolved, key) = loop {
        let peek = match &req.which {
            Which::Thread(id) => Some(threads.read(id)?),
            Which::New => None,
        };
        let resolved = settle_account(env.accounts, peek.as_ref(), req.account, req.model)?;
        key_for(&env, &resolved)?;
        if let Some((k, text)) = pending_disclosure(env.seen, &accepted, &resolved, req.script) {
            if !ui.disclose(&text) {
                return Err(HostError::Declined);
            }
            if let Err(e) = env.seen.insert(&k) {
                ui.notice(&format!("couldn't remember that the disclosure was shown: {e}"));
            }
            accepted.push(k);
        }
        let existing = match &req.which {
            Which::Thread(id) => Some(threads.lock_and_read(id, req.wait)?),
            Which::New => None,
        };
        let resolved = settle_account(env.accounts, existing.as_ref().map(|(_, f)| f), req.account, req.model)?;
        let key = key_for(&env, &resolved)?;
        // The thread moved to another account meanwhile: ask about that one,
        // without holding the thread.
        if pending_disclosure(env.seen, &accepted, &resolved, req.script).is_none() {
            break (existing, resolved, key);
        }
    };
    for w in resolved.warnings() {
        ui.notice(&w);
    }
    let created = existing.is_none();
    let (lock, mut file) = match existing {
        Some((lock, mut file)) => {
            let finished = take_finished(&mut file);
            turn::resume(&mut file.thread, finished);
            if let Some(n) = prepare(&mut file, &resolved) {
                ui.notice(n);
            }
            (lock, file)
        }
        None => {
            let lock = threads.create(env.clocks.ids.as_mut())?;
            let file = new_thread(
                lock.id(),
                message.unwrap_or_default(),
                &file_name(req.script),
                &resolved,
                &env.clocks.clock.now(),
            );
            (lock, file)
        }
    };
    if let Some(w) = lock.warning() {
        ui.notice(w);
    }
    if let Some(m) = message {
        // Not blank (checked above), so it is added.
        let _added = file.thread.push_user(m);
    }
    let clock = &*env.clocks.clock;
    let save = |file: &mut ThreadFile| -> Result<(), HostError> {
        file.updated = clock.now();
        threads.write(&lock, file)?;
        Ok(())
    };
    save(&mut file)?;
    if created {
        ui.thread_created(&file.id);
    }
    ui.saved(&file);

    // 4. The loop.
    let ctx = RoundCtx {
        req,
        price: PriceBook::from_accounts(env.accounts).price(&resolved.origin()),
        dir: req.script.canonicalize().ok().and_then(|p| p.parent().map(Path::to_path_buf)),
        clock,
        cancel,
        save: &save,
    };
    let specs = scriptorium_tools::catalogue();
    let time = TimeScale::new(clock.scale(Duration::from_secs(1)).as_secs_f64());
    let mut run = Run::new(req.limits, resolved.protocol, resolved.origin());
    let mut action = run.start(&mut file.thread);
    let outcome = loop {
        let input = match action {
            Action::Send => {
                match scriptorium_providers::build_request(
                    &resolved,
                    key.as_deref(),
                    &file.system,
                    &specs,
                    &file.thread,
                ) {
                    Err(ProvidersError::Agent(e)) => Input::Failed(e),
                    Err(e) => Input::Failed(AgentError::NotReady(e.to_string())),
                    Ok(request) => {
                        match receive(transport, &request, resolved.protocol, cancel, &mut |e| ui.event(e)) {
                            Ok(r) => r.into_input(),
                            Err(e) => Input::Failed(e),
                        }
                    }
                }
            }
            Action::Wait { secs } => {
                ui.waiting(secs);
                if time.sleep_unless(secs, cancel) { Input::Retry } else { Input::Stop(vec![]) }
            }
            Action::RunTools(calls) => run_tools(&calls, tools, &mut file, &ctx, ui)?,
            Action::Done(outcome) => break outcome,
        };
        action = run.next(&mut file.thread, input);
        file.extra.remove(FINISHED_KEY);
        save(&mut file)?;
        ui.saved(&file);
    };
    save(&mut file)?;
    ui.saved(&file);
    Ok(Finished { thread: file, outcome, created })
}

/// What a round of tool calls needs besides the tools and the thread.
struct RoundCtx<'a> {
    req: &'a Request<'a>,
    price: Option<Price>,
    /// The script's folder, canonical, for planning the review's name.
    dir: Option<std::path::PathBuf>,
    clock: &'a dyn Clock,
    cancel: &'a CancelToken,
    save: &'a dyn Fn(&mut ThreadFile) -> Result<(), HostError>,
}

/// Runs a round's calls in order, saving the thread (with the results so
/// far under [`FINISHED_KEY`]) after each, and stopping with the results so
/// far when the run is cancelled.
fn run_tools(
    calls: &[ToolCall],
    tools: &mut dyn Tools,
    file: &mut ThreadFile,
    ctx: &RoundCtx,
    ui: &mut dyn Ui,
) -> Result<Input, HostError> {
    let mut results: Vec<ToolResult> = vec![];
    for call in calls {
        if ctx.cancel.is_cancelled() {
            return Ok(Input::Stop(results));
        }
        ui.tool_started(call);
        let done = run_one(call, tools, file, ctx, ui)?;
        for n in tools.take_notes() {
            ui.notice(&n);
        }
        ui.tool_finished(call, &done);
        results.push(done.result);
        set_finished(file, &results);
        (ctx.save)(file)?;
    }
    if ctx.cancel.is_cancelled() {
        return Ok(Input::Stop(results));
    }
    Ok(Input::Results(results))
}

/// One call: its planned review name saved first if it may create the
/// review, the whole-script check, then the call, each tried again while the
/// script's files are busy.
fn run_one(
    call: &ToolCall,
    tools: &mut dyn Tools,
    file: &mut ThreadFile,
    ctx: &RoundCtx,
    ui: &mut dyn Ui,
) -> Result<CallDone, HostError> {
    if scriptorium_tools::may_append(&call.name)
        && let Some(planned) = ctx.dir.as_deref().and_then(|dir| exec::plan_review_path(dir, file))
    {
        file.review_path = Some(planned);
        (ctx.save)(file)?;
    }
    let mut busy = Busy::default();
    let done = run_gated(call, tools, file, ctx, ui, &mut busy);
    if busy.tries > 0 {
        ui.busy(false);
    }
    done
}

/// [`run_one`] after its planned review name is saved.
fn run_gated(
    call: &ToolCall,
    tools: &mut dyn Tools,
    file: &mut ThreadFile,
    ctx: &RoundCtx,
    ui: &mut dyn Ui,
    busy: &mut Busy,
) -> Result<CallDone, HostError> {
    let gate = loop {
        match tools.gate(file, call, ctx.req.allow_whole_script, ctx.price.as_ref()) {
            Err(ReviewError::Busy) if busy.wait(ctx, ui) => continue,
            other => break other,
        }
    };
    let allowed = match gate {
        Err(e) => return Ok(CallDone { busy: false, ..CallDone::failed(call, &e) }),
        Ok(Gate::Run) => true,
        Ok(Gate::Refuse) => false,
        Ok(Gate::Ask { tokens, cost }) => {
            let yes = ui.confirm_whole_script(&exec::whole_script_question(tokens, cost));
            // Stopped while asking: no answer, so nothing is recorded.
            if ctx.cancel.is_cancelled() {
                return Ok(CallDone::new(call, turn::CANCELLED, true));
            }
            exec::record_whole_script(file, Some(yes), Some(tokens));
            yes
        }
    };
    if !allowed {
        return Ok(CallDone { result: exec::refused(call), added: None, busy: false });
    }
    loop {
        let done = tools.call(file, call);
        if !(done.busy && busy.wait(ctx, ui)) {
            return Ok(CallDone { busy: false, ..done });
        }
    }
}

/// Waiting out busy script files: every [`exec::BUSY_RETRY`] (scaled by the
/// clock) for up to [`exec::BUSY_FOR`].
#[derive(Default)]
struct Busy {
    tries: u32,
}

impl Busy {
    /// Waits before the next try; false once the time is up or the run is
    /// cancelled.
    fn wait(&mut self, ctx: &RoundCtx, ui: &mut dyn Ui) -> bool {
        let max = (exec::BUSY_FOR.as_millis() / exec::BUSY_RETRY.as_millis()) as u32;
        if self.tries >= max || ctx.cancel.is_cancelled() {
            return false;
        }
        if self.tries == 0 {
            ui.busy(true);
        }
        self.tries += 1;
        let until = std::time::Instant::now() + ctx.clock.scale(exec::BUSY_RETRY);
        while std::time::Instant::now() < until {
            if ctx.cancel.is_cancelled() {
                return false;
            }
            std::thread::sleep((until - std::time::Instant::now()).min(Duration::from_millis(20)));
        }
        true
    }
}
