//! `scriptorium-mcp`: Scriptorium as MCP tools, for Claude in Cowork, Claude
//! Desktop and Claude Code (shipped inside the Scriptorium plugin).
//!
//! Every tool runs the `scriptorium` CLI. That keeps one code path for agents
//! (AGENTS.md rule 5): the server can do exactly what the CLI can, works on the
//! same session file as the app, and needs no changes when the CLI grows a
//! feature beyond adding a tool here.
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

/// Shown to the model when it connects: how to use the tools well.
const INSTRUCTIONS: &str = "\
Scriptorium holds screenplays (Final Draft .fdx files) and reviews of them. A review is a list of \
suggested changes to lines; a person decides each one in the Scriptorium app (accept, keep their own \
line, or rewrite). The app and these tools share one saved session next to the script, so the person \
sees your work and you see theirs.

Prefer proposing over editing: use suggest_change to add suggestions to a review file and let the person \
decide in the app, unless they asked you to decide or edit directly. Find paragraphs with \
list_paragraphs (filter it: scripts have thousands of paragraphs) and refer to them by id. Every change \
made through decide_suggestion or edit_paragraph can be undone with undo. Scripts are never \
overwritten: write_script writes a new .fdx next to the original.";

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
            match tools::call(name, &args) {
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

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}
