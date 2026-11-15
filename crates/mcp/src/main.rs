//! `scriptorium-mcp`: Scriptorium as MCP tools, for Claude in Cowork, Claude
//! Desktop and Claude Code (shipped inside the Scriptorium plugin).
//!
//! Every tool runs in-process through the `scriptorium_cli` library, the same
//! code the `scriptorium` command line runs. That keeps one code path for
//! agents (AGENTS.md rule 5): the server can do exactly what the CLI can, works
//! on the same session file as the app, and shares its reading and suggesting
//! tools (`scriptorium_tools`) with the app's built-in agent.
//!
//! Transport: MCP over stdio, one JSON-RPC message per line. Hand-written on
//! serde_json because the server only needs `initialize`, `tools/list` and
//! `tools/call`, which isn't worth an async runtime.

mod tools;

use std::io::{BufRead, Write};

use serde_json::{Value, json};

/// Protocol versions this server speaks, newest first. It only uses tools, so
/// any of them works; the client's choice is echoed back when it's listed here.
const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// Shown to the model when it connects: what the tools are for, and the few
/// rules that matter in every session. How to run a pass is in the plugin's
/// skill; each tool's details are in its description.
const INSTRUCTIONS: &str = "\
Scriptorium reviews screenplays (Final Draft .fdx): read the script, propose changes (to a line, to lines that go \
together, new lines, or cutting lines) into a review file, and the writer decides each one in the Scriptorium app, which shares a \
saved session with these tools.
Propose, don't edit: use suggest_changes or suggest_change; decide or edit (decide_suggestion, edit_paragraph) \
only when the writer asks you to.
When the writer asks for feedback rather than changes, answer in chat instead of proposing.
Always give review_file when proposing, named for the pass (e.g. dialogue-pass.review.txt).
Pass the script as an absolute path; if you don't know where the script is, ask the writer for its path.";

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&msg),
            Err(e) => Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        if let Some(reply) = reply {
            // stdout carries only protocol messages; a failed write means the client is gone.
            if writeln!(stdout, "{reply}").and_then(|()| stdout.flush()).is_err() {
                break;
            }
        }
    }
}

/// The reply to one message, or `None` for notifications (no `id`).
fn handle(msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str);
            let version = asked.filter(|v| PROTOCOL_VERSIONS.contains(v)).unwrap_or(PROTOCOL_VERSIONS[0]);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "scriptorium", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools::list() }),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match guarded(|| tools::call(name, &args)) {
                Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
                // Tool failures go back to the model as results it can read and act on.
                Err(tools::Failure::Tool(text)) => {
                    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
                }
                Err(tools::Failure::UnknownTool) => return Some(error(id, -32602, &format!("unknown tool {name:?}"))),
            }
        }
        _ => return Some(error(id, -32601, &format!("method not found: {method}"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// Runs one tool call, turning a panic into a tool error, so a bug in one
/// call never takes the server (and every later call) down. The panic's
/// message also goes to stderr through the default hook.
fn guarded(f: impl FnOnce() -> Result<String, tools::Failure>) -> Result<String, tools::Failure> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|panic| {
        let what = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic".into());
        Err(tools::Failure::Tool(format!("internal error: {what}")))
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
        assert!(matches!(r, Err(tools::Failure::Tool(t)) if t == "internal error: boom 1"));
        let r = guarded(|| std::panic::panic_any(7_u8));
        assert!(matches!(r, Err(tools::Failure::Tool(t)) if t == "internal error: a panic"));
        assert!(matches!(guarded(|| Ok("fine".into())), Ok(t) if t == "fine"));
    }

    #[test]
    fn instructions_stay_short() {
        assert!(INSTRUCTIONS.lines().count() <= 7, "{INSTRUCTIONS}");
        for must in ["review_file", "feedback", "ask the writer for its path"] {
            assert!(INSTRUCTIONS.contains(must), "{must}");
        }
    }
}
