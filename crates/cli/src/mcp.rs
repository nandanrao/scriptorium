//! `scriptorium mcp`: the agent connector. Scriptorium as MCP tools for
//! Claude Desktop, Claude Code, Codex and other agents, over stdio (one
//! JSON-RPC message per line; hand-written on serde_json, as the server only
//! needs a few methods).
//!
//! - `initialize`, `tools/list` ([`catalogue`]`(Offer::Agent)`),
//!   `prompts/list` and `prompts/get` ([`guidance`]) are answered here: the
//!   connector comes from the same build as the app.
//! - **Each `tools/call` picks its route** (plan "agent connect", B2): to the
//!   running app over the link ([`scriptorium_fsio::link`]) when it answers
//!   with the same version, which runs it on the live open script; otherwise
//!   on the script's files, through the same executor
//!   ([`run_call`] with [`Caller::Agent`] over [`FileScript`]).
//! - Not delivered (the app hung up before taking the call): reconnect once,
//!   then the files. The app gone after taking it: a read-only tool runs
//!   again on the files; a writing tool never runs twice, and says it may
//!   have been applied.
//! - `script` is optional: over the link the app uses its open script. On
//!   the files a call needs one.
//!
//! stdout carries only protocol; notes go to stderr. The connector needs no
//! working folder or PATH (agents start it with `/` and a minimal
//! environment); the socket's path comes from `$HOME`
//! ([`scriptorium_fsio::link::default_socket_path`]).

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use scriptorium_agent::ToolCall;
use scriptorium_chat::exec::{
    BUSY_FOR, BUSY_RETRY, Caller, FileScript, ReviewError, ReviewStore, Script, expand_home, run_call,
};
use scriptorium_tools::{Offer, catalogue, guidance, mcp_tool};
use serde_json::{Value, json};

/// Protocol versions this server speaks, newest first. The client's choice
/// is echoed back when it's listed here.
const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// The tools the app answers whatever script is open: never run on the
/// files while the link is up.
const APP_ONLY: [&str; 2] = ["guide", "where_is_writer"];

/// A call that needs a script, with none given and no app to use its open one.
pub const NO_APP: &str = "Scriptorium isn't open, so there's no open script to use. Ask the writer to open the script in Scriptorium, \
     or give its absolute path as `script`.";

/// A call with no script that the app sent to the files (nothing open there).
pub const NO_OPEN_SCRIPT: &str = "Scriptorium has no script open for this call. Ask the writer to open the script in Scriptorium, or give its \
     absolute path as `script`.";

/// A call that needs a script, with none given, when another version of the
/// app is open (its open script can't be used from here).
pub const OTHER_VERSION: &str = "Scriptorium is open but a different version (another copy of the app), so its open \
                                 script can't be used from here. Give the script's absolute path as `script`.";

/// `where_is_writer` when another version of the app is open.
pub const OTHER_VERSION_WHERE: &str = "Scriptorium is open but a different version (another copy of the app), so \
                                       this connection can't tell what is open or where the writer is. Ask the \
                                       writer for the script's path.";

/// A call that needs a script, with none given, when the app didn't answer.
pub const NO_ANSWER: &str = "Scriptorium didn't answer, so its open script can't be used. Try again, or give the \
                             script's absolute path as `script`.";

/// `where_is_writer` when the app didn't answer.
pub const NO_ANSWER_WHERE: &str = "Scriptorium didn't answer, so this connection can't tell what is open or where the writer is. Try again, or \
     ask the writer for the script's path.";

/// `where_is_writer` with no app: a plain answer, not an error.
pub const NOT_OPEN: &str = "Scriptorium isn't open, so nothing is open and there's no writer's position to tell. Ask the writer for the \
     script's path, or to open it in Scriptorium.";

/// A writing tool whose call the app took and then closed during.
pub const MAYBE_APPLIED: &str = "Scriptorium closed during this call; it may have been applied. Check with \
                                 list_suggestions before trying again.";

/// Serve MCP on stdin and stdout until stdin ends, routing calls to the app
/// at [`default_socket_path`](scriptorium_fsio::link) when it answers.
///
/// One message at a time: a `ping` or a cancel sent during a long call
/// waits behind it (fine for v1: the tools answer in seconds).
pub fn serve() {
    #[cfg(unix)]
    let socket = scriptorium_fsio::link::default_socket_path();
    #[cfg(not(unix))]
    let socket = None;
    let mut connector = Connector::new(socket);
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match stdin.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        // Not UTF-8: a parse error for that line, and on to the next.
        let reply = match std::str::from_utf8(&buf) {
            Ok(line) if line.trim().is_empty() => continue,
            Ok(line) => match serde_json::from_str::<Value>(line) {
                Ok(msg) => connector.handle(&msg),
                Err(e) => Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
            },
            Err(e) => Some(error(Value::Null, -32700, &format!("parse error: not UTF-8 ({e})"))),
        };
        if let Some(reply) = reply {
            // A failed write means the client is gone.
            if writeln!(stdout, "{reply}").and_then(|()| stdout.flush()).is_err() {
                break;
            }
        }
    }
}

