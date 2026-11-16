//! `scriptorium chat`, `threads` and `accounts`: the built-in agent from the
//! command line, on the same files as the app's agent panel (feature `chat`).
//!
//! - `chat` runs one turn of a thread through [`host::run`], the one run loop
//!   the app also uses, with the script opened from its files
//!   ([`Direct`]`(`[`FileScript`]`)`). The model's text streams to stdout;
//!   tool rows (the panel's labels), notices and the closing usage line go
//!   to stderr. With `--json` it prints one result object on stdout whatever
//!   happened (see [`ChatEnd`]), and the exit status says how it ended.
//!   Ctrl+C stops the run through its [`CancelToken`]: the thread is saved at
//!   every step, so `--retry` carries on from there.
//! - `threads` lists, shows, re-models and removes a script's threads
//!   (`<script>.threads/`). Changing a thread takes its lock, waiting
//!   [`CLI_WAIT`] before refusing a thread another program is running.
//! - `accounts` edits the per-user accounts file and keeps API keys in the
//!   OS keyring. Keys are read from stdin, never from the command line, and
//!   never printed.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use scriptorium_agent::file::ThreadFile;
use scriptorium_agent::protocol::parse_models;
use scriptorium_agent::turn::Outcome;
use scriptorium_agent::{AgentError, Block, Effort, ErrorKind, Event, Protocol, Role, ToolCall, ToolResult};
use scriptorium_chat::exec::{Added, CallDone, Direct, FileScript};
use scriptorium_chat::host::{self, Env, HostError, Request, Ui, Which};
use scriptorium_chat::labels;
use scriptorium_chat::lock::CLI_WAIT;
use scriptorium_chat::model::{Entry, parse_rfc3339};
use scriptorium_chat::seen::{self, Seen};
use scriptorium_chat::store::{Listed, StoreError, ThreadStore};
use scriptorium_chat::{clock, naming};
use scriptorium_fsio::opened::Opened;
use scriptorium_providers::shell::{self, CancelToken, Flow, KeyStatus, Keys, REPLAY_ENV, Transport, config};
use scriptorium_providers::{Account, AccountsFile, PriceBook, Provider, models_request, thread_cost};
use serde_json::{Value, json};

use crate::commands::{EXIT_BUSY, EXIT_DECLINED, EXIT_ERROR, EXIT_STOPPED, EXIT_USAGE, Failed};

/// `SCRIPTORIUM_KEYRING=off` (see the providers crate): `key set` and
/// `key delete` refuse.
pub use scriptorium_providers::shell::KEYRING_ENV;
use scriptorium_providers::shell::keyring_off;

pub fn command(cmd: &str, args: &[&str], notes: &mut Vec<String>) -> Result<String, Failed> {
    match cmd {
        "chat" => chat(args),
        "threads" => threads(args, notes),
        "accounts" => accounts(args, notes),
        _ => Err(Failed::new(EXIT_USAGE, format!("unknown command {cmd:?}"))),
    }
}

fn usage(message: impl Into<String>) -> Failed {
    Failed::new(EXIT_USAGE, message)
}

/// Arguments split into positionals, switches and `--flag value` pairs.
/// `--` ends the flags, so a message may start with `-`.
struct Args<'a> {
    positional: Vec<&'a str>,
    switches: Vec<&'a str>,
    values: HashMap<&'a str, &'a str>,
}

impl<'a> Args<'a> {
    fn parse(args: &[&'a str], switches: &[&str], valued: &[&str]) -> Result<Self, String> {
        let mut out = Args { positional: vec![], switches: vec![], values: HashMap::new() };
        let mut it = args.iter().copied();
        while let Some(a) = it.next() {
            if a == "--" {
                out.positional.extend(it.by_ref());
            } else if switches.contains(&a) {
                out.switches.push(a);
            } else if valued.contains(&a) {
                let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
                if out.values.insert(a, v).is_some() {
                    return Err(format!("{a} given twice"));
                }
            } else if a.starts_with("--") {
                return Err(format!("unknown option {a}"));
            } else {
                out.positional.push(a);
            }
        }
        Ok(out)
    }

    fn has(&self, switch: &str) -> bool {
        self.switches.contains(&switch)
    }

    fn get(&self, flag: &str) -> Option<&'a str> {
        self.values.get(flag).copied()
    }

    /// Refuses flags outside `allowed`.
    fn only(&self, allowed: &[&str]) -> Result<(), Failed> {
        let extra: Vec<&str> =
            self.switches.iter().chain(self.values.keys()).copied().filter(|f| !allowed.contains(f)).collect();
        if extra.is_empty() { Ok(()) } else { Err(usage(format!("unexpected {}", extra.join(", ")))) }
    }
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).expect("JSON values serialize") + "\n"
}

/// The keys: the OS keyring and the environment, or the environment alone
/// with `SCRIPTORIUM_KEYRING=off`.
fn keys() -> Keys {
    Keys::from_env()
}

fn accounts_path() -> Result<std::path::PathBuf, String> {
    config::default_path().ok_or_else(|| "no home folder to keep the accounts file in (set HOME)".to_string())
}

/// The accounts file. Its warnings (plain http to another machine…) are
/// shown by the commands that set up or test an account, not by every one.
fn load_accounts() -> Result<(std::path::PathBuf, AccountsFile), String> {
    let path = accounts_path()?;
    let file = config::load(&path).map_err(|e| e.to_string())?;
    Ok((path, file))
}

/// `arg` as a POSIX shell word.
fn sh(arg: &str) -> String {
    let plain = !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || "_./=:@%+-,".contains(c));
    if plain { arg.to_string() } else { format!("'{}'", arg.replace('\'', "'\\''")) }
}

