//! `scriptorium chat`, `threads` and `accounts`: the built-in agent from the
//! command line, on the same files as the app's agent panel.
//!
//! - `chat` runs one turn of a thread through [`host::run`], the one run loop
//!   the app also uses, with the script opened from its files
//!   ([`Direct`]`(`[`FileScript`]`)`). The model's text streams to stdout;
//!   tool rows (the panel's labels), notices and the closing usage line go
//!   to stderr. Ctrl+C stops the run through its [`CancelToken`]: the thread
//!   is saved at every step, so `--retry` carries on from there.
//! - `threads` lists, shows, re-models and deletes a script's threads
//!   (`<script>.threads/`). Changing a thread takes its lock, waiting
//!   [`CLI_WAIT`] before refusing a thread another program is running.
//! - `accounts` edits the per-user accounts file and keeps API keys in the
//!   OS keyring. Keys are read from stdin, never from the command line, and
//!   never printed.
//!
//! With `--json`, each prints one JSON value on stdout for programs; errors
//! are still the `error:` line on stderr and a failing exit status.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::Path;

use scriptorium_agent::file::ThreadFile;
use scriptorium_agent::protocol::parse_models;
use scriptorium_agent::turn::Outcome;
use scriptorium_agent::{Block, Effort, Event, Protocol, Role, ToolCall, ToolResult};
use scriptorium_chat::exec::{Added, CallDone, Direct, FileScript};
use scriptorium_chat::host::{self, Env, HostError, Request, Ui, Which};
use scriptorium_chat::labels;
use scriptorium_chat::lock::CLI_WAIT;
use scriptorium_chat::model::{Entry, parse_rfc3339};
use scriptorium_chat::seen::{self, Seen};
use scriptorium_chat::store::ThreadStore;
use scriptorium_chat::{clock, naming};
use scriptorium_fsio::opened::Opened;
use scriptorium_providers::shell::{self, CancelToken, Flow, KeyStatus, Keys, REPLAY_ENV, config};
use scriptorium_providers::{Account, AccountsFile, PriceBook, Provider, models_request, thread_cost};
use serde_json::{Value, json};

/// The commands this module handles, dispatched by [`crate::run`].
pub const COMMANDS: &[&str] = &["chat", "threads", "accounts"];

/// Set to `off` to keep keys out of the OS keyring: only the
/// `SCRIPTORIUM_<ACCOUNT>_KEY` / `SCRIPTORIUM_<PROVIDER>_KEY` variables are
/// read, and nothing on the machine's keyring is read or changed (CI, a
/// session without a keyring, tests).
pub const KEYRING_ENV: &str = "SCRIPTORIUM_KEYRING";

pub fn command(cmd: &str, args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    match cmd {
        "chat" => chat(args, notes),
        "threads" => threads(args, notes),
        "accounts" => accounts(args, notes),
        _ => Err(format!("unknown command {cmd:?}")),
    }
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
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).expect("JSON values serialize") + "\n"
}

/// The keys: the OS keyring and the environment, or the environment alone
/// with `SCRIPTORIUM_KEYRING=off`.
fn keys() -> Keys {
    match std::env::var(KEYRING_ENV) {
        Ok(v) if v.trim().eq_ignore_ascii_case("off") => Keys::env_only(),
        _ => Keys::system(),
    }
}

fn accounts_path() -> Result<std::path::PathBuf, String> {
    config::default_path().ok_or_else(|| "no home folder to keep the accounts file in (set HOME)".to_string())
}

fn load_accounts(notes: &mut Vec<String>) -> Result<(std::path::PathBuf, AccountsFile), String> {
    let path = accounts_path()?;
    let file = config::load(&path).map_err(|e| e.to_string())?;
    notes.extend(file.warnings());
    Ok((path, file))
}

// ---------------------------------------------------------------------------
// chat

