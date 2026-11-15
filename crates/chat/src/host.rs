//! The blocking run loop: one run of a thread, from the writer's message to
//! the model's last reply. `scriptorium chat` calls [`run`]; the app's driver
//! runs the same steps on its own (a GPUI task) with the helpers they share:
//! [`crate::exec::run_call`], [`crate::exec::whole_script_gate`],
//! [`disclosure`], [`new_thread`] and [`prepare`].
//!
//! Order of a run:
//!
//! 1. The thread: an existing one is locked (waiting [`Request::wait`]),
//!    then read again, so turns another program added are kept; if another
//!    program holds it, [`HostError::Store`] with
//!    [`StoreError::Busy`]. The account and model are settled (the
//!    request's, else the thread's, else the default account's).
//! 2. The disclosure, once per provider host and script ([`disclosure`]):
//!    [`Ui::disclose`] must accept it, or nothing is sent or written.
//! 3. A new thread is created (and locked) only now; the message is pushed.
//! 4. The loop over [`Run`]: each request through
//!    [`receive`] with the run's [`CancelToken`]; each tool call through
//!    [`crate::exec::run_call`] (after the whole-script check); retry waits
//!    scaled by the clock. The thread file is written after every step, so a
//!    crash loses at most one.
//! 5. [`Transport::finish`]: a replayed cassette with exchanges left over
//!    fails the run ([`HostError::Transport`]), after the thread was saved.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use scriptorium_agent::file::{ThreadFile, ToolsCheck, VERSION, check_tools, tools_hash};
use scriptorium_agent::turn::{Action, Input, Limits, Outcome, Run};
use scriptorium_agent::{AgentError, Event, Thread, ToolCall, ToolResult};
use scriptorium_providers::shell::{CancelToken, Keys, TimeScale, Transport, receive};
use scriptorium_providers::{AccountsFile, PriceBook, ProvidersError, Resolved};

use crate::clock::Clocks;
use crate::exec::{self, Gate, Script};
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
    /// How long to wait for the thread's lock ([`CLI_WAIT`] for the CLI).
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
pub trait Ui {
    /// A streamed event (text, thinking, tool-call progress, usage).
    fn event(&mut self, _event: &Event) {}
    /// A request failed and will be retried in `secs` seconds (before scaling).
    fn waiting(&mut self, _secs: u64) {}
    fn tool_started(&mut self, _call: &ToolCall) {}
    fn tool_finished(&mut self, _call: &ToolCall, _done: &exec::CallDone) {}
    /// Something to tell the writer (older tools, a review file left out…).
    fn notice(&mut self, _text: &str) {}
    /// The first-use disclosure ([`disclosure_text`]): send (true) or not.
    /// The CLI: true with `--yes`, else asks on a terminal, else false.
    fn disclose(&mut self, text: &str) -> bool;
    /// Before `read_script` sends a long script
    /// ([`exec::whole_script_question`]): send (true) or not. The answer is
    /// stored in the thread.
    fn confirm_whole_script(&mut self, question: &str) -> bool;
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
    Some((key, disclosure_text(&host, &name, &format!("{}/", folder.unwrap_or_default()))))
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

/// Runs one turn of a thread (see the module docs). `script` opens the
/// script for each tool call ([`exec::FileScript`] for the CLI).
/// `cancel` stops the run: the request in flight is dropped, a retry wait
/// ends, tools not yet run are answered as cancelled.
pub fn run(
    transport: &dyn Transport,
    script: &mut dyn Script,
    req: &Request,
    env: Env,
    ui: &mut dyn Ui,
    cancel: &CancelToken,
) -> Result<Finished, HostError> {
    let threads = ThreadStore::for_script(req.script);
    // 1. The thread (locked and re-read), and the account.
    let existing = match &req.which {
        Which::Thread(id) => Some(threads.lock_and_read(id, req.wait)?),
        Which::New => None,
    };
    let resolved = settle_account(env.accounts, existing.as_ref().map(|(_, f)| f), req.account, req.model)?;
    let account = env.accounts.get(&resolved.account).expect("settle_account found it");
    let key = env.keys.get(account).map(|(k, _)| k);
    if resolved.needs_key && key.is_none() {
        return Err(ProvidersError::MissingKey { account: resolved.account.clone() }.into());
    }
    let message = req.message.filter(|m| !m.trim().is_empty());
    if req.message.is_some() && message.is_none() || existing.is_none() && message.is_none() {
        return Err(HostError::NothingToSend);
    }
    // 2. The disclosure.
    if let Some((k, text)) = disclosure(env.seen, &resolved.base_url, req.script) {
        if !ui.disclose(&text) {
            return Err(HostError::Declined);
        }
        if let Err(e) = env.seen.insert(&k) {
            ui.notice(&format!("couldn't remember that the disclosure was shown: {e}"));
        }
    }
    // 3. The thread, ready to run.
    let created = existing.is_none();
    let (lock, mut file) = match existing {
        Some((lock, mut file)) => {
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

    // 4. The loop.
    let tools = scriptorium_tools::catalogue();
    let price = PriceBook::from_accounts(env.accounts).price(&resolved.origin());
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
                    &tools,
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
            Action::RunTools(calls) => {
                let input = run_tools(&calls, script, &mut file, req, price.as_ref(), ui, cancel);
                for n in script.take_notes() {
                    ui.notice(&n);
                }
                input
            }
            Action::Done(outcome) => break outcome,
        };
        action = run.next(&mut file.thread, input);
        save(&mut file)?;
    };
    save(&mut file)?;
    // 5. The transport's own check.
    transport.finish().map_err(HostError::Transport)?;
    Ok(Finished { thread: file, outcome, created })
}

/// Runs a round's calls in order, stopping (with the results so far) when
/// `cancel` is cancelled.
fn run_tools(
    calls: &[ToolCall],
    script: &mut dyn Script,
    file: &mut ThreadFile,
    req: &Request,
    price: Option<&scriptorium_providers::Price>,
    ui: &mut dyn Ui,
    cancel: &CancelToken,
) -> Input {
    let mut results: Vec<ToolResult> = vec![];
    for call in calls {
        if cancel.is_cancelled() {
            return Input::Stop(results);
        }
        ui.tool_started(call);
        let gate = match exec::whole_script_gate(script, file, &call.name, req.allow_whole_script, price) {
            Ok(g) => g,
            Err(e) => {
                let done = exec::CallDone {
                    result: ToolResult { id: call.id.clone(), content: e.to_string(), is_error: true },
                    added: None,
                };
                ui.tool_finished(call, &done);
                results.push(done.result);
                continue;
            }
        };
        let allowed = match gate {
            Gate::Run => true,
            Gate::Refuse => false,
            Gate::Ask { tokens, cost } => {
                let yes = ui.confirm_whole_script(&exec::whole_script_question(tokens, cost));
                file.whole_script = Some(yes);
                yes
            }
        };
        let done = if allowed {
            exec::run_call(script, file, call)
        } else {
            exec::CallDone { result: exec::refused(call), added: None }
        };
        ui.tool_finished(call, &done);
        results.push(done.result);
    }
    if cancel.is_cancelled() {
        return Input::Stop(results);
    }
    Input::Results(results)
}