fn command_line(args: &[&str]) -> String {
    std::iter::once("scriptorium".to_string()).chain(args.iter().map(|a| sh(a))).collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// chat

/// What went wrong, for `--json`'s `error`.
struct ErrorInfo {
    /// `usage`, `busy`, `declined`, `stopped`, `refused`, `round_limit`,
    /// `account`, `script`, `config`, `thread`, `transport`, or a provider
    /// failure: `auth`, `rate_limit`, `overloaded`, `offline`,
    /// `bad_request`, `other`, `parse`, `not_ready`, `mismatch`, `file`.
    kind: String,
    message: String,
    /// Whether running [`ChatEnd::retry`] (unchanged) may work.
    retryable: bool,
}

/// How a `chat` ended: printed as the `--json` object, or as text.
struct ChatEnd {
    /// `finished`, `stopped`, `error`, `round_limit`, `withdrawn`,
    /// `declined`, `busy`.
    outcome: &'static str,
    code: u8,
    thread: Option<String>,
    /// The thread as saved, when there is one.
    file: Option<ThreadFile>,
    created: Option<bool>,
    error: Option<ErrorInfo>,
    /// The command to run to try again.
    retry: Option<String>,
    added: Vec<Added>,
    usage: Option<Value>,
    usage_text: Option<String>,
}

impl ChatEnd {
    fn failed(code: u8, outcome: &'static str, kind: &str, message: impl Into<String>, retryable: bool) -> Self {
        ChatEnd {
            outcome,
            code,
            thread: None,
            file: None,
            created: None,
            error: Some(ErrorInfo { kind: kind.into(), message: message.into(), retryable }),
            retry: None,
            added: vec![],
            usage: None,
            usage_text: None,
        }
    }

    fn with_file(mut self, accounts: &AccountsFile, file: Option<ThreadFile>) -> Self {
        if let Some(f) = &file {
            self.thread = Some(f.id.clone());
            self.usage = Some(usage_json(accounts, f));
            self.usage_text = Some(usage_line(accounts, f));
        }
        self.file = file;
        self
    }

    fn render(self, json: bool) -> Result<String, Failed> {
        let f = self.file.as_ref();
        if json {
            let added: Vec<Value> =
                self.added.iter().map(|a| json!({"review_id": a.review_id, "numbers": a.numbers})).collect();
            let obj = json!({
                "outcome": self.outcome,
                "thread": self.thread,
                "title": f.map(|f| &f.title),
                "created": self.created,
                "account": f.map(|f| &f.account),
                "model": f.map(|f| &f.model),
                "review_path": f.and_then(|f| f.review_path.as_ref()),
                "added": added,
                "text": f.map(reply_text),
                "usage": self.usage,
                "usage_text": self.usage_text,
                "error": self.error.as_ref().map(|e| json!({"kind": e.kind, "message": e.message, "retryable": e.retryable})),
                "retry": self.retry,
            });
            return match &self.error {
                None => Ok(pretty(&obj)),
                Some(e) => Err(Failed { message: e.message.clone(), stdout: pretty(&obj), code: self.code }),
            };
        }
        match &self.error {
            None => {
                if let (Some(f), Some(u)) = (f, &self.usage_text) {
                    eprintln!("thread {} ({}) · {u}", f.id, f.title);
                }
                if let (false, Some(path)) = (self.added.is_empty(), f.and_then(|f| f.review_path.as_ref())) {
                    eprintln!("suggestions are in {path}");
                }
                Ok(String::new())
            }
            Some(e) => {
                let mut message = e.message.clone();
                if let Some(r) = &self.retry {
                    let _ = write!(message, " (try again: {r})");
                }
                Err(Failed::new(self.code, message))
            }
        }
    }
}

fn chat(args: &[&str]) -> Result<String, Failed> {
    let json = args.contains(&"--json");
    run_chat(args).render(json)
}

/// The message given as `-`: all of stdin, to its end.
fn read_message() -> Result<String, String> {
    let mut text = String::new();
    std::io::stdin().lock().read_to_string(&mut text).map_err(|e| format!("can't read the message from stdin: {e}"))?;
    Ok(text.trim_end().to_string())
}

fn run_chat(args: &[&str]) -> ChatEnd {
    let usage_end = |m: String| ChatEnd::failed(EXIT_USAGE, "error", "usage", m, false);
    let a = match Args::parse(
        args,
        &["--new", "--yes", "--allow-whole-script", "--retry", "--json"],
        &["--thread", "--account", "--model", "--record"],
    ) {
        Ok(a) => a,
        Err(e) => return usage_end(e),
    };
    let (script, message_arg) = match (a.positional.as_slice(), a.has("--retry")) {
        ([script], true) => (*script, None),
        ([_, _], true) => return usage_end("--retry sends the thread again as it is: leave out the message".into()),
        ([script, message], false) => (*script, Some(*message)),
        ([_], false) => return usage_end("what's the message? (or --retry to send the thread again)".into()),
        ([], _) => return usage_end(format!("wrong arguments for `chat`\n\n{}", crate::USAGE)),
        _ => return usage_end("put the message in quotes: one argument (or `-` to read it from stdin)".into()),
    };
    let which = match (a.get("--thread"), a.has("--new")) {
        (Some(_), true) => return usage_end("--thread and --new: choose one".into()),
        (Some(id), false) => Which::Thread(id.to_string()),
        (None, _) if a.has("--retry") => return usage_end("--retry needs --thread <id>".into()),
        (None, _) => Which::New,
    };
    let message = match message_arg {
        Some("-") => match read_message() {
            Ok(m) => Some(m),
            Err(e) => return usage_end(e),
        },
        other => other.map(str::to_string),
    };
    // The command as given, with a message from stdin written in: to run again.
    let original: Vec<&str> = std::iter::once("chat")
        .chain(
            args.iter()
                .map(|a| if *a == "-" && message_arg == Some("-") { message.as_deref().unwrap_or("") } else { a }),
        )
        .collect();
    let script_path = Path::new(script);
    let mut retry_saved: Vec<&str> = vec!["chat", script, "--thread", "", "--retry"];
    if a.has("--allow-whole-script") {
        retry_saved.push("--allow-whole-script");
    }
    if a.has("--json") {
        retry_saved.push("--json");
    }
    let retry_for = |id: &str| {
        let mut v = retry_saved.clone();
        v[3] = id;
        command_line(&v)
    };

    let setup = |kind: &str, e: String| ChatEnd::failed(EXIT_ERROR, "error", kind, e, false);
    let accounts = match load_accounts() {
        Ok((_, f)) => f,
        Err(e) => return setup("account", e),
    };
    let keys = keys();
    let Some(seen_path) = seen::default_path() else {
        return setup("config", "no home folder to remember disclosures in (set HOME)".into());
    };
    let mut seen = Seen::load(&seen_path);
    let mut clocks = match clock::from_env() {
        Ok(c) => c,
        Err(e) => return setup("config", e),
    };
    let mut tools = match FileScript::new(script_path) {
        Ok(s) => Direct(s),
        Err(e) => return setup("script", e),
    };
    if let Some(cassette) = std::env::var_os(REPLAY_ENV).filter(|v| !v.is_empty()) {
        eprintln!(
            "note: replaying {} ({REPLAY_ENV} is set): nothing is sent to a provider",
            Path::new(&cassette).display()
        );
    }
    let transport: Arc<dyn Transport> = match shell::transport_from_env(a.get("--record").map(Path::new)) {
        Ok(t) => Arc::from(t),
        Err(e) => return setup("transport", e.to_string()),
    };
    let store = ThreadStore::for_script(script_path);

    let mut req = Request::new(script_path, which.clone(), message.as_deref());
    req.account = a.get("--account");
    req.model = a.get("--model");
    req.allow_whole_script = a.has("--allow-whole-script");

    let cancel = CancelToken::new();
    let prompting = Arc::new(AtomicBool::new(false));
    stop_on_ctrl_c(&cancel, transport.clone(), prompting.clone());
    let mut ui = TermUi {
        yes: a.has("--yes"),
        json: a.has("--json"),
        cancel: cancel.clone(),
        prompting,
        mid_line: false,
        not_asked: false,
        created: None,
        added: vec![],
    };
    let env = Env { accounts: &accounts, keys: &keys, seen: &mut seen, clocks: &mut clocks };
    let result = host::run(&*transport, &mut tools, &req, env, &mut ui, &cancel);
    ui.line_break();
    let stopped = cancel.is_cancelled();

    let done = match result {
        Ok(done) => done,
        Err(e) => {
            // The thread, if this run got as far as making one.
            let id = match &which {
                Which::Thread(id) => Some(id.clone()),
                Which::New => ui.created.clone(),
            };
            let saved = id.as_deref().filter(|id| store.read(id).is_ok());
            let rerun = command_line(&original);
            let (code, outcome, kind, message, retryable, retry) = match &e {
                _ if stopped => (
                    EXIT_STOPPED,
                    "stopped",
                    "stopped",
                    "stopped before anything was sent".to_string(),
                    true,
                    Some(saved.map_or(rerun, retry_for)),
                ),
                HostError::Declined if ui.not_asked => {
                    let mut again = original.clone();
                    again.push("--yes");
                    (
                        EXIT_DECLINED,
                        "declined",
                        "declined",
                        "nothing was sent: to accept the disclosure above, run again with --yes".to_string(),
                        true,
                        Some(command_line(&again)),
                    )
                }
                HostError::Declined => (EXIT_DECLINED, "declined", "declined", e.to_string(), false, None),
                HostError::Store(StoreError::Busy) => (
                    EXIT_BUSY,
                    "busy",
                    "busy",
                    format!("thread {}: {e}", id.as_deref().unwrap_or("?")),
                    true,
                    Some(rerun),
                ),
                HostError::Store(_) => (EXIT_ERROR, "error", "thread", e.to_string(), false, None),
                HostError::Providers(_) | HostError::NoAccount => {
                    (EXIT_ERROR, "error", "account", e.to_string(), false, None)
                }
                HostError::NothingToSend => (EXIT_USAGE, "error", "usage", e.to_string(), false, None),
                HostError::Transport(_) => {
                    (EXIT_ERROR, "error", "transport", e.to_string(), false, saved.map(retry_for))
                }
            };
            let file = saved.and_then(|id| store.read(id).ok());
            let mut end = ChatEnd::failed(code, outcome, kind, message, retryable).with_file(&accounts, file);
            end.thread = end.thread.or(id);
            end.retry = retry;
            end.added = ui.added;
            return end;
        }
    };
    let id = done.thread.id.clone();
    let retry = retry_for(&id);
    let (code, outcome, error, retry) = match &done.outcome {
        Outcome::Finished => (0, "finished", None, None),
        Outcome::Stopped if can_resend(&done.thread) => (
            EXIT_STOPPED,
            "stopped",
            Some(ErrorInfo {
                kind: "stopped".into(),
                message: format!("stopped; thread {id} is saved"),
                retryable: true,
            }),
            Some(retry),
        ),
        // Stopped mid-reply: the partial reply is the thread's last word, so
        // there is nothing to send again; a new message carries on.
        Outcome::Stopped => (
            EXIT_STOPPED,
            "stopped",
            Some(ErrorInfo {
                kind: "stopped".into(),
                message: format!(
                    "stopped; thread {id} is saved with the reply so far (send a new message to carry on)"
                ),
                retryable: false,
            }),
            None,
        ),
        Outcome::Withdrawn(text) => (
            EXIT_ERROR,
            "withdrawn",
            Some(ErrorInfo {
                kind: "refused".into(),
                message: format!(
                    "the model declined; your message was taken back out of thread {id} (rephrase it and send again): {text}"
                ),
                retryable: false,
            }),
            None,
        ),
        Outcome::Error(e) => (
            EXIT_ERROR,
            "error",
            Some(ErrorInfo {
                kind: agent_kind(e).into(),
                message: format!("{e} (thread {id} is saved)"),
                retryable: agent_retryable(e),
            }),
            Some(retry),
        ),
        Outcome::RoundLimit => (
            EXIT_ERROR,
            "round_limit",
            Some(ErrorInfo {
                kind: "round_limit".into(),
                message: format!("stopped: the agent reached its limit of tool rounds in thread {id}"),
                retryable: true,
            }),
            Some(retry),
        ),
    };
    // A stop that came after the reply finished is no stop.
    let code = if stopped && outcome == "stopped" { EXIT_STOPPED } else { code };
    let mut end = ChatEnd {
        outcome,
        code,
        thread: None,
        file: None,
        created: Some(done.created),
        error,
        retry,
        added: ui.added,
        usage: None,
        usage_text: None,
    };
    end = end.with_file(&accounts, Some(done.thread));
    end
}

/// Whether `--retry` has something to send: the thread doesn't end on the
/// assistant's reply (calls still waiting are answered as cancelled first).
fn can_resend(file: &ThreadFile) -> bool {
    file.thread
        .messages
        .iter()
        .rev()
        .find(|m| !m.is_display_only())
        .is_some_and(|m| m.role != Role::Assistant || !m.tool_calls().is_empty())
}

fn agent_kind(e: &AgentError) -> &'static str {
    match e {
        AgentError::Api { kind, .. } => match kind {
            ErrorKind::Auth => "auth",
            ErrorKind::RateLimit => "rate_limit",
            ErrorKind::Overloaded => "overloaded",
            ErrorKind::Offline => "offline",
            ErrorKind::BadRequest => "bad_request",
            ErrorKind::Other => "other",
        },
        AgentError::Parse(_) => "parse",
        AgentError::NotReady(_) => "not_ready",
        AgentError::Mismatch(_) => "mismatch",
        AgentError::File(_) => "file",
    }
}

