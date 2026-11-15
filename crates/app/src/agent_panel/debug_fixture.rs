//! Debug builds only: `SCRIPTORIUM_DEBUG_THREAD=file.json` loads a fixed
//! conversation into the message view, for headless screenshots.
//!
//! The file is a `Thread`, or an object `{"thread": Thread, "stream": [...],
//! "error": "...", "active": bool, "review_id": "...", "review_name": "...",
//! "open": bool}`. Each `stream` entry is `{"text": ".."}`, `{"thinking":
//! ".."}` or `{"tool": {"id": "..", "name": ".."}}`; `open` expands every
//! tool and reasoning row.

use scriptorium_agent::{AgentError, Event, Thread};
use serde_json::Value;

use super::messages::ReviewRef;

pub struct Fixture {
    pub thread: Thread,
    pub events: Vec<Event>,
    pub error: Option<String>,
    pub active: bool,
    pub review: Option<ReviewRef>,
    pub open: bool,
}

impl Fixture {
    pub fn error(&self) -> Option<AgentError> {
        self.error.clone().map(AgentError::Parse)
    }
}

pub fn load() -> Option<Fixture> {
    let path = std::env::var_os("SCRIPTORIUM_DEBUG_THREAD")?;
    let read = || -> Result<Fixture, String> {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let wrapped = v.get("thread").is_some();
        let thread =
            serde_json::from_value(v.get("thread").cloned().unwrap_or_else(|| v.clone())).map_err(|e| e.to_string())?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let mut events = Vec::new();
        for (i, e) in v.get("stream").and_then(Value::as_array).into_iter().flatten().enumerate() {
            let text = |k: &str| e.get(k).and_then(Value::as_str).map(str::to_string);
            if let Some(text) = text("text") {
                events.push(Event::Text { index: i, text });
            } else if let Some(text) = text("thinking") {
                events.push(Event::Thinking { index: i, text });
            } else if let Some(t) = e.get("tool") {
                let f = |k: &str| t.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                events.push(Event::ToolCallStarted { id: f("id"), name: f("name") });
            }
        }
        Ok(Fixture {
            thread,
            events,
            error: s("error"),
            active: wrapped && v.get("active").and_then(Value::as_bool) == Some(true),
            review: s("review_id").map(|id| ReviewRef { id, name: s("review_name").unwrap_or_default() }),
            open: v.get("open").and_then(Value::as_bool) == Some(true),
        })
    };
    match read() {
        Ok(f) => Some(f),
        Err(e) => {
            crate::log::warn(&format!("SCRIPTORIUM_DEBUG_THREAD: {e}"));
            None
        }
    }
}