/// The connector's state across messages: the link to the app (kept between
/// calls, reconnected when needed) and whether the agent was told about
/// another version of the app.
pub struct Connector {
    /// Where the app's socket is; `None`: never the app (no home folder, or
    /// a platform without the link).
    socket: Option<PathBuf>,
    #[cfg(unix)]
    client: Option<scriptorium_fsio::link::Client>,
    /// The mismatch notice was given (once per connector).
    told_mismatch: bool,
}

/// How a call ended: its text, and whether it is an error.
struct Ended {
    text: String,
    is_error: bool,
}

impl Ended {
    fn ok(text: impl Into<String>) -> Self {
        Ended { text: text.into(), is_error: false }
    }

    fn err(text: impl Into<String>) -> Self {
        Ended { text: text.into(), is_error: true }
    }
}

/// Why a call runs on the files: what the agent is told when it can't
/// (no script given, or `where_is_writer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoApp {
    /// The app isn't running (or went away).
    Absent,
    /// The app is there and sent the call to the files (nothing open, or
    /// another script).
    Routed,
    /// Another version of the app is open.
    OtherVersion,
    /// Something answered at the socket but not in time, or not properly.
    NoAnswer,
}

/// What the app made of a call.
enum ViaApp {
    /// The app ran it.
    Ran(Ended),
    /// Run it on the files.
    Files(NoApp),
    /// The app took it and then went away; `script`: the script it named.
    Lost { script: Option<String> },
}

impl Connector {
    /// A connector reaching the app at `socket`.
    pub fn new(socket: Option<PathBuf>) -> Self {
        Connector {
            socket,
            #[cfg(unix)]
            client: None,
            told_mismatch: false,
        }
    }

