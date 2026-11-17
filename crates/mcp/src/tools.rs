//! The tools: [`scriptorium_tools::catalogue`] for outside agents
//! ([`Offer::Agent`]), each call run by the one executor
//! ([`scriptorium_chat::exec::run_call`] with [`Caller::Agent`]) over the
//! script's files ([`FileScript`]), as the CLI `tool` runs it.
//!
//! Paths: `script` is taken as given (relative to the server's folder, which
//! in Cowork and Claude Desktop isn't the script's, so the schema asks for an
//! absolute path), with a leading `~` as the home folder. Every other path is
//! relative to the script's folder and confined to it by the executor
//! (AGENTS.md rule 6).

use std::path::Path;

use scriptorium_agent::ToolCall;
use scriptorium_chat::exec::{Caller, FileScript, ReviewError, ReviewStore, Script, expand_home, run_call};
use scriptorium_tools::{Offer, catalogue, mcp_tool};
use serde_json::Value;

pub enum Failure {
    UnknownTool,
    /// The command ran and failed, or the arguments were wrong: told to the model.
    Tool(String),
}

/// `tools/list`: every tool outside agents are offered, with annotations.
pub fn list() -> Vec<Value> {
    catalogue(Offer::Agent).iter().map(mcp_tool).collect()
}

/// Runs tool `name` on the files of the script `args` names, waiting for
/// the session lock up to [`scriptorium_fsio::LOCK_WAIT`]. The result is
/// followed by any notes opening the script turned up.
pub fn call(name: &str, args: &Value) -> Result<String, Failure> {
    if !catalogue(Offer::Agent).iter().any(|s| s.name == name) {
        return Err(Failure::UnknownTool);
    }
    let call = ToolCall {
        id: "mcp".into(),
        name: name.into(),
        input: args.clone(),
        input_error: None,
        raw_input: None,
        extra: None,
    };
    match args.get("script").and_then(Value::as_str) {
        Some(script) => {
            let mut files = FileScript::new(Path::new(&expand_home(script)))
                .map_err(Failure::Tool)?
                .with_lock_wait(scriptorium_fsio::LOCK_WAIT);
            let done = run_call(&mut files, Caller::Agent, &call);
            finish(done.result.content, done.result.is_error, &files.take_notes())
        }
        // No script: `guide` needs none, and any other tool is refused by
        // its arguments' check before anything is opened.
        None => {
            let done = run_call(&mut NoScript, Caller::Agent, &call);
            finish(done.result.content, done.result.is_error, &[])
        }
    }
}

/// The tool result: the output, or the error, with the notes (e.g. an
/// attached review that can't be read) after it; they matter to the model
/// too. A note the error already says isn't said again.
fn finish(text: String, is_error: bool, notes: &[String]) -> Result<String, Failure> {
    let text = text.trim_end().to_string();
    let notes: Vec<String> = notes
        .iter()
        .filter(|n| !(is_error && !n.is_empty() && text.contains(n.as_str())))
        .map(|n| format!("note: {n}"))
        .collect();
    let text = if notes.is_empty() { text } else { format!("{text}\n\n{}", notes.join("\n")) };
    if is_error { Err(Failure::Tool(text)) } else { Ok(text) }
}

/// The script of a call that names none: never opened.
struct NoScript;

impl Script for NoScript {
    fn open(&mut self, _writes: bool) -> Result<Box<dyn ReviewStore + '_>, ReviewError> {
        Err(ReviewError::Failed("missing `script`: the screenplay's absolute path".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_name_only_real_tools() {
        let specs = catalogue(Offer::Agent);
        // Snake-case words that aren't tools must be parameters some tool takes.
        let unknown = scriptorium_tools::prompt::unknown_tools(crate::INSTRUCTIONS, &specs);
        let params: Vec<String> = list()
            .iter()
            .flat_map(|t| t["inputSchema"]["properties"].as_object().unwrap().keys().cloned().collect::<Vec<_>>())
            .collect();
        let neither: Vec<&String> = unknown.iter().filter(|w| !params.contains(w)).collect();
        assert!(neither.is_empty(), "{neither:?}");
    }

    #[test]
    fn notes_follow_the_result_once() {
        let notes = vec!["a.review.txt can't be read".to_string()];
        let ok = finish("done\n".into(), false, &notes);
        assert!(matches!(ok, Ok(t) if t == "done\n\nnote: a.review.txt can't be read"));
        let said = finish("failed: a.review.txt can't be read".into(), true, &notes);
        assert!(matches!(said, Err(Failure::Tool(t)) if t == "failed: a.review.txt can't be read"));
    }

    #[test]
    fn guide_needs_no_script() {
        let r = call("guide", &serde_json::json!({ "topic": "review" }));
        assert!(matches!(r, Ok(t) if !t.is_empty()));
        let r = call("read_scene", &serde_json::json!({ "scene": 1 }));
        assert!(matches!(r, Err(Failure::Tool(t)) if t.contains("script")));
        assert!(matches!(call("no_such_tool", &serde_json::json!({})), Err(Failure::UnknownTool)));
    }
}