fn chat(args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let a = Args::parse(
        args,
        &["--new", "--yes", "--allow-whole-script", "--retry", "--json"],
        &["--thread", "--account", "--model", "--record"],
    )?;
    let (script, message) = match (a.positional.as_slice(), a.has("--retry")) {
        ([script], true) => (*script, None),
        ([_, _], true) => return Err("--retry sends the thread again as it is: leave out the message".into()),
        ([script, message], false) => (*script, Some(*message)),
        ([_], false) => return Err("what's the message? (or --retry to send the thread again)".into()),
        ([], _) => return Err(format!("wrong arguments for `chat`\n\n{}", crate::USAGE)),
        _ => return Err("put the message in quotes: one argument".into()),
    };
    let which = match (a.get("--thread"), a.has("--new")) {
        (Some(_), true) => return Err("--thread and --new: choose one".into()),
        (Some(id), false) => Which::Thread(id.to_string()),
        (None, _) if a.has("--retry") => return Err("--retry needs --thread <id>".into()),
        (None, _) => Which::New,
    };
    let json = a.has("--json");
    let script_path = Path::new(script);

    let (_, accounts) = load_accounts(notes)?;
    let keys = keys();
    let seen_path = seen::default_path().ok_or("no home folder to remember disclosures in (set HOME)")?;
    let mut seen = Seen::load(&seen_path);
    let mut clocks = clock::from_env()?;
    if let Some(cassette) = std::env::var_os(REPLAY_ENV).filter(|v| !v.is_empty()) {
        eprintln!(
            "note: replaying {} ({REPLAY_ENV} is set): nothing is sent to a provider",
            Path::new(&cassette).display()
        );
    }
    let transport = shell::transport_from_env(a.get("--record").map(Path::new)).map_err(|e| e.to_string())?;
    let mut tools = Direct(FileScript::new(script_path)?);

    let mut req = Request::new(script_path, which, message);
    req.account = a.get("--account");
    req.model = a.get("--model");
    req.allow_whole_script = a.has("--allow-whole-script");

    let cancel = CancelToken::new();
    stop_on_ctrl_c(&cancel);
    let mut ui = TermUi { yes: a.has("--yes"), json, ..TermUi::default() };
    let env = Env { accounts: &accounts, keys: &keys, seen: &mut seen, clocks: &mut clocks };
    let result = host::run(&*transport, &mut tools, &req, env, &mut ui, &cancel);
    ui.line_break();

    let done = match result {
        Ok(done) => done,
        Err(HostError::Declined) if ui.not_asked => {
            return Err("nothing was sent: to accept the disclosure above, run again with --yes".into());
        }
        Err(e) => return Err(e.to_string()),
    };
    let file = &done.thread;
    let id = &file.id;
    let again = format!("scriptorium chat {script:?} --thread {id} --retry");
    let usage = usage_line(&accounts, file);
    match &done.outcome {
        Outcome::Finished => {}
        Outcome::Stopped => return Err(format!("stopped; thread {id} is saved (carry on: {again})")),
        Outcome::Withdrawn(text) => {
            return Err(format!(
                "the model declined; your message was taken out of thread {id}, rephrase it and send again: {text}"
            ));
        }
        Outcome::Error(e) => return Err(format!("{e} (thread {id} is saved; try again: {again})")),
        Outcome::RoundLimit => {
            return Err(format!(
                "stopped: the agent reached its limit of tool rounds in thread {id} (carry on: {again})"
            ));
        }
    }
    if json {
        let added: Vec<Value> =
            ui.added.iter().map(|a| json!({"review_id": a.review_id, "numbers": a.numbers})).collect();
        return Ok(pretty(&json!({
            "thread": id,
            "title": file.title,
            "created": done.created,
            "account": file.account,
            "model": file.model,
            "review_path": file.review_path,
            "added": added,
            "text": reply_text(file),
            "usage": usage,
        })));
    }
    eprintln!("thread {id} ({}) · {usage}", file.title);
    if let (false, Some(path)) = (ui.added.is_empty(), &file.review_path) {
        eprintln!("suggestions are in {path}");
    }
    Ok(String::new())
}

/// Ctrl+C cancels the run (the thread was saved at its last step); a second
/// Ctrl+C quits at once.
fn stop_on_ctrl_c(cancel: &CancelToken) {
    let cancel = cancel.clone();
    // Only one handler per process; a second `chat` in one process (tests
    // calling `run`) keeps the first, harmlessly.
    let _ = ctrlc::try_set_handler(move || {
        if cancel.is_cancelled() {
            std::process::exit(130);
        }
        eprintln!("\nstopping… (Ctrl+C again to quit at once)");
        cancel.cancel();
    });
}

/// The assistant's text after the last message the writer sent: what this
/// run answered.
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

/// "130 tokens · $0.01" for the whole thread.
fn usage_line(accounts: &AccountsFile, file: &ThreadFile) -> String {
    let cost = thread_cost(&PriceBook::from_accounts(accounts), &file.thread);
    labels::usage_text(&file.thread.usage(), cost.complete.then_some(cost.usd))
}

/// The terminal side of a run: streamed text on stdout, everything else on
/// stderr, and the two questions asked on a terminal (or answered by flags).
#[derive(Default)]
struct TermUi {
    /// `--yes`: accept the disclosure without asking.
    yes: bool,
    /// `--json`: nothing streams to stdout.
    json: bool,
    /// Stdout is in the middle of a line of streamed text.
    mid_line: bool,
    /// The disclosure was refused because there was nobody to ask.
    not_asked: bool,
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