    /// The reply to one message, or `None` for notifications (no `id`).
    pub fn handle(&mut self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned()?;
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str);
                let version = asked.filter(|v| PROTOCOL_VERSIONS.contains(v)).unwrap_or(PROTOCOL_VERSIONS[0]);
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {}, "prompts": {} },
                    "serverInfo": { "name": "scriptorium", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": guidance::INSTRUCTIONS,
                })
            }
            "ping" => json!({}),
            "tools/list" => json!({ "tools": catalogue(Offer::Agent).iter().map(mcp_tool).collect::<Vec<_>>() }),
            "prompts/list" => json!({ "prompts": guidance::prompts() }),
            "prompts/get" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                match guidance::prompt(name, &args) {
                    Ok(p) => p,
                    Err(e) => return Some(error(id, -32602, &e.to_string())),
                }
            }
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                if !offered(name) {
                    return Some(error(id, -32602, &format!("unknown tool {name:?}")));
                }
                let ended = guarded(|| self.call(name, &args));
                let mut result = json!({ "content": [{ "type": "text", "text": ended.text }] });
                // Tool failures go back to the model as results it can read and act on.
                if ended.is_error {
                    result["isError"] = json!(true);
                }
                result
            }
            _ => return Some(error(id, -32601, &format!("method not found: {method}"))),
        };
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    /// Run tool `name`: in the app if it takes it, otherwise on the files.
    fn call(&mut self, name: &str, args: &Value) -> Ended {
        let mut notes = vec![];
        let via = self.via_app(name, args, &mut notes);
        let ended = match via {
            ViaApp::Ran(ended) => ended,
            ViaApp::Files(why) => self.on_files(name, args, why, &mut notes),
            ViaApp::Lost { .. } if scriptorium_tools::writes(name) => Ended::err(MAYBE_APPLIED),
            ViaApp::Lost { script } => {
                // A read: run it again on the files, on the script the app named.
                let mut args = args.clone();
                if let (Some(s), Some(map)) = (script, args.as_object_mut()) {
                    map.insert("script".into(), Value::String(s));
                }
                self.on_files(name, &args, NoApp::Absent, &mut notes)
            }
        };
        finish(ended, &notes)
    }

    /// Offer the call to the app, if it is there.
    #[cfg(unix)]
    fn via_app(&mut self, name: &str, args: &Value, notes: &mut Vec<String>) -> ViaApp {
        use scriptorium_fsio::link::proto::Call;
        use scriptorium_fsio::link::{Answer, Gone};

        let call = Call { name: name.into(), arguments: for_the_app(args) };
        // Twice at most: a call not delivered goes once more, on a new link.
        for _ in 0..2 {
            let client = match self.link(notes) {
                Ok(c) => c,
                Err(why) => return ViaApp::Files(why),
            };
            match client.call(&call) {
                Ok(Answer::Done { done, .. }) => {
                    return ViaApp::Ran(Ended { text: done.text, is_error: done.is_error });
                }
                Ok(Answer::Files) => return ViaApp::Files(NoApp::Routed),
                Err(Gone::NotDelivered) => {
                    eprintln!("note: Scriptorium closed before taking {name}; connecting again");
                    self.client = None;
                }
                Err(Gone::AfterTaken { script }) => {
                    eprintln!("note: Scriptorium closed during {name}");
                    self.client = None;
                    return ViaApp::Lost { script };
                }
            }
        }
        ViaApp::Files(NoApp::Absent)
    }

    #[cfg(not(unix))]
    fn via_app(&mut self, _name: &str, _args: &Value, _notes: &mut Vec<String>) -> ViaApp {
        ViaApp::Files(NoApp::Absent)
    }

    /// The link to the app: the one kept, or a new one (connect and hello
    /// within [`HELLO_TIMEOUT`](scriptorium_fsio::link::HELLO_TIMEOUT)).
    /// Another version of the app is told to the agent once, in `notes`.
    /// No link: why.
    #[cfg(unix)]
    fn link(&mut self, notes: &mut Vec<String>) -> Result<&mut scriptorium_fsio::link::Client, NoApp> {
        use scriptorium_fsio::link::proto::{Hello, mismatch_notice};
        use scriptorium_fsio::link::{Client, HELLO_TIMEOUT, NoLink};

        if self.client.is_none() {
            let socket = self.socket.as_deref().ok_or(NoApp::Absent)?;
            let exe = std::env::current_exe().ok().map(|p| p.display().to_string());
            let ours = Hello { exe, ..Hello::this_build() };
            match Client::connect(socket, &ours, HELLO_TIMEOUT) {
                Ok(c) => self.client = Some(c),
                Err(NoLink::Absent) => return Err(NoApp::Absent),
                Err(NoLink::Mismatch(theirs)) => {
                    if !self.told_mismatch {
                        self.told_mismatch = true;
                        let notice = mismatch_notice(&ours, &theirs);
                        eprintln!("note: {notice}");
                        notes.push(notice);
                    }
                    return Err(NoApp::OtherVersion);
                }
                Err(NoLink::Failed(why)) => {
                    eprintln!("note: not connected to Scriptorium ({why}); working on the files");
                    return Err(NoApp::NoAnswer);
                }
            }
        }
        self.client.as_mut().ok_or(NoApp::Absent)
    }

    /// Run the call on the script's files, waiting for the session lock and
    /// retrying while busy. `why`: why not in the app, for what the
    /// app-only tools and a missing script say.
    fn on_files(&mut self, name: &str, args: &Value, why: NoApp, notes: &mut Vec<String>) -> Ended {
        let call = ToolCall {
            id: "mcp".into(),
            name: name.into(),
            input: args.clone(),
            input_error: None,
            raw_input: None,
            extra: None,
        };
        if why == NoApp::Routed && APP_ONLY.contains(&name) {
            // The app answers these itself; it sending one here is its bug.
            return Ended::err(format!("Scriptorium didn't answer {name}; try again."));
        }
        if name == "where_is_writer" {
            return Ended::ok(match why {
                NoApp::OtherVersion => OTHER_VERSION_WHERE,
                NoApp::NoAnswer => NO_ANSWER_WHERE,
                NoApp::Absent | NoApp::Routed => NOT_OPEN,
            });
        }
        if name == "guide" {
            let done = run_call(&mut NoScript, Caller::Agent, &call);
            return Ended { text: done.result.content, is_error: done.result.is_error };
        }
        let script = match args.get("script") {
            Some(Value::String(s)) => s,
            // Not a string: the arguments' check says so.
            Some(v) if !v.is_null() => {
                let done = run_call(&mut NoScript, Caller::Agent, &call);
                return Ended { text: done.result.content, is_error: done.result.is_error };
            }
            _ => {
                return Ended::err(match why {
                    NoApp::Absent => NO_APP,
                    NoApp::Routed => NO_OPEN_SCRIPT,
                    NoApp::OtherVersion => OTHER_VERSION,
                    NoApp::NoAnswer => NO_ANSWER,
                });
            }
        };
        let mut files = match FileScript::new(Path::new(&expand_home(script))) {
            Ok(f) => f.with_lock_wait(scriptorium_fsio::LOCK_WAIT),
            Err(e) => return Ended::err(e),
        };
        let start = Instant::now();
        let done = loop {
            let done = run_call(&mut files, Caller::Agent, &call);
            if !done.busy || start.elapsed() >= BUSY_FOR {
                break done;
            }
            std::thread::sleep(BUSY_RETRY);
        };
        notes.extend(files.take_notes());
        Ended { text: done.result.content, is_error: done.result.is_error }
    }
}