fn agent_retryable(e: &AgentError) -> bool {
    match e {
        AgentError::Api { retryable, .. } => *retryable,
        AgentError::Parse(_) | AgentError::Mismatch(_) => true,
        AgentError::NotReady(_) | AgentError::File(_) => false,
    }
}

/// Ctrl+C cancels the run (the thread was saved at its last step); a second
/// Ctrl+C quits at once, after closing a `--record` cassette so it never
/// ends in half an exchange.
fn stop_on_ctrl_c(cancel: &CancelToken, transport: Arc<dyn Transport>, prompting: Arc<AtomicBool>) {
    let cancel = cancel.clone();
    // Only one handler per process; a second `chat` in one process (tests
    // calling `run`) keeps the first, harmlessly.
    let _ = ctrlc::try_set_handler(move || {
        if cancel.is_cancelled() {
            let _ = transport.finish();
            std::process::exit(i32::from(EXIT_STOPPED));
        }
        if prompting.load(Ordering::SeqCst) {
            eprintln!("\nstopping… press Enter (or Ctrl+C again to quit at once)");
        } else {
            eprintln!("\nstopping… (Ctrl+C again to quit at once)");
        }
        cancel.cancel();
    });
}

/// The assistant's text after the last message the writer sent: what the
/// last run answered.
fn reply_text(file: &ThreadFile) -> String {
    let messages = &file.thread.messages;
    let start = messages.iter().rposition(|m| m.role == Role::User && !m.text().is_empty()).map_or(0, |i| i + 1);
    let texts: Vec<String> = messages[start..]
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .map(|m| m.text())
        .filter(|t| !t.trim().is_empty())
        .collect();
    texts.join("\n\n")
}