    /// Asks `question` on the terminal; `None` when stdin isn't one.
    fn ask(&mut self, question: &str) -> Option<bool> {
        if !std::io::stdin().is_terminal() {
            return None;
        }
        eprint!("{question} [y/N] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer).ok()?;
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

fn threads(args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let a = Args::parse(args, &["--json"], &[])?;
    let json = a.has("--json");
    match a.positional.as_slice() {
        [script] => list_threads(script, json, notes),
        [script, "show", id] => show_thread(script, id, json, notes),
        [script, "delete", id] => delete_thread(script, id, json),
        [script, "set-model", id, account, model] => set_model(script, id, account, model, json, notes),
        _ => Err(format!("wrong arguments for `threads`\n\n{}", crate::USAGE)),
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

fn list_threads(script: &str, json: bool, notes: &mut Vec<String>) -> Result<String, String> {
    let store = ThreadStore::for_script(Path::new(script));
    let listed = store.list().map_err(|e| e.to_string())?;
    let opened = if listed.is_empty() { None } else { open_session(script, notes) };
    let mut rows: Vec<(Entry, Option<&ThreadFile>, Option<ReviewState>)> = listed
        .iter()
        .map(|l| {
            let file = l.file.as_ref().ok();
            let state = file.and_then(|f| review_state(script, f, opened.as_ref()));
            let total = state.as_ref().and_then(|s| s.counts.as_ref()).map(|p| p.total);
            (Entry::from_listed(l, total), file, state)
        })
        .collect();
    // Newest first, as the panel lists them.
    rows.sort_by(|a, b| b.0.updated.cmp(&a.0.updated).then(a.0.id.cmp(&b.0.id)));
    if json {
        let out: Vec<Value> = rows
            .iter()
            .map(|(e, f, s)| {
                json!({
                    "id": e.id,
                    "title": e.title,
                    "account": f.map(|f| f.account.as_str()),
                    "model": f.map(|f| f.model.as_str()),
                    "updated": f.map(|f| f.updated.as_str()),
                    "review_path": s.as_ref().map(|s| s.path.as_str()),
                    "suggestions": s.as_ref().and_then(|s| s.counts.as_ref()).map(counts_json),
                    "problem": e.problem.as_ref().map(|p| p.reason()),
                })
            })
            .collect();
        return Ok(pretty(&Value::Array(out)));
    }
    if rows.is_empty() {
        return Ok(format!("no threads yet (start one: scriptorium chat {script:?} \"<message>\")\n"));
    }
    let now = now_secs()?;
    let mut out = String::new();
    let width = rows.iter().map(|r| r.0.id.len()).max().unwrap_or(0);
    for (e, _, _) in &rows {
        let _ = writeln!(out, "{:<width$}  {}", e.id, e.title);
        let _ = writeln!(out, "{:<width$}  {}", "", e.detail(now));
    }
    Ok(out)
}

fn show_thread(script: &str, id: &str, json: bool, notes: &mut Vec<String>) -> Result<String, String> {
    let store = ThreadStore::for_script(Path::new(script));
    let file = store.read(id).map_err(|e| e.to_string())?;
    let opened = file.review_path.as_ref().and_then(|_| open_session(script, notes));
    let state = review_state(script, &file, opened.as_ref());
    let accounts = load_accounts(notes).map(|(_, a)| a).unwrap_or_default();
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
            "usage": usage,
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

fn delete_thread(script: &str, id: &str, json: bool) -> Result<String, String> {
    let store = ThreadStore::for_script(Path::new(script));
    let (lock, file) = store.lock_and_read(id, CLI_WAIT).map_err(|e| e.to_string())?;
    store.delete(&lock).map_err(|e| e.to_string())?;
    if json {
        return Ok(pretty(&json!({"deleted": id, "review_path": file.review_path})));
    }
    Ok(match &file.review_path {
        Some(p) => format!("deleted thread {id}; its review {p} stays, with its decisions\n"),
        None => format!("deleted thread {id}\n"),
    })
}

fn set_model(
    script: &str,
    id: &str,
    account: &str,
    model: &str,
    json: bool,
    notes: &mut Vec<String>,
) -> Result<String, String> {
    let (_, accounts) = load_accounts(notes)?;
    let resolved = accounts.resolve(account, Some(model)).map_err(|e| e.to_string())?;
    let store = ThreadStore::for_script(Path::new(script));
    let (lock, mut file) = store.lock_and_read(id, CLI_WAIT).map_err(|e| e.to_string())?;
    file.account = resolved.account.clone();
    file.model = resolved.model.clone();
    file.updated = clock::from_env()?.clock.now();
    store.write(&lock, &file).map_err(|e| e.to_string())?;
    if json {
        return Ok(pretty(&json!({"id": id, "account": file.account, "model": file.model})));
    }
    Ok(format!("thread {id} runs on {} · {} from its next message\n", file.account, file.model))
}

// ---------------------------------------------------------------------------
// accounts

fn accounts(args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let a = Args::parse(
        args,
        &["--json", "--default", "--key-stdin"],
        &["--provider", "--name", "--model", "--base-url", "--protocol", "--effort", "--max-tokens"],
    )?;
    let json = a.has("--json");
    let only = |allowed: &[&str]| -> Result<(), String> {
        let extra: Vec<&str> =
            a.switches.iter().chain(a.values.keys()).copied().filter(|f| !allowed.contains(f)).collect();
        if extra.is_empty() { Ok(()) } else { Err(format!("unexpected {}", extra.join(", "))) }
    };
    match a.positional.as_slice() {
        [] | ["list"] => {
            only(&["--json"])?;
            list_accounts(json, notes)
        }
        ["add", id @ ..] if id.len() <= 1 => {
            only(&[
                "--json",
                "--default",
                "--key-stdin",
                "--provider",
                "--name",
                "--model",
                "--base-url",
                "--protocol",
                "--effort",
                "--max-tokens",
            ])?;
            add_account(id.first().copied(), &a, notes)
        }
        ["remove", id] => {
            only(&["--json"])?;
            remove_account(id, json, notes)
        }
        ["default", id] => {
            only(&["--json"])?;
            let (path, mut file) = load_accounts(notes)?;
            file.set_default(id).map_err(|e| e.to_string())?;
            config::save(&path, &file).map_err(|e| e.to_string())?;
            Ok(if json { pretty(&json!({"default": id})) } else { format!("new threads use {id}\n") })
        }
        ["key", "set", id] => {
            only(&[])?;
            let (_, file) = load_accounts(notes)?;
            let account = find(&file, id)?;
            set_key(account)?;
            Ok(format!("key for {id} saved in the keyring\n"))
        }
        ["key", "delete", id] => {
            only(&[])?;
            let (_, file) = load_accounts(notes)?;
            keys().delete(find(&file, id)?).map_err(|e| e.to_string())?;
            Ok(format!("key for {id} removed from the keyring\n"))
        }
        ["key", "status", id] => {
            only(&["--json"])?;
            let (_, file) = load_accounts(notes)?;
            let account = find(&file, id)?;
            let status = keys().status(account);
            Ok(if json {
                pretty(&json!({"id": id, "key": key_json(&status)}))
            } else {
                format!("{id}: {}\n", key_text(&status))
            })
        }
        ["check", id] => {
            only(&["--json"])?;
            check_account(id, json, notes)
        }
        _ => Err(format!("wrong arguments for `accounts`\n\n{}", crate::USAGE)),
    }
}

fn find<'a>(file: &'a AccountsFile, id: &str) -> Result<&'a Account, String> {
    file.get(id).ok_or_else(|| format!("there is no account \"{id}\" (see `scriptorium accounts list`)"))
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

fn list_accounts(json: bool, notes: &mut Vec<String>) -> Result<String, String> {
    let (_, file) = load_accounts(notes)?;
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
                    "model": a.model,
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
        let _ = writeln!(out, "{mark} {}  {} · {}", a.id, a.provider.name(), a.model.as_deref().unwrap_or("no model"));
        let _ = writeln!(out, "    key {key}");
    }
    Ok(out)
}

/// A serde enum value from its name ("openai_chat", "xhigh").
fn from_name<T: serde::de::DeserializeOwned>(what: &str, v: &str) -> Result<T, String> {
    serde_json::from_value(Value::String(v.to_string())).map_err(|_| format!("unknown {what} {v:?}"))
}

fn add_account(id: Option<&str>, a: &Args, notes: &mut Vec<String>) -> Result<String, String> {
    let (path, mut file) = load_accounts(notes)?;
    let provider = match a.get("--provider") {
        None => return Err("which provider? --provider anthropic|openai|openrouter|custom".into()),
        Some("custom") => {
            let base_url = a.get("--base-url").ok_or("a custom provider needs --base-url <url>")?;
            let protocol: Protocol = match a.get("--protocol") {
                Some(p) => from_name("protocol (anthropic, openai_chat)", p)?,
                None => return Err("a custom provider needs --protocol anthropic|openai_chat".into()),
            };
            Provider::Custom { protocol, base_url: base_url.to_string() }
        }
        Some(p) => {
            if a.get("--base-url").is_some() || a.get("--protocol").is_some() {
                return Err("--base-url and --protocol are for --provider custom".into());
            }
            Provider::from_id(p)
                .ok_or_else(|| format!("unknown provider {p:?} (anthropic, openai, openrouter, custom)"))?
        }
    };
    let id = match id {
        Some(id) => id.to_string(),
        None => file.unused_id(provider_id(&provider)),
    };
    if file.get(&id).is_some() {
        return Err(format!("there is already an account \"{id}\" (remove it first, or pick another id)"));
    }
    let mut account = Account::new(&id, a.get("--name").unwrap_or(&id), provider);
    if let Some(m) = a.get("--model") {
        account.model = Some(m.to_string());
    }
    if let Some(e) = a.get("--effort") {
        account.effort = Some(from_name::<Effort>("effort (low, medium, high, xhigh, max)", e)?);
    }
    if let Some(n) = a.get("--max-tokens") {
        account.max_tokens = Some(n.parse().map_err(|_| format!("--max-tokens must be a number, got {n:?}"))?);
    }
    // Read the key before writing anything, so a failed read adds nothing.
    let key = if a.has("--key-stdin") { Some(read_key()?) } else { None };
    file.upsert(account).map_err(|e| e.to_string())?;
    if a.has("--default") {
        file.set_default(&id).map_err(|e| e.to_string())?;
    }
    config::save(&path, &file).map_err(|e| e.to_string())?;
    notes.extend(file.get(&id).map(Account::warnings).unwrap_or_default());
    let account = file.get(&id).expect("just added");
    let key_note = match key {
        Some(k) => match keys().set(account, &k) {
            Ok(()) => "key saved in the keyring".to_string(),
            Err(e) => return Err(format!("added account {id}, but its key wasn't saved: {e}")),
        },
        None if account.provider.needs_key() => key_text(&keys().status(account)),
        None => "no key needed".into(),
    };
    if a.has("--json") {
        return Ok(pretty(&json!({"added": id, "default": file.default_account().map(|d| d.id == id)})));
    }
    Ok(format!("added account {id} ({}); key {key_note}\n", account.provider.name()))
}

fn remove_account(id: &str, json: bool, notes: &mut Vec<String>) -> Result<String, String> {
    let (path, mut file) = load_accounts(notes)?;
    let account = file.remove(id).ok_or_else(|| format!("there is no account \"{id}\""))?;
    config::save(&path, &file).map_err(|e| e.to_string())?;
    if let Err(e) = keys().delete(&account) {
        notes.push(format!("the account is removed, but its key couldn't be deleted from the keyring: {e}"));
    }
    Ok(if json { pretty(&json!({"removed": id})) } else { format!("removed account {id}\n") })
}

/// The key from stdin: the first line (prompted on a terminal), trimmed.
fn read_key() -> Result<String, String> {
    let stdin = std::io::stdin();
    let mut key = String::new();
    if stdin.is_terminal() {
        eprint!("Paste the API key, then Enter: ");
        let _ = std::io::stderr().flush();
        stdin.lock().read_line(&mut key).map_err(|e| format!("can't read the key: {e}"))?;
    } else {
        stdin.lock().read_to_string(&mut key).map_err(|e| format!("can't read the key from stdin: {e}"))?;
    }
    let key = key.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
    if key.is_empty() {
        return Err("no key on stdin (pipe it in: printf %s \"$KEY\" | scriptorium accounts key set <id>)".into());
    }
    Ok(key)
}

fn set_key(account: &Account) -> Result<(), String> {
    let key = read_key()?;
    keys().set(account, &key).map_err(|e| e.to_string())
}

/// Lists the account's models: cheap (no tokens), sends the key but no
/// script text.
fn check_account(id: &str, json: bool, notes: &mut Vec<String>) -> Result<String, String> {
    let (_, file) = load_accounts(notes)?;
    let account = find(&file, id)?;
    let key = keys().get(account).map(|(k, _)| k);
    let req = models_request(account, key.as_deref()).map_err(|e| e.to_string())?;
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
        return Ok(pretty(&json!({"id": id, "ok": true, "models": models})));
    }
    let mut out =
        format!("ok: {id} answered with {} {}\n", models.len(), if models.len() == 1 { "model" } else { "models" });
    for m in &models {
        let _ = writeln!(out, "  {m}");
    }
    Ok(out)
}