/// Whether `tools/call` takes tool `name`: the Agent offer, and
/// `where_is_writer` (the app's; answered plainly without it).
fn offered(name: &str) -> bool {
    name == "where_is_writer" || catalogue(Offer::Agent).iter().any(|s| s.name == name)
}

/// The arguments as the app gets them: `script` with `~` expanded and made
/// absolute from the connector's folder (the app's folder is another), so
/// the app can tell whether it names the open script.
fn for_the_app(args: &Value) -> Value {
    let mut args = args.clone();
    if let Some(Value::String(s)) = args.get("script") {
        let path = PathBuf::from(expand_home(s));
        let path =
            if path.is_absolute() { path } else { std::env::current_dir().map(|d| d.join(&path)).unwrap_or(path) };
        if let Some(p) = path.to_str() {
            args["script"] = Value::String(p.to_string());
        }
    }
    args
}

/// The tool result: the output, or the error, with the notes (an attached
/// review that can't be read, another version of the app) after it; they
/// matter to the model too. A note the error already says isn't said again.
fn finish(ended: Ended, notes: &[String]) -> Ended {
    let text = ended.text.trim_end().to_string();
    let mut said: Vec<String> = vec![];
    for n in notes {
        let line = format!("note: {n}");
        if (ended.is_error && !n.is_empty() && text.contains(n.as_str())) || said.contains(&line) {
            continue;
        }
        said.push(line);
    }
    let text = if said.is_empty() { text } else { format!("{text}\n\n{}", said.join("\n")) };
    Ended { text, is_error: ended.is_error }
}

/// The script of a call that opens none (`guide`, or arguments the check
/// refuses first): never opened.
struct NoScript;

impl Script for NoScript {
    fn open(&mut self, _writes: bool) -> Result<Box<dyn ReviewStore + '_>, ReviewError> {
        Err(ReviewError::Failed(NO_APP.into()))
    }
}

/// Runs one tool call, turning a panic into a tool error, so a bug in one
/// call never takes the server (and every later call) down. The panic's
/// message also goes to stderr through the default hook.
fn guarded(f: impl FnOnce() -> Ended) -> Ended {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|panic| {
        let what = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic".into());
        Ended::err(format!("internal error: {what}"))
    })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panicking_call_is_a_tool_error() {
        let r = guarded(|| panic!("boom {}", 1));
        assert!(r.is_error && r.text == "internal error: boom 1");
        let r = guarded(|| std::panic::panic_any(7_u8));
        assert!(r.is_error && r.text == "internal error: a panic");
        let r = guarded(|| Ended::ok("fine"));
        assert!(!r.is_error && r.text == "fine");
    }

    #[test]
    fn notes_follow_the_result_once() {
        let notes = vec!["a.review.txt can't be read".to_string(), "a.review.txt can't be read".to_string()];
        let ok = finish(Ended::ok("done\n"), &notes);
        assert_eq!(ok.text, "done\n\nnote: a.review.txt can't be read");
        let said = finish(Ended::err("failed: a.review.txt can't be read"), &notes);
        assert!(said.is_error && said.text == "failed: a.review.txt can't be read");
    }

    #[test]
    fn no_app_no_script() {
        let mut c = Connector::new(None);
        let r = c.call("guide", &json!({ "topic": "review" }));
        assert!(!r.is_error && !r.text.is_empty());
        let r = c.call("read_scene", &json!({ "scene": 1 }));
        assert!(r.is_error && r.text == NO_APP, "{}", r.text);
        let r = c.call("where_is_writer", &json!({}));
        assert!(!r.is_error && r.text == NOT_OPEN, "{}", r.text);
        // A script that isn't a string: the arguments' check says so.
        let r = c.call("read_scene", &json!({ "script": 3, "scene": 1 }));
        assert!(r.is_error && r.text.contains("script"), "{}", r.text);
        assert!(!offered("no_such_tool") && offered("where_is_writer") && offered("guide"));
    }

    #[test]
    fn the_app_gets_an_absolute_script() {
        let args = for_the_app(&json!({ "script": "film.fdx", "scene": 1 }));
        let s = args["script"].as_str().unwrap();
        assert!(Path::new(s).is_absolute() && s.ends_with("film.fdx"), "{s}");
        assert_eq!(args["scene"], 1);
        assert_eq!(for_the_app(&json!({ "scene": 1 })), json!({ "scene": 1 }));
        assert_eq!(for_the_app(&json!({ "script": "/a/b.fdx" }))["script"], "/a/b.fdx");
    }
}