/// "130 tokens · $0.01" for the whole thread ("at least …+" after a
/// stopped reply, whose counts aren't reported).
fn usage_line(accounts: &AccountsFile, file: &ThreadFile) -> String {
    let cost = thread_cost(&PriceBook::from_accounts(accounts), &file.thread);
    let usage = file.thread.usage();
    labels::usage_text(&usage, (cost.complete || !usage.known).then_some(cost.usd))
}

/// The thread's usage so far. `input_tokens` counts all input, cached
/// included; `cost_usd` is a lower bound unless `cost_complete`.
fn usage_json(accounts: &AccountsFile, file: &ThreadFile) -> Value {
    let u = file.thread.usage();
    let cost = thread_cost(&PriceBook::from_accounts(accounts), &file.thread);
    json!({
        "input_tokens": u.input,
        "output_tokens": u.output,
        "cache_read_tokens": u.cached,
        "cache_write_tokens": u.cache_write,
        "cost_usd": cost.usd,
        "cost_complete": cost.complete,
    })
}

/// The terminal side of a run: streamed text on stdout, everything else on
/// stderr, and the two questions asked on a terminal (or answered by flags).
struct TermUi {
    /// `--yes`: accept the disclosure without asking.
    yes: bool,
    /// `--json`: nothing streams to stdout.
    json: bool,
    cancel: CancelToken,
    /// A question is waiting for an answer (Ctrl+C then says to press Enter).
    prompting: Arc<AtomicBool>,
    /// Stdout is in the middle of a line of streamed text.
    mid_line: bool,
    /// The disclosure was refused because there was nobody to ask.
    not_asked: bool,
    /// The new thread this run created ([`Ui::thread_created`]).
    created: Option<String>,
    added: Vec<Added>,
}

impl TermUi {
    /// Ends a streamed line before something else is printed.
    fn line_break(&mut self) {
        if self.mid_line {
            println!();
            self.mid_line = false;
        }
    }

