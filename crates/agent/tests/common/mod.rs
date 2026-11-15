//! Shared helpers: feed an SSE fixture to a parser the way the caller would, line by line.

#![allow(dead_code)]

use scriptorium_agent::{AgentError, Event, Protocol, ReplyParser, Turn};

/// Feeds every line (line endings included) and finishes. Stops at the first error.
pub fn run(protocol: Protocol, body: &str) -> (Vec<Event>, Result<Turn, AgentError>) {
    let mut p = ReplyParser::new(protocol);
    let mut events = vec![];
    for line in body.split_inclusive('\n') {
        match p.line(line) {
            Ok(e) => events.extend(e),
            Err(e) => return (events, Err(e)),
        }
    }
    (events, p.finish())
}

pub fn turn(protocol: Protocol, body: &str) -> Turn {
    run(protocol, body).1.expect("reply should parse")
}

pub fn error(protocol: Protocol, body: &str) -> AgentError {
    run(protocol, body).1.expect_err("reply should fail")
}

/// The text of every `Event::Text`, joined.
pub fn streamed_text(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The events that carry reply content (text, thinking, tool calls started),
/// without usage, stop and argument-progress events.
pub fn content(events: &[Event]) -> Vec<Event> {
    events
        .iter()
        .filter(|e| matches!(e, Event::Text { .. } | Event::Thinking { .. } | Event::ToolCallStarted { .. }))
        .cloned()
        .collect()
}

pub fn text(index: usize, t: &str) -> Event {
    Event::Text { index, text: t.into() }
}

pub fn thinking(index: usize, t: &str) -> Event {
    Event::Thinking { index, text: t.into() }
}

/// [`scriptorium_agent::Thread::check_sendable`] for a target that wrote none
/// of the thread, so no replay data counts.
pub fn sendable(t: &scriptorium_agent::Thread) -> Result<(), AgentError> {
    t.check_sendable(Protocol::Anthropic, &scriptorium_agent::Origin::new("https://elsewhere.invalid", "none"))
}