    /// Asks `question` on the terminal; `None` when stdin isn't one. A
    /// Ctrl+C while it waits answers no.
    fn ask(&mut self, question: &str) -> Option<bool> {
        if !std::io::stdin().is_terminal() {
            return None;
        }
        eprint!("{question} [y/N] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        self.prompting.store(true, Ordering::SeqCst);
        let read = std::io::stdin().lock().read_line(&mut answer);
        self.prompting.store(false, Ordering::SeqCst);
        if self.cancel.is_cancelled() {
            return Some(false);
        }
        read.ok()?;
        Some(matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
    }
}

impl Ui for TermUi {
    fn event(&mut self, event: &Event) {
        if let Event::Text { text, .. } = event
            && !self.json
        {
            print!("{text}");
            let _ = std::io::stdout().flush();
            self.mid_line = !text.ends_with('\n');
        }
    }

    fn waiting(&mut self, secs: u64) {
        self.line_break();
        eprintln!("the provider is busy; retrying in {secs} s…");
    }

    fn tool_finished(&mut self, call: &ToolCall, done: &CallDone) {
        self.line_break();
        eprintln!("  ▸ {}", tool_row(call, Some(&done.result)));
        if let Some(a) = &done.added {
            self.added.push(a.clone());
        }
    }

    fn notice(&mut self, text: &str) {
        self.line_break();
        eprintln!("note: {text}");
    }

    fn thread_created(&mut self, id: &str) {
        self.created = Some(id.to_string());
    }

    fn disclose(&mut self, text: &str) -> bool {
        self.line_break();
        eprintln!("{text}");
        if self.yes {
            return true;
        }
        match self.ask("Send?") {
            Some(yes) => yes,
            None => {
                self.not_asked = true;
                false
            }
        }
    }

    fn confirm_whole_script(&mut self, question: &str) -> bool {
        self.line_break();
        match self.ask(&format!("{question} Send it?")) {
            Some(yes) => yes,
            None => {
                eprintln!("note: {question} Not sent: run with --allow-whole-script to allow it.");
                false
            }
        }
    }
}

/// A tool call's row, as the panel labels it.
fn tool_row(call: &ToolCall, result: Option<&ToolResult>) -> String {
    match result {
        None => labels::NOT_RUN.to_string(),
        Some(r) if r.is_error => labels::failed(&call.name, &r.content),
        Some(r) => labels::done(&call.name, &call.input, &r.content, None),
    }
}

// ---------------------------------------------------------------------------
// threads

fn store_failed(e: StoreError) -> Failed {
    let code = if e == StoreError::Busy { EXIT_BUSY } else { EXIT_ERROR };
    Failed::new(code, e.to_string())
}

fn threads(args: &[&str], notes: &mut Vec<String>) -> Result<String, Failed> {
    let a = Args::parse(args, &["--json"], &[]).map_err(usage)?;
    let json = a.has("--json");
    match a.positional.as_slice() {
        [script] => list_threads(script, json, notes),
        [script, "show", id] => show_thread(script, id, json, notes),
        [script, "remove", id] => remove_thread(script, id, json),
        [script, "set-model", id, account, model] => set_model(script, id, account, model, json, notes),
        _ => Err(usage(format!("wrong arguments for `threads`\n\n{}", crate::USAGE))),
    }
}

/// How a thread's review stands, from the script's saved session.
struct ReviewState {
    path: String,
    /// `None`: the file isn't attached (or doesn't exist yet).
    counts: Option<scriptorium_review::Progress>,
    exists: bool,
}

/// The script's session, opened for reading; `None` (with a note) if it can't be.
fn open_session(script: &str, notes: &mut Vec<String>) -> Option<Opened> {
    match Opened::open(script, &[], notes) {
        Ok(o) => Some(o),
        Err(e) => {
            notes.push(format!("can't read the script's reviews: {e}"));
            None
        }
    }
}

fn review_state(script: &str, file: &ThreadFile, opened: Option<&Opened>) -> Option<ReviewState> {
    let path = file.review_path.clone()?;
    let dir = Path::new(script).parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let exists = dir.join(&path).is_file();
    let counts = opened.and_then(|o| {
        let id = file.review_id.clone().unwrap_or_else(|| naming::review_id(&file.id));
        let r = o.session.reviews.iter().position(|r| r.id == id)?;
        Some(o.session.progress(&o.doc, r))
    });
    Some(ReviewState { path, counts, exists })
}

fn counts_json(p: &scriptorium_review::Progress) -> Value {
    json!({
        "total": p.total,
        "pending": p.pending,
        "decided": p.done(),
        "accepted": p.accepted,
        "kept": p.rejected,
        "rewritten": p.rewritten,
        "removed": p.removed,
    })
}

fn now_secs() -> Result<u64, String> {
    let clocks = clock::from_env()?;
    Ok(parse_rfc3339(&clocks.clock.now()).unwrap_or(0))
}

fn list_threads(script: &str, json: bool, notes: &mut Vec<String>) -> Result<String, Failed> {
    let store = ThreadStore::for_script(Path::new(script));
    let listed = store.list().map_err(store_failed)?;
    let opened = if listed.is_empty() { None } else { open_session(script, notes) };
    let accounts = load_accounts().map(|(_, a)| a).unwrap_or_default();
    // As the panel's list: another program is running the thread (readable
    // threads only; this program runs none).
    let busy = |l: &Listed| l.file.is_ok() && store.probe(&l.id) == Err(StoreError::Busy);
    let mut rows: Vec<(Entry, Option<&ThreadFile>, Option<ReviewState>, bool)> = listed
        .iter()
        .map(|l| {
            let file = l.file.as_ref().ok();
            let state = file.and_then(|f| review_state(script, f, opened.as_ref()));
            let total = state.as_ref().and_then(|s| s.counts.as_ref()).map(|p| p.total);
            (Entry::from_listed(l, total), file, state, busy(l))
        })
        .collect();
    // Newest first, as the panel lists them.
    rows.sort_by(|a, b| b.0.updated.cmp(&a.0.updated).then(b.0.id.cmp(&a.0.id)));
    if json {
        let out: Vec<Value> = rows
            .iter()
            .map(|(e, f, s, busy)| {
                json!({
                    "id": e.id,
                    "busy": busy,
                    "title": e.title,
                    "account": f.map(|f| f.account.as_str()),
                    "model": f.map(|f| f.model.as_str()),
                    "updated": f.map(|f| f.updated.as_str()),
                    "review_path": s.as_ref().map(|s| s.path.as_str()),
                    "suggestions": s.as_ref().and_then(|s| s.counts.as_ref()).map(counts_json),
                    "usage": f.map(|f| usage_json(&accounts, f)),
                    "problem": e.problem.as_ref().map(|p| p.reason()),
                })
            })
            .collect();
        return Ok(pretty(&Value::Array(out)));
    }
    if rows.is_empty() {
        return Ok(format!("no threads yet (start one: scriptorium chat {} \"<message>\")\n", sh(script)));
    }
    let now = now_secs()?;
    let mut out = String::new();
    let width = rows.iter().map(|r| r.0.id.len()).max().unwrap_or(0);
    for (e, _, _, busy) in &rows {
        let _ = writeln!(out, "{:<width$}  {}", e.id, e.title);
        let _ = writeln!(out, "{:<width$}  {} · busy: {}", "", e.detail(now), if *busy { "yes" } else { "no" });
    }
    Ok(out)
}

fn show_thread(script: &str, id: &str, json: bool, notes: &mut Vec<String>) -> Result<String, Failed> {
    let store = ThreadStore::for_script(Path::new(script));
    let file = store.read(id).map_err(store_failed)?;
    let opened = file.review_path.as_ref().and_then(|_| open_session(script, notes));
    let state = review_state(script, &file, opened.as_ref());
    let accounts = load_accounts().map(|(_, a)| a).unwrap_or_default();
    let usage = usage_line(&accounts, &file);
    let results: HashMap<&str, &ToolResult> =
        file.thread.messages.iter().flat_map(|m| m.tool_results()).map(|r| (r.id.as_str(), r)).collect();
    if json {
        let mut messages = vec![];
        for m in &file.thread.messages {
            match m.role {
                Role::User => messages.push(json!({"role": "user", "text": m.text()})),
                Role::Assistant => {
                    let calls: Vec<Value> = m
                        .tool_calls()
                        .iter()
                        .map(|c| {
                            let r = results.get(c.id.as_str());
                            json!({
                                "id": c.id,
                                "name": c.name,
                                "input": c.input,
                                "label": tool_row(c, r.copied()),
                                "result": r.map(|r| r.content.as_str()),
                                "is_error": r.map(|r| r.is_error),
                            })
                        })
                        .collect();
                    messages.push(json!({"role": "assistant", "text": m.text(), "tool_calls": calls}));
                }
                Role::Tool => {}
            }
            for b in &m.blocks {
                if let Block::Notice { text } = b {
                    messages.push(json!({"role": "notice", "text": text}));
                }
            }
        }
        return Ok(pretty(&json!({
            "id": file.id,
            "title": file.title,
            "account": file.account,
            "model": file.model,
            "created": file.created,
            "updated": file.updated,
            "review_id": file.review_id,
            "review_path": file.review_path,
            "review_exists": state.as_ref().map(|s| s.exists),
            "suggestions": state.as_ref().and_then(|s| s.counts.as_ref()).map(counts_json),
            "whole_script": file.whole_script,
            "usage": usage_json(&accounts, &file),
            "usage_text": usage,
            "messages": messages,
        })));
    }
    let mut out = String::new();
    let _ = writeln!(out, "{}  {}", file.id, file.title);
    let _ = writeln!(out, "  account   {} · {}", file.account, file.model);
    let _ = writeln!(out, "  updated   {}", file.updated);
    let _ = writeln!(out, "  usage     {usage}");
    match &state {
        None => {
            let _ = writeln!(out, "  review    none yet");
        }
        Some(s) => {
            let detail = match (&s.counts, s.exists) {
                (Some(p), _) => format!(
                    "{} {}: {} pending, {} decided",
                    p.total,
                    if p.total == 1 { "suggestion" } else { "suggestions" },
                    p.pending,
                    p.done()
                ),
                (None, true) => "not attached to the script".into(),
                (None, false) => "not written yet".into(),
            };
            let _ = writeln!(out, "  review    {} ({detail})", s.path);
        }
    }
    for m in &file.thread.messages {
        for b in &m.blocks {
            match (m.role, b) {
                (Role::User, Block::Text { text }) => {
                    let _ = writeln!(out, "\nYou: {text}");
                }
                (Role::Assistant, Block::Text { text }) if !text.trim().is_empty() => {
                    let _ = writeln!(out, "\nAgent: {text}");
                }
                (Role::Assistant, Block::ToolCall(c)) => {
                    let _ = writeln!(out, "  ▸ {}", tool_row(c, results.get(c.id.as_str()).copied()));
                }
                (_, Block::Notice { text }) => {
                    let _ = writeln!(out, "  ({text})");
                }
                _ => {}
            }
        }
    }
    Ok(out)
}

fn remove_thread(script: &str, id: &str, json: bool) -> Result<String, Failed> {
    let store = ThreadStore::for_script(Path::new(script));
    let (lock, file) = store.lock_and_read(id, CLI_WAIT).map_err(store_failed)?;
    store.delete(&lock).map_err(store_failed)?;
    if json {
        return Ok(pretty(&json!({"removed": id, "review_path": file.review_path})));
    }
    Ok(match &file.review_path {
        Some(p) => format!("removed thread {id}; its review {p} stays, with its decisions\n"),
        None => format!("removed thread {id}\n"),
    })
}

fn set_model(
    script: &str,
    id: &str,
    account: &str,
    model: &str,
    json: bool,
    notes: &mut Vec<String>,
) -> Result<String, Failed> {
    let (_, accounts) = load_accounts()?;
    let resolved = accounts.resolve(account, Some(model)).map_err(|e| e.to_string())?;
    notes.extend(resolved.warnings());
    let now = clock::from_env()?.clock.now();
    let store = ThreadStore::for_script(Path::new(script));
    let (lock, mut file) = store.lock_and_read(id, CLI_WAIT).map_err(store_failed)?;
    file.account = resolved.account.clone();
    file.model = resolved.model.clone();
    file.updated = now;
    store.write(&lock, &file).map_err(store_failed)?;
    if json {
        return Ok(pretty(&json!({"id": id, "account": file.account, "model": file.model})));
    }
    Ok(format!("thread {id} runs on {} · {} from its next message\n", file.account, file.model))
}

// ---------------------------------------------------------------------------
// accounts

const ACCOUNT_FIELDS: &[&str] = &["--name", "--model", "--effort", "--max-tokens", "--base-url"];

fn accounts(args: &[&str], notes: &mut Vec<String>) -> Result<String, Failed> {
    let a = Args::parse(
        args,
        &["--json", "--default", "--key-stdin"],
        &["--provider", "--name", "--model", "--base-url", "--protocol", "--effort", "--max-tokens"],
    )
    .map_err(usage)?;
    let json = a.has("--json");
    match a.positional.as_slice() {
        [] | ["list"] => {
            a.only(&["--json"])?;
            list_accounts(json)
        }
        ["add", id @ ..] if id.len() <= 1 => {
            let mut allowed = vec!["--json", "--default", "--key-stdin", "--provider", "--protocol"];
            allowed.extend(ACCOUNT_FIELDS);
            a.only(&allowed)?;
            add_account(id.first().copied(), &a, notes)
        }
        ["set", id] => {
            let mut allowed = vec!["--json"];
            allowed.extend(ACCOUNT_FIELDS);
            a.only(&allowed)?;
            set_account(id, &a, notes)
        }
        ["remove", id] => {
            a.only(&["--json"])?;
            remove_account(id, json, notes)
        }
        ["default", id] => {
            a.only(&["--json"])?;
            let (path, mut file) = load_accounts()?;
            file.set_default(id).map_err(|e| e.to_string())?;
            config::save(&path, &file).map_err(|e| e.to_string())?;
            Ok(if json { pretty(&json!({"default": id})) } else { format!("new threads use {id}\n") })
        }
        ["key", "set", id] => {
            a.only(&[])?;
            let (_, file) = load_accounts()?;
            let account = find(&file, id)?;
            refuse_keyring_off(account)?;
            let key = read_key()?;
            let stored = keys().set(account, &key);
            drop(key);
            stored.map_err(|e| e.to_string())?;
            Ok(format!("key for {id} saved in the keyring\n"))
        }
        ["key", "delete", id] => {
            a.only(&[])?;
            let (_, file) = load_accounts()?;
            let account = find(&file, id)?;
            refuse_keyring_off(account)?;
            keys().delete(account).map_err(|e| e.to_string())?;
            Ok(format!("key for {id} removed from the keyring (the account stays)\n"))
        }
        ["key", "status", id] => {
            a.only(&["--json"])?;
            let (_, file) = load_accounts()?;
            let account = find(&file, id)?;
            let status = keys().status(account);
            Ok(if json {
                pretty(&json!({"id": id, "key": key_json(&status)}))
            } else {
                format!("{id}: {}\n", key_text(&status))
            })
        }
        ["check", id] => {
            a.only(&["--json"])?;
            check_account(id, json, notes)
        }
        _ => Err(usage(format!("wrong arguments for `accounts`\n\n{}", crate::USAGE))),
    }
}

fn find<'a>(file: &'a AccountsFile, id: &str) -> Result<&'a Account, String> {
    file.get(id).ok_or_else(|| format!("there is no account \"{id}\" (see `scriptorium accounts list`)"))
}

/// With `SCRIPTORIUM_KEYRING=off` nothing is stored or deleted: say so
/// rather than pretend.
fn refuse_keyring_off(account: &Account) -> Result<(), String> {
    if keyring_off() {
        return Err(format!(
            "{KEYRING_ENV}=off: the keyring isn't used, so nothing was stored or deleted; set {} instead",
            account.key_env_vars().join(" or ")
        ));
    }
    Ok(())
}

/// "in keychain", "from SCRIPTORIUM_ANTHROPIC_KEY", "missing (…)": never the key.
fn key_text(status: &KeyStatus) -> String {
    match status {
        KeyStatus::Keyring => "in keychain".into(),
        KeyStatus::Env(var) => format!("from {var}"),
        KeyStatus::Missing { env_vars, keyring } => {
            let mut s = format!("missing (scriptorium accounts key set, or set {})", env_vars.join(" or "));
            if let Some(why) = keyring {
                let _ = write!(s, "; keyring: {why}");
            }
            s
        }
    }
}

fn key_json(status: &KeyStatus) -> Value {
    match status {
        KeyStatus::Keyring => json!({"status": "keyring"}),
        KeyStatus::Env(var) => json!({"status": "env", "var": var}),
        KeyStatus::Missing { env_vars, keyring } => {
            json!({"status": "missing", "env_vars": env_vars, "keyring_error": keyring})
        }
    }
}

fn provider_id(p: &Provider) -> &'static str {
    p.preset().map_or("custom", |p| p.id)
}

fn list_accounts(json: bool) -> Result<String, Failed> {
    let (_, file) = load_accounts()?;
    let keys = keys();
    let default = file.default_account().map(|a| a.id.clone());
    if json {
        let out: Vec<Value> = file
            .accounts
            .iter()
            .map(|a| {
                json!({
                    "id": a.id,
                    "name": a.name,
                    "provider": provider_id(&a.provider),
                    "protocol": a.provider.protocol(),
                    "base_url": a.provider.base_url(),
                    "model": a.model.as_deref().or(a.provider.default_model()),
                    "effort": a.effort,
                    "max_tokens": a.max_tokens,
                    "default": default.as_deref() == Some(a.id.as_str()),
                    "key": if a.provider.needs_key() { key_json(&keys.status(a)) } else { json!({"status": "not_needed"}) },
                })
            })
            .collect();
        return Ok(pretty(&Value::Array(out)));
    }
    if file.accounts.is_empty() {
        return Ok("no accounts yet (add one: scriptorium accounts add --provider anthropic --key-stdin)\n".into());
    }
    let mut out = String::new();
    for a in &file.accounts {
        let mark = if default.as_deref() == Some(a.id.as_str()) { "*" } else { " " };
        let key = if a.provider.needs_key() { key_text(&keys.status(a)) } else { "no key needed".into() };
        let model = a.model.as_deref().or(a.provider.default_model()).unwrap_or("no model");
        let _ = writeln!(out, "{mark} {}  {} · {model}", a.id, a.provider.name());
        let _ = writeln!(out, "    key: {key}");
    }
    Ok(out)
}

/// A serde enum value from its name ("openai_chat", "xhigh").
fn from_name<T: serde::de::DeserializeOwned>(what: &str, v: &str) -> Result<T, String> {
    serde_json::from_value(Value::String(v.to_string())).map_err(|_| format!("unknown {what} {v:?}"))
}

/// Applies `--name`, `--model`, `--effort`, `--max-tokens` and (custom
/// only) `--base-url` to `account`; an empty value clears the setting.
fn apply_fields(account: &mut Account, a: &Args) -> Result<(), Failed> {
    let some = |v: &str| (!v.trim().is_empty()).then(|| v.trim().to_string());
    if let Some(n) = a.get("--name") {
        account.name = some(n).unwrap_or_else(|| account.id.clone());
    }
    if let Some(m) = a.get("--model") {
        account.model = some(m);
    }
    if let Some(e) = a.get("--effort") {
        account.effort = match some(e) {
            Some(e) => Some(from_name::<Effort>("effort (low, medium, high, xhigh, max)", &e).map_err(usage)?),
            None => None,
        };
    }
    if let Some(n) = a.get("--max-tokens") {
        account.max_tokens = match some(n) {
            Some(n) => Some(n.parse().map_err(|_| usage(format!("--max-tokens must be a number, got {n:?}")))?),
            None => None,
        };
    }
    if let Some(url) = a.get("--base-url") {
        match &mut account.provider {
            Provider::Custom { base_url, .. } => *base_url = url.trim().to_string(),
            _ => return Err(usage("--base-url is for custom providers")),
        }
    }
    Ok(())
}

fn add_account(id: Option<&str>, a: &Args, notes: &mut Vec<String>) -> Result<String, Failed> {
    let (path, mut file) = load_accounts()?;
    let provider = match a.get("--provider") {
        None => return Err(usage("which provider? --provider anthropic|openai|openrouter|custom")),
        Some("custom") => {
            let base_url = a.get("--base-url").ok_or_else(|| usage("a custom provider needs --base-url <url>"))?;
            let protocol: Protocol = match a.get("--protocol") {
                Some(p) => from_name("protocol (anthropic, openai_chat)", p).map_err(usage)?,
                None => return Err(usage("a custom provider needs --protocol anthropic|openai_chat")),
            };
            Provider::Custom { protocol, base_url: base_url.to_string() }
        }
        Some(p) => {
            if a.get("--base-url").is_some() || a.get("--protocol").is_some() {
                return Err(usage("--base-url and --protocol are for --provider custom"));
            }
            Provider::from_id(p)
                .ok_or_else(|| usage(format!("unknown provider {p:?} (anthropic, openai, openrouter, custom)")))?
        }
    };
    let id = match id {
        Some(id) => id.to_string(),
        None => file.unused_id(provider_id(&provider)),
    };
    if file.get(&id).is_some() {
        return Err(format!(
            "there is already an account \"{id}\" (change it with `accounts set`, or pick another id)"
        )
        .into());
    }
    let mut account = Account::new(&id, &id, provider);
    apply_fields(&mut account, a)?;
    if a.has("--key-stdin") {
        refuse_keyring_off(&account)?;
    }
    // Read the key before writing anything, so a failed read adds nothing.
    let key = if a.has("--key-stdin") { Some(read_key()?) } else { None };
    file.upsert(account).map_err(|e| e.to_string())?;
    if a.has("--default") {
        file.set_default(&id).map_err(|e| e.to_string())?;
    }
    config::save(&path, &file).map_err(|e| e.to_string())?;
    let account = file.get(&id).expect("just added");
    notes.extend(account.warnings());
    let key_note = match key {
        Some(k) => {
            let stored = keys().set(account, &k);
            drop(k);
            match stored {
                Ok(()) => "saved in the keyring".to_string(),
                Err(e) => return Err(format!("added account {id}, but its key wasn't saved: {e}").into()),
            }
        }
        None if account.provider.needs_key() => key_text(&keys().status(account)),
        None => "no key needed".into(),
    };
    if a.has("--json") {
        return Ok(pretty(&json!({"added": id, "default": file.default_account().map(|d| d.id == id)})));
    }
    Ok(format!("added account {id} ({}); key: {key_note}\n", account.provider.name()))
}

fn set_account(id: &str, a: &Args, notes: &mut Vec<String>) -> Result<String, Failed> {
    if !ACCOUNT_FIELDS.iter().any(|f| a.get(f).is_some()) {
        return Err(usage(format!("what to change? {}", ACCOUNT_FIELDS.join(", "))));
    }
    let (path, mut file) = load_accounts()?;
    let mut account = find(&file, id)?.clone();
    apply_fields(&mut account, a)?;
    file.upsert(account).map_err(|e| e.to_string())?;
    config::save(&path, &file).map_err(|e| e.to_string())?;
    let account = file.get(id).expect("just saved");
    notes.extend(account.warnings());
    if a.has("--json") {
        return Ok(pretty(&json!({
            "id": id,
            "name": account.name,
            "base_url": account.provider.base_url(),
            "model": account.model,
            "effort": account.effort,
            "max_tokens": account.max_tokens,
        })));
    }
    Ok(format!("changed account {id}\n"))
}

fn remove_account(id: &str, json: bool, notes: &mut Vec<String>) -> Result<String, Failed> {
    let (path, mut file) = load_accounts()?;
    let account = file.remove(id).ok_or_else(|| format!("there is no account \"{id}\""))?;
    config::save(&path, &file).map_err(|e| e.to_string())?;
    let key = if keyring_off() {
        format!("keyring not touched ({KEYRING_ENV}=off)")
    } else {
        match keys().delete(&account) {
            Ok(()) => "its key, if any, deleted from the keyring".to_string(),
            Err(e) => {
                notes.push(format!("the account is removed, but its key couldn't be deleted from the keyring: {e}"));
                "its key couldn't be deleted from the keyring".to_string()
            }
        }
    };
    Ok(if json {
        pretty(&json!({"removed": id, "keyring_touched": !keyring_off()}))
    } else {
        format!("removed account {id}; {key}\n")
    })
}

/// The key from stdin: one line (prompted on a terminal), trimmed. Only one
/// line is read, so a harness that leaves stdin open doesn't hang it.
fn read_key() -> Result<String, String> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprint!("Paste the API key, then Enter: ");
        let _ = std::io::stderr().flush();
    }
    let mut key = String::new();
    stdin.lock().read_line(&mut key).map_err(|e| format!("can't read the key from stdin: {e}"))?;
    let trimmed = key.trim().to_string();
    drop(key);
    if trimmed.is_empty() {
        return Err("no key on stdin (pipe it in: printf '%s\\n' \"$KEY\" | scriptorium accounts key set <id>)".into());
    }
    Ok(trimmed)
}

/// Lists the account's models: cheap (no tokens), sends the key but no
/// script text.
fn check_account(id: &str, json: bool, notes: &mut Vec<String>) -> Result<String, Failed> {
    let (_, file) = load_accounts()?;
    let account = find(&file, id)?;
    notes.extend(account.warnings());
    let key = keys().get(account).map(|(k, _)| k);
    let req = models_request(account, key.as_deref()).map_err(|e| e.to_string())?;
    drop(key);
    let transport = shell::transport_from_env(None).map_err(|e| e.to_string())?;
    let mut body = String::new();
    let sent = transport.send(&req, &CancelToken::new(), &mut |line| {
        body.push_str(line);
        body.push('\n');
        Flow::Continue
    });
    let finished = transport.finish();
    sent.map_err(|e| format!("{id}: {e}"))?;
    finished.map_err(|e| format!("{id}: {e}"))?;
    let models = parse_models(&body).map_err(|e| format!("{id}: {e}"))?;
    if json {
        return Ok(pretty(&json!({"id": id, "ok": true, "count": models.len(), "models": models})));
    }
    let mut out =
        format!("ok: {id} answered with {} {}\n", models.len(), if models.len() == 1 { "model" } else { "models" });
    for m in &models {
        let _ = writeln!(out, "  {m}");
    }
    Ok(out)
}
