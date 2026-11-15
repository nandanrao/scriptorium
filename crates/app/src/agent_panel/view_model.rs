//! The chat panel's view model: a [`Thread`] plus the run's streaming buffer
//! become the rows the message list shows. Pure: no GPUI, no I/O, no clocks.
//!
//! The shell owns one [`Stream`] per open thread and feeds it what the run
//! reports ([`Stream::event`], [`Stream::wait`], [`Stream::fail`],
//! [`Stream::stop`], [`Stream::clear`]). To paint, call [`rows`] with the
//! thread, the stream and a [`Context`] (or keep a [`RowCache`], which
//! reuses the rows of finished messages across frames); every row has a
//! stable [`Key`], so the view can
//! keep one `TextViewState` per key across repaints and across the moment the
//! pushed turn replaces the streaming rows.

// Consumed by the message view (U3), composer (U6) and driver (U4).
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use scriptorium_agent::{AgentError, Block, Event, Message, Role, StopReason, Thread, Usage, turn};
use scriptorium_chat::labels;
use serde_json::Value;

/// Lines of a tool result shown before "Show all".
pub const RESULT_LINES: usize = 40;

/// Identity of a row across repaints.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// The writer's message `message`.
    User(usize),
    /// The `block`th text or reasoning block of message `message`, counting
    /// only those two kinds. While a reply streams, `message` is the index it
    /// will get in the thread (`thread.messages.len()`) and `block` counts
    /// streamed text and reasoning parts in arrival order, so the pushed turn
    /// lands on the same keys as its streaming rows even when it holds blocks
    /// that never streamed (a redacted reasoning block sends no event).
    Block { message: usize, block: usize },
    /// Redacted reasoning: block `block` (raw index) of message `message`.
    /// It never streams, so it has keys of its own.
    Redacted { message: usize, block: usize },
    /// A notice: block `block` (raw index) of message `message`.
    Notice { message: usize, block: usize },
    /// A tool row, by call id.
    Tool(String),
    /// A tool call that came without an id: the `n`th call of message
    /// `message` (streamed or pushed), so such calls don't share one key.
    ToolAt { message: usize, n: usize },
    /// A line after the messages (error, waiting, stopping).
    Tail(&'static str),
}

/// Where a tool call stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    /// Arguments are still arriving (`Event::ToolArgs`).
    Streaming,
    /// Waiting for the result.
    Running,
    Done,
    /// The result is an error: shown in the warning colour.
    Failed,
    /// Never ran because the run was stopped.
    NotRun,
}

/// One tool call with its result, as a single expandable row.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolRow {
    pub id: String,
    pub name: String,
    pub state: ToolState,
    /// The one-line label (`chat::labels`).
    pub label: String,
    /// Readable arguments for the expanded row; empty while they stream.
    pub args: Arc<str>,
    /// The result, split for "first 40 lines, then Show all"; `None` until it
    /// arrives (and for a call that never ran).
    pub result: Option<Preview>,
}

/// A result cut to its first lines: the whole text, shared, and where the
/// shown part ends in it (no second copy).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    full: Arc<str>,
    /// Byte length of the shown prefix.
    shown_len: usize,
    /// Lines left out; non-zero means offer "Show all".
    pub hidden: usize,
}

impl Preview {
    pub fn new(text: &str, max_lines: usize) -> Self {
        let total = text.lines().count();
        let shown_len = if total <= max_lines {
            text.len()
        } else if max_lines == 0 {
            0
        } else {
            // The end of line `max_lines`, without its line break. There are
            // more lines after it, so its `\n` exists.
            let end = text.match_indices('\n').nth(max_lines - 1).map_or(text.len(), |(i, _)| i);
            text[..end].strip_suffix('\r').map_or(end, str::len)
        };
        Preview { full: text.into(), shown_len, hidden: total.saturating_sub(max_lines) }
    }

    /// The first lines.
    pub fn shown(&self) -> &str {
        &self.full[..self.shown_len]
    }

    /// The whole result, for "Show all".
    pub fn full(&self) -> &str {
        &self.full
    }

    /// The whole result, shared (no copy).
    pub fn full_shared(&self) -> Arc<str> {
        self.full.clone()
    }
}

/// What a row is.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// The writer's message: plain text.
    User {
        text: Arc<str>,
    },
    /// Assistant text: rendered as Markdown. `streaming` while it grows.
    Text {
        text: Arc<str>,
        streaming: bool,
    },
    /// Collapsed "Reasoning". `hidden`: the provider redacted it.
    Reasoning {
        text: Arc<str>,
        hidden: bool,
        streaming: bool,
    },
    Tool(ToolRow),
    /// A muted line for the writer ("Stopped before the tools ran.").
    Notice {
        text: String,
    },
    /// The run failed; the thread is unchanged and may be sent again.
    Error {
        /// Plain words for the writer.
        text: String,
        /// The provider's or library's own message, for an expandable detail.
        detail: String,
        retryable: bool,
        /// The button the row offers, if any.
        action: Option<ErrorAction>,
    },
    /// A retryable failure; sending again in `secs`.
    Waiting {
        secs: u64,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub key: Key,
    pub kind: Kind,
}

impl Row {
    /// The heading to show on a reasoning row.
    pub fn reasoning_title(hidden: bool) -> &'static str {
        if hidden { "Reasoning (hidden by the provider)" } else { "Reasoning" }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Part {
    Text { index: usize, text: String },
    Thinking { index: usize, text: String },
    Tool { id: String, name: String, bytes: usize },
}

/// A failure shown at the tail of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// Plain words for the writer.
    pub text: String,
    /// The raw error text.
    pub detail: String,
    pub retryable: bool,
    pub action: Option<ErrorAction>,
}

/// What an error row offers (docs/plans/chat-panel.md, "Cancel and errors").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorAction {
    /// Send the thread again ([Retry]).
    Retry,
    /// Go on after the round limit ([Continue]).
    Continue,
    /// The key was refused, or there is no account or key.
    ManageAccounts,
    /// The provider refused the request (unknown model…).
    ChooseModel,
    /// The thread can't go on as it is (the tools-changed banner, U7).
    NewThread,
}

impl ErrorAction {
    pub fn label(self) -> &'static str {
        match self {
            ErrorAction::Retry => "Retry",
            ErrorAction::Continue => "Continue",
            ErrorAction::ManageAccounts => "Manage accounts",
            ErrorAction::ChooseModel => "Choose model",
            ErrorAction::NewThread => "New thread",
        }
    }
}

/// The button for a failed run's `error`.
pub fn error_action(error: &AgentError) -> Option<ErrorAction> {
    use scriptorium_agent::ErrorKind;
    match error {
        AgentError::Api { kind: ErrorKind::Auth, .. } => Some(ErrorAction::ManageAccounts),
        AgentError::Api { kind: ErrorKind::BadRequest, .. } => Some(ErrorAction::ChooseModel),
        AgentError::Api { .. } | AgentError::Parse(_) | AgentError::Mismatch(_) => Some(ErrorAction::Retry),
        AgentError::NotReady(_) | AgentError::File(_) => None,
    }
}

/// The reply being streamed, plus the run's state lines. Fed by the shell.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stream {
    parts: Vec<Part>,
    usage: Option<Usage>,
    stopped: bool,
    waiting: Option<u64>,
    failure: Option<Failure>,
}

impl Stream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Nothing buffered and no state line: the list is just the thread.
    pub fn is_empty(&self) -> bool {
        *self == Stream::default()
    }

    /// Takes one reply event. Text and reasoning pieces with the same index
    /// continue one block; a tool call adds a running row.
    pub fn event(&mut self, ev: Event) {
        self.waiting = None;
        self.failure = None;
        match ev {
            Event::Text { index, text } => {
                let found = self.parts.iter_mut().find_map(|p| match p {
                    Part::Text { index: i, text } if *i == index => Some(text),
                    _ => None,
                });
                match found {
                    Some(t) => t.push_str(&text),
                    None => self.parts.push(Part::Text { index, text }),
                }
            }
            Event::Thinking { index, text } => {
                let found = self.parts.iter_mut().find_map(|p| match p {
                    Part::Thinking { index: i, text } if *i == index => Some(text),
                    _ => None,
                });
                match found {
                    Some(t) => t.push_str(&text),
                    None => self.parts.push(Part::Thinking { index, text }),
                }
            }
            Event::ToolCallStarted { id, name } => {
                let known =
                    !id.is_empty() && self.parts.iter().any(|p| matches!(p, Part::Tool { id: i, .. } if *i == id));
                if !known {
                    self.parts.push(Part::Tool { id, name, bytes: 0 });
                }
            }
            Event::ToolArgs { id, bytes_so_far } => {
                // The id may be empty: then it is the latest call.
                let found = self.parts.iter_mut().rev().find_map(|p| match p {
                    Part::Tool { id: i, bytes, .. } if id.is_empty() || *i == id => Some(bytes),
                    _ => None,
                });
                if let Some(b) = found {
                    *b = bytes_so_far;
                }
            }
            Event::Usage(u) => self.usage = Some(u),
            Event::Stop(StopReason::Cancelled) => self.stopped = true,
            Event::Stop(_) => {}
        }
    }

    /// A retryable failure: the run waits `secs` before sending again. The
    /// half-streamed reply is dropped (the request is sent again from scratch).
    pub fn wait(&mut self, secs: u64) {
        self.clear();
        self.waiting = Some(secs);
    }

    /// `Input::Failed` ended the request or the run: the buffer is cleared
    /// (turn.rs contract) and the error shows at the tail until the next event.
    pub fn fail(&mut self, error: &AgentError) {
        self.clear();
        let retryable = matches!(error, AgentError::Api { retryable: true, .. });
        let action = error_action(error);
        self.failure = Some(Failure { text: plain_error(error), detail: error.to_string(), retryable, action });
    }

    /// A failure told in words (the thread is busy elsewhere, the round
    /// limit…), offering `action`: shown like [`Stream::fail`], no detail.
    pub fn fail_with(&mut self, text: impl Into<String>, action: Option<ErrorAction>) {
        self.clear();
        let retryable = matches!(action, Some(ErrorAction::Retry | ErrorAction::Continue));
        self.failure = Some(Failure { text: text.into(), detail: String::new(), retryable, action });
    }

    /// The writer pressed Stop: running rows read "Not run (stopped)" and a
    /// "Stopping…" line shows until the run reports (then [`Stream::clear`]).
    pub fn stop(&mut self) {
        self.stopped = true;
        self.waiting = None;
    }

    /// The reply was pushed to the thread, or a new request or run starts:
    /// nothing streams any more. Also clears any state line.
    pub fn clear(&mut self) {
        *self = Stream::default();
    }

    pub fn failure(&self) -> Option<&Failure> {
        self.failure.as_ref()
    }

    pub fn is_stopping(&self) -> bool {
        self.stopped
    }

    /// The in-flight request's usage so far (the last piece is the total).
    pub fn usage(&self) -> Option<Usage> {
        self.usage
    }
}

/// What went wrong, in words for the writer (the raw text stays in the detail).
pub fn plain_error(error: &AgentError) -> String {
    use scriptorium_agent::ErrorKind;
    match error {
        AgentError::Api { kind, .. } => match kind {
            ErrorKind::Auth => "The service refused the key. Check your key in Accounts.",
            ErrorKind::RateLimit => "The service is limiting requests, or the credit is used up. Check Accounts.",
            ErrorKind::Overloaded => "The service is busy right now.",
            ErrorKind::Offline => "Can't reach the service. Check your connection.",
            ErrorKind::BadRequest => "The service refused the request. Check the model in Accounts.",
            ErrorKind::Other => "The service had a problem.",
        },
        AgentError::Parse(_) => "The service's reply wasn't understood.",
        AgentError::NotReady(_) | AgentError::Mismatch(_) => "This conversation can't be sent as it is.",
        AgentError::File(_) => "This conversation's file can't be read.",
    }
    .into()
}

/// What the view model needs besides the thread and the stream.
#[derive(Debug, Clone, Copy, Default)]
pub struct Context<'a> {
    /// A run is in progress ([`scriptorium_agent::turn::Run::is_active`]).
    /// Calls without a result read "running" only then; otherwise they are
    /// "not run".
    pub active: bool,
    /// The name of the thread's review, for "→ Dialogue pass" in tool labels
    /// (`None`: the thread's own, unnamed).
    pub review: Option<&'a str>,
}

/// The rows to show: the thread's messages, then the streaming reply, then
/// the run's state line. Builds everything afresh; a view that repaints
/// every frame keeps a [`RowCache`] instead.
pub fn rows(thread: &Thread, stream: &Stream, cx: Context) -> Vec<Row> {
    RowCache::default().rows(thread, stream, cx)
}

/// [`rows`], remembering the rows of finished messages (every tool call
/// answered), so a repaint copies their text only once. Cached rows share
/// their text ([`Arc<str>`]), so handing them out again is cheap.
///
/// An entry is used only while its message is unchanged (compared in full,
/// which allocates nothing), so a thread that is replaced, reloaded or has
/// its last message withdrawn is still shown right. One cache per open
/// thread view.
#[derive(Debug, Default)]
pub struct RowCache {
    done: Vec<Option<(Message, Vec<Row>)>>,
    /// The review name the cached tool labels were made with.
    review: Option<String>,
}

impl RowCache {
    pub fn rows(&mut self, thread: &Thread, stream: &Stream, cx: Context) -> Vec<Row> {
        if self.review.as_deref() != cx.review {
            self.done.clear();
            self.review = cx.review.map(str::to_string);
        }
        self.done.resize_with(thread.messages.len(), || None);
        let results: HashMap<&str, (&str, bool)> = thread
            .messages
            .iter()
            .flat_map(|m| m.blocks.iter())
            .filter_map(|b| match b {
                Block::ToolResult(r) => Some((r.id.as_str(), (r.content.as_str(), r.is_error))),
                _ => None,
            })
            .collect();
        let mut out = Vec::new();
        for (mi, m) in thread.messages.iter().enumerate() {
            if let Some((cached, rows)) = &self.done[mi]
                && cached == m
            {
                out.extend(rows.iter().cloned());
                continue;
            }
            let rows = message_rows(mi, m, &results, stream, cx);
            let finished = m.blocks.iter().all(|b| match b {
                Block::ToolCall(c) => results.contains_key(c.id.as_str()),
                _ => true,
            });
            out.extend(rows.iter().cloned());
            self.done[mi] = finished.then(|| (m.clone(), rows));
        }
        stream_rows(thread.messages.len(), stream, &mut out);
        out
    }
}

/// The rows of message `mi`.
fn message_rows(
    mi: usize,
    m: &Message,
    results: &HashMap<&str, (&str, bool)>,
    stream: &Stream,
    cx: Context,
) -> Vec<Row> {
    let mut out = Vec::new();
    if m.role == Role::User {
        let text: String = m
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        if !text.trim().is_empty() {
            out.push(Row { key: Key::User(mi), kind: Kind::User { text: text.into() } });
        }
    }
    let mut keys = Keys::new(mi);
    for (bi, b) in m.blocks.iter().enumerate() {
        match (m.role, b) {
            (Role::Assistant, Block::Text { text }) => {
                // Counted even when blank: it streamed as a part.
                let key = keys.block();
                if !text.trim().is_empty() {
                    out.push(Row { key, kind: Kind::Text { text: text.as_str().into(), streaming: false } });
                }
            }
            (Role::Assistant, Block::Thinking { text, .. }) => {
                out.push(Row {
                    key: keys.block(),
                    kind: Kind::Reasoning { text: text.as_str().into(), hidden: false, streaming: false },
                });
            }
            (Role::Assistant, Block::Native { value, .. })
                if value.get("type").and_then(Value::as_str) == Some("redacted_thinking") =>
            {
                out.push(Row {
                    key: Key::Redacted { message: mi, block: bi },
                    kind: Kind::Reasoning { text: "".into(), hidden: true, streaming: false },
                });
            }
            (Role::Assistant, Block::ToolCall(c)) => {
                let row = match results.get(c.id.as_str()) {
                    Some(&(content, is_error)) => finished(&c.name, &c.input, &c.id, content, is_error, cx),
                    None if cx.active && !stream.stopped => running(&c.name, &c.input, &c.id),
                    None => not_run(&c.name, &c.id),
                };
                out.push(Row { key: keys.tool(&c.id), kind: Kind::Tool(row) });
            }
            (_, Block::Notice { text }) => {
                out.push(Row { key: Key::Notice { message: mi, block: bi }, kind: Kind::Notice { text: text.clone() } })
            }
            _ => {}
        }
    }
    out
}

/// The streaming reply (it will be message `message`) and the state lines.
fn stream_rows(message: usize, stream: &Stream, out: &mut Vec<Row>) {
    let mut keys = Keys::new(message);
    for p in &stream.parts {
        match p {
            Part::Text { text, .. } => {
                out.push(Row { key: keys.block(), kind: Kind::Text { text: text.as_str().into(), streaming: true } });
            }
            Part::Thinking { text, .. } => {
                out.push(Row {
                    key: keys.block(),
                    kind: Kind::Reasoning { text: text.as_str().into(), hidden: false, streaming: true },
                });
            }
            Part::Tool { id, name, bytes } => {
                let row = if stream.stopped {
                    not_run(name, id)
                } else {
                    let label = labels::running(name, &Value::Null, Some(*bytes).filter(|b| *b > 0));
                    ToolRow {
                        id: id.clone(),
                        name: name.clone(),
                        state: ToolState::Streaming,
                        label,
                        args: "".into(),
                        result: None,
                    }
                };
                out.push(Row { key: keys.tool(id), kind: Kind::Tool(row) });
            }
        }
    }
    if let Some(secs) = stream.waiting {
        out.push(Row { key: Key::Tail("waiting"), kind: Kind::Waiting { secs } });
    }
    if let Some(f) = &stream.failure {
        out.push(Row {
            key: Key::Tail("error"),
            kind: Kind::Error {
                text: f.text.clone(),
                detail: f.detail.clone(),
                retryable: f.retryable,
                action: f.action,
            },
        });
    }
    if stream.stopped {
        out.push(Row { key: Key::Tail("stopping"), kind: Kind::Notice { text: "Stopping…".into() } });
    }
}

/// Hands out the keys of one message's rows, the same way for a streaming
/// reply and for the pushed turn (see [`Key::Block`] and [`Key::ToolAt`]).
struct Keys {
    message: usize,
    blocks: usize,
    tools: usize,
}

impl Keys {
    fn new(message: usize) -> Self {
        Keys { message, blocks: 0, tools: 0 }
    }

    /// The next text or reasoning block.
    fn block(&mut self) -> Key {
        self.blocks += 1;
        Key::Block { message: self.message, block: self.blocks - 1 }
    }

    /// The next tool call: by id, or by position when it has none.
    fn tool(&mut self, id: &str) -> Key {
        self.tools += 1;
        if id.is_empty() { Key::ToolAt { message: self.message, n: self.tools - 1 } } else { Key::Tool(id.into()) }
    }
}

fn finished(name: &str, input: &Value, id: &str, content: &str, is_error: bool, cx: Context) -> ToolRow {
    if content == turn::CANCELLED {
        return not_run(name, id);
    }
    let (state, label) = if is_error {
        (ToolState::Failed, labels::failed(name, content.lines().next().unwrap_or("").trim()))
    } else {
        (ToolState::Done, labels::done(name, input, content, cx.review))
    };
    ToolRow {
        id: id.into(),
        name: name.into(),
        state,
        label,
        args: args_text(input).into(),
        result: Some(Preview::new(content, RESULT_LINES)),
    }
}

fn running(name: &str, input: &Value, id: &str) -> ToolRow {
    ToolRow {
        id: id.into(),
        name: name.into(),
        state: ToolState::Running,
        label: labels::running(name, input, None),
        args: args_text(input).into(),
        result: None,
    }
}

fn not_run(name: &str, id: &str) -> ToolRow {
    ToolRow {
        id: id.into(),
        name: name.into(),
        state: ToolState::NotRun,
        label: labels::NOT_RUN.into(),
        args: "".into(),
        result: None,
    }
}

/// Arguments as readable indented JSON; empty for none.
pub fn args_text(args: &Value) -> String {
    match args {
        Value::Null => String::new(),
        v => serde_json::to_string_pretty(v).unwrap_or_default(),
    }
}

/// "18.2k tokens · $0.24" for the thread plus the request in flight. `priced`
/// is the price-table cost of that total, used when the provider reported none.
pub fn usage_line(thread: &Thread, stream: &Stream, priced: Option<f64>) -> String {
    let mut total = thread.usage();
    if let Some(u) = stream.usage() {
        total.add(u);
    }
    labels::usage_text(&total, priced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use scriptorium_agent::{Message, Origin, ToolCall, ToolResult, Turn};
    use serde_json::json;

    fn ev_text(index: usize, t: &str) -> Event {
        Event::Text { index, text: t.into() }
    }

    fn call(id: &str, name: &str, input: Value) -> ToolCall {
        ToolCall { id: id.into(), name: name.into(), input, input_error: None, raw_input: None, extra: None }
    }

    fn asst(blocks: Vec<Block>, usage: Usage, stop: StopReason) -> Turn {
        Turn { message: Message::assistant(blocks), stop, usage, refusal: None }
    }

    fn origin() -> Origin {
        Origin::new("https://x", "m")
    }

    fn kinds(rows: &[Row]) -> Vec<&Kind> {
        rows.iter().map(|r| &r.kind).collect()
    }

    fn build(t: &Thread, s: &Stream, active: bool) -> Vec<Row> {
        rows(t, s, Context { active, review: Some("Dialogue pass") })
    }

    fn text_turn(t: &mut Thread, text: &str, usage: Usage) {
        t.push_turn(asst(vec![Block::Text { text: text.into() }], usage, StopReason::EndTurn), origin());
    }

    #[test]
    fn user_and_assistant_text() {
        let mut t = Thread::default();
        assert!(t.push_user("Hello"));
        text_turn(&mut t, "Hi there", Usage::default());
        let r = build(&t, &Stream::new(), false);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].kind, Kind::User { text: "Hello".into() });
        assert_eq!(r[1].kind, Kind::Text { text: "Hi there".into(), streaming: false });
        assert_eq!(r[1].key, Key::Block { message: 1, block: 0 });
    }

    #[test]
    fn streaming_deltas_append_by_index() {
        let t = Thread::default();
        let mut s = Stream::new();
        assert!(s.is_empty());
        s.event(Event::Thinking { index: 0, text: "hm".into() });
        s.event(Event::Thinking { index: 0, text: "m".into() });
        s.event(ev_text(1, "Hel"));
        s.event(ev_text(1, "lo"));
        let r = build(&t, &s, true);
        assert_eq!(
            kinds(&r),
            vec![
                &Kind::Reasoning { text: "hmm".into(), hidden: false, streaming: true },
                &Kind::Text { text: "Hello".into(), streaming: true },
            ]
        );
        assert!(!s.is_empty());
    }

    #[test]
    fn streaming_keys_match_the_pushed_turn() {
        let mut t = Thread::default();
        assert!(t.push_user("q"));
        let mut s = Stream::new();
        s.event(Event::Thinking { index: 0, text: "why".into() });
        s.event(ev_text(1, "answer"));
        let streaming: Vec<Key> = build(&t, &s, true).into_iter().skip(1).map(|r| r.key).collect();
        t.push_turn(
            asst(
                vec![Block::Thinking { text: "why".into(), signature: None }, Block::Text { text: "answer".into() }],
                Usage::default(),
                StopReason::EndTurn,
            ),
            origin(),
        );
        s.clear();
        let pushed: Vec<Key> = build(&t, &s, false).into_iter().skip(1).map(|r| r.key).collect();
        assert_eq!(streaming, pushed);
    }

    #[test]
    fn a_redacted_block_mid_reply_keeps_the_other_keys() {
        let mut t = Thread::default();
        assert!(t.push_user("q"));
        let mut s = Stream::new();
        // Anthropic streams no event for the redacted block (index 1).
        s.event(Event::Thinking { index: 0, text: "why".into() });
        s.event(ev_text(2, "answer"));
        s.event(Event::ToolCallStarted { id: "c1".into(), name: "read_scene".into() });
        s.event(ev_text(4, "more"));
        let streaming: Vec<Key> = build(&t, &s, true).into_iter().skip(1).map(|r| r.key).collect();
        let redacted = Block::Native {
            protocol: scriptorium_agent::Protocol::Anthropic,
            value: json!({"type": "redacted_thinking"}),
        };
        t.push_turn(
            asst(
                vec![
                    Block::Thinking { text: "why".into(), signature: None },
                    redacted,
                    Block::Text { text: "answer".into() },
                    Block::ToolCall(call("c1", "read_scene", json!({"scene": 1}))),
                    Block::Text { text: "more".into() },
                ],
                Usage::default(),
                StopReason::ToolUse,
            ),
            origin(),
        );
        s.clear();
        let pushed: Vec<Key> = build(&t, &s, true).into_iter().skip(1).map(|r| r.key).collect();
        assert_eq!(
            pushed,
            [
                Key::Block { message: 1, block: 0 },
                Key::Redacted { message: 1, block: 1 },
                Key::Block { message: 1, block: 1 },
                Key::Tool("c1".into()),
                Key::Block { message: 1, block: 2 },
            ]
        );
        let without_redacted: Vec<Key> = pushed.into_iter().filter(|k| !matches!(k, Key::Redacted { .. })).collect();
        assert_eq!(streaming, without_redacted);
    }

    #[test]
    fn calls_without_ids_get_keys_of_their_own() {
        let mut t = Thread::default();
        assert!(t.push_user("q"));
        let mut s = Stream::new();
        s.event(Event::ToolCallStarted { id: String::new(), name: "read_scene".into() });
        s.event(Event::ToolCallStarted { id: String::new(), name: "script_info".into() });
        let streaming: Vec<Key> = build(&t, &s, true).into_iter().skip(1).map(|r| r.key).collect();
        assert_eq!(streaming, [Key::ToolAt { message: 1, n: 0 }, Key::ToolAt { message: 1, n: 1 }]);
        // push_turn gives id-less calls ids; a message that has none anyway
        // (pushed some other way) still keys each call apart.
        t.messages.push(Message::assistant(vec![
            Block::ToolCall(call("", "read_scene", json!({"scene": 1}))),
            Block::ToolCall(call("", "script_info", json!({}))),
        ]));
        s.clear();
        let pushed: Vec<Key> = build(&t, &s, true).into_iter().skip(1).map(|r| r.key).collect();
        assert_eq!(streaming, pushed);
    }

    #[test]
    fn pushed_turn_replaces_streaming_rows() {
        let mut t = Thread::default();
        assert!(t.push_user("q"));
        let mut s = Stream::new();
        s.event(ev_text(0, "ans"));
        assert_eq!(build(&t, &s, true).len(), 2);
        text_turn(&mut t, "answer", Usage::default());
        s.clear();
        let r = build(&t, &s, false);
        assert_eq!(r.len(), 2);
        assert_eq!(r[1].kind, Kind::Text { text: "answer".into(), streaming: false });
    }

    #[test]
    fn whitespace_text_and_native_rows() {
        let mut t = Thread::default();
        assert!(t.push_user("q"));
        let native = |value| Block::Native { protocol: scriptorium_agent::Protocol::Anthropic, value };
        t.push_turn(
            asst(
                vec![
                    Block::Text { text: "  \n".into() },
                    native(json!({"type": "redacted_thinking", "data": "x"})),
                    native(json!({"type": "other"})),
                ],
                Usage::default(),
                StopReason::EndTurn,
            ),
            origin(),
        );
        let r = build(&t, &Stream::new(), false);
        assert_eq!(r.len(), 2);
        assert_eq!(r[1].kind, Kind::Reasoning { text: "".into(), hidden: true, streaming: false });
        assert_eq!(Row::reasoning_title(true), "Reasoning (hidden by the provider)");
        assert_eq!(Row::reasoning_title(false), "Reasoning");
    }

    #[test]
    fn tool_row_lifecycle() {
        let mut t = Thread::default();
        assert!(t.push_user("go"));
        let mut s = Stream::new();
        s.event(Event::ToolCallStarted { id: "c1".into(), name: "suggest_changes".into() });
        s.event(Event::ToolArgs { id: "c1".into(), bytes_so_far: 3200 });
        let r = build(&t, &s, true);
        let Kind::Tool(row) = &r[1].kind else { panic!() };
        assert_eq!(row.state, ToolState::Streaming);
        assert_eq!(row.label, "Writing suggestions… (3.2 kB)");

        // Pushed, waiting for its result.
        let input = json!({"suggestions": [{}, {}]});
        t.push_turn(
            asst(vec![Block::ToolCall(call("c1", "suggest_changes", input))], Usage::default(), StopReason::ToolUse),
            origin(),
        );
        s.clear();
        let r = build(&t, &s, true);
        let Kind::Tool(row) = &r[1].kind else { panic!() };
        assert_eq!(row.state, ToolState::Running);
        assert_eq!(row.label, "Writing 2 suggestions…");
        assert!(row.args.contains("suggestions"));
        assert!(row.result.is_none());
        assert_eq!(r[1].key, Key::Tool("c1".into()));

        // Answered.
        t.push_tool_results(vec![ToolResult { id: "c1".into(), content: "ok".into(), is_error: false }]).unwrap();
        let r = build(&t, &s, true);
        let Kind::Tool(row) = &r[1].kind else { panic!() };
        assert_eq!(row.state, ToolState::Done);
        assert!(row.label.starts_with("Proposed"), "{}", row.label);
        assert_eq!(row.result.as_ref().unwrap().shown(), "ok");
        // The Tool message adds no row of its own.
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn tool_args_with_empty_id_go_to_the_latest_call() {
        let mut s = Stream::new();
        s.event(Event::ToolCallStarted { id: "a".into(), name: "read_scene".into() });
        s.event(Event::ToolCallStarted { id: "b".into(), name: "suggest_changes".into() });
        s.event(Event::ToolArgs { id: String::new(), bytes_so_far: 50 });
        let r = build(&Thread::default(), &s, true);
        let (Kind::Tool(a), Kind::Tool(b)) = (&r[0].kind, &r[1].kind) else { panic!() };
        assert_eq!(a.label, "Reading a scene…");
        assert_eq!(b.label, "Writing suggestions… (50 B)");
    }

    fn thread_with_call(name: &str, result: Option<ToolResult>) -> Thread {
        let mut t = Thread::default();
        assert!(t.push_user("go"));
        t.push_turn(
            asst(vec![Block::ToolCall(call("c1", name, json!({"scene": 12})))], Usage::default(), StopReason::ToolUse),
            origin(),
        );
        if let Some(r) = result {
            t.push_tool_results(vec![r]).unwrap();
        }
        t
    }

    #[test]
    fn tool_error_shows_the_reason() {
        let t = thread_with_call(
            "read_scene",
            Some(ToolResult { id: "c1".into(), content: "no such scene\nmore".into(), is_error: true }),
        );
        let r = build(&t, &Stream::new(), false);
        let Kind::Tool(row) = &r[1].kind else { panic!() };
        assert_eq!(row.state, ToolState::Failed);
        assert_eq!(row.label, "Couldn't read the scene");
        assert_eq!(row.result.as_ref().unwrap().full(), "no such scene\nmore");
    }

    #[test]
    fn cancelled_calls_read_not_run() {
        let mut t = thread_with_call("read_scene", None);
        assert_eq!(turn::cancel(&mut t), 1);
        let r = build(&t, &Stream::new(), false);
        let Kind::Tool(row) = &r[1].kind else { panic!() };
        assert_eq!(row.state, ToolState::NotRun);
        assert_eq!(row.label, "Not run (stopped)");
        assert!(row.result.is_none());
        assert_eq!(r[2].kind, Kind::Notice { text: turn::CANCELLED_NOTICE.into() });
    }

    #[test]
    fn unanswered_call_with_no_run_is_not_run() {
        let t = thread_with_call("read_scene", None);
        let r = build(&t, &Stream::new(), false);
        let Kind::Tool(row) = &r[1].kind else { panic!() };
        assert_eq!(row.state, ToolState::NotRun);
    }

    #[test]
    fn stop_marks_running_rows_and_shows_stopping() {
        let t = thread_with_call("read_scene", None);
        let mut s = Stream::new();
        s.event(Event::ToolCallStarted { id: "c2".into(), name: "script_info".into() });
        s.stop();
        assert!(s.is_stopping());
        let r = build(&t, &s, true);
        let (Kind::Tool(a), Kind::Tool(b)) = (&r[1].kind, &r[2].kind) else { panic!() };
        assert_eq!((a.state, b.state), (ToolState::NotRun, ToolState::NotRun));
        assert_eq!(r.last().unwrap().kind, Kind::Notice { text: "Stopping…".into() });
        s.clear();
        assert!(!build(&t, &s, false).iter().any(|r| r.key == Key::Tail("stopping")));
    }

    #[test]
    fn stopped_reply_keeps_text_with_its_notice() {
        let mut t = Thread::default();
        assert!(t.push_user("q"));
        t.push_turn(asst(vec![Block::Text { text: "part".into() }], Usage::unknown(), StopReason::Cancelled), origin());
        let r = build(&t, &Stream::new(), false);
        assert_eq!(r[1].kind, Kind::Text { text: "part".into(), streaming: false });
        assert_eq!(r[2].kind, Kind::Notice { text: scriptorium_agent::STOPPED.into() });
    }

    #[test]
    fn cancel_event_marks_stream_stopped() {
        let mut s = Stream::new();
        s.event(ev_text(0, "x"));
        s.event(Event::Stop(StopReason::Cancelled));
        assert!(s.is_stopping());
    }

    #[test]
    fn failure_clears_buffer_and_shows_error() {
        let mut s = Stream::new();
        s.event(ev_text(0, "partial"));
        s.event(Event::Usage(Usage { input: 5, ..Usage::default() }));
        s.fail(&AgentError::offline("no connection"));
        assert!(s.usage().is_none());
        let r = build(&Thread::default(), &s, false);
        assert_eq!(
            kinds(&r),
            vec![&Kind::Error {
                text: plain_error(&AgentError::offline("no connection")),
                detail: "no connection".into(),
                retryable: true,
                action: Some(ErrorAction::Retry),
            }]
        );
        assert_eq!(s.failure().unwrap().detail, "no connection");
        // A new event clears the line.
        s.event(ev_text(0, "again"));
        assert!(s.failure().is_none());
    }

    #[test]
    fn non_retryable_error() {
        let mut s = Stream::new();
        s.fail(&AgentError::Parse("bad".into()));
        let r = build(&Thread::default(), &s, false);
        assert_eq!(
            kinds(&r),
            vec![&Kind::Error {
                text: "The service's reply wasn't understood.".into(),
                detail: "unexpected reply: bad".into(),
                retryable: false,
                action: Some(ErrorAction::Retry),
            }]
        );
    }

    #[test]
    fn wait_drops_the_partial_reply() {
        let mut s = Stream::new();
        s.event(ev_text(0, "partial"));
        s.wait(4);
        let r = build(&Thread::default(), &s, true);
        assert_eq!(kinds(&r), vec![&Kind::Waiting { secs: 4 }]);
        s.event(ev_text(0, "fresh"));
        let r = build(&Thread::default(), &s, true);
        assert_eq!(kinds(&r), vec![&Kind::Text { text: "fresh".into(), streaming: true }]);
    }

    #[test]
    fn refusal_withdraws_message_and_shows_notice() {
        let mut t = Thread::default();
        assert!(t.push_user("bad ask"));
        let p = t.push_turn(
            Turn {
                message: Message::assistant(vec![]),
                stop: StopReason::Refusal,
                usage: Usage { input: 10, output: 0, ..Usage::default() },
                refusal: Some("no".into()),
            },
            origin(),
        );
        assert_eq!(p.withdrawn.as_deref(), Some("bad ask"));
        let r = build(&t, &Stream::new(), false);
        assert_eq!(r.len(), 1);
        assert!(matches!(r[0].kind, Kind::Notice { .. }));
    }

    #[test]
    fn long_results_are_cut_to_forty_lines() {
        let text: String = (1..=100).map(|n| format!("line {n}\n")).collect();
        let p = Preview::new(&text, RESULT_LINES);
        assert_eq!(p.shown().lines().count(), 40);
        assert!(p.shown().starts_with("line 1\n") && p.shown().ends_with("line 40"), "{}", p.shown());
        assert_eq!(p.hidden, 60);
        assert_eq!(p.full(), text);
        let short = Preview::new("a\nb", RESULT_LINES);
        assert_eq!((short.shown(), short.hidden), ("a\nb", 0));
        let exact: String = (1..=40).map(|n| format!("{n}\n")).collect();
        assert_eq!(Preview::new(&exact, 40).hidden, 0);
        let crlf = Preview::new("a\r\nb\r\nc", 2);
        assert_eq!((crlf.shown(), crlf.hidden), ("a\r\nb", 1));
        assert_eq!((Preview::new("a\nb", 0).shown(), Preview::new("a\nb", 0).hidden), ("", 2));
    }

    #[test]
    fn the_cache_reuses_finished_messages_and_drops_changed_ones() {
        let mut cache = RowCache::default();
        let cx = Context { active: true, review: Some("Dialogue pass") };
        let mut t = thread_with_call("read_scene", None);
        let first = cache.rows(&t, &Stream::new(), cx);
        assert_eq!(first, rows(&t, &Stream::new(), cx));
        // The unanswered call isn't cached; once answered, the row updates.
        t.push_tool_results(vec![ToolResult { id: "c1".into(), content: "scene".into(), is_error: false }]).unwrap();
        let answered = cache.rows(&t, &Stream::new(), cx);
        assert_eq!(answered, rows(&t, &Stream::new(), cx));
        let Kind::Tool(row) = &answered[1].kind else { panic!() };
        assert_eq!(row.state, ToolState::Done);
        // Cached now: the next frame shares the same text.
        let again = cache.rows(&t, &Stream::new(), cx);
        let (Kind::Tool(a), Kind::Tool(b)) = (&answered[1].kind, &again[1].kind) else { panic!() };
        assert!(Arc::ptr_eq(&a.args, &b.args));
        // A different thread in the same view, and another review name, are rebuilt.
        let mut other = Thread::default();
        assert!(other.push_user("other"));
        assert_eq!(cache.rows(&other, &Stream::new(), cx), rows(&other, &Stream::new(), cx));
        let cx2 = Context { review: None, ..cx };
        assert_eq!(cache.rows(&t, &Stream::new(), cx2), rows(&t, &Stream::new(), cx2));
    }

    #[test]
    fn arguments_are_readable() {
        assert_eq!(args_text(&Value::Null), "");
        assert_eq!(args_text(&json!({"scene": 3})), "{\n  \"scene\": 3\n}");
    }

    #[test]
    fn usage_and_cost() {
        let mut t = Thread::default();
        assert!(t.push_user("q"));
        text_turn(&mut t, "a", Usage { input: 15_000, output: 3_200, cost: Some(0.24), ..Usage::default() });
        assert_eq!(usage_line(&t, &Stream::new(), None), "18.2k tokens · $0.24");

        // Priced from the table when the provider reported no cost.
        let mut t2 = Thread::default();
        assert!(t2.push_user("q"));
        text_turn(&mut t2, "a", Usage { input: 1000, output: 500, ..Usage::default() });
        assert_eq!(usage_line(&t2, &Stream::new(), Some(0.5)), "1.5k tokens · $0.50");
        assert_eq!(usage_line(&t2, &Stream::new(), None), "1.5k tokens · cost unknown");

        // The request in flight counts toward the total.
        let mut s = Stream::new();
        s.event(Event::Usage(Usage { input: 1000, output: 100, cost: Some(0.01), ..Usage::default() }));
        assert_eq!(usage_line(&t, &s, None), "19.3k tokens · $0.25");

        // Nothing reported: unknown, whatever the table says.
        let mut t3 = Thread::default();
        assert!(t3.push_user("q"));
        t3.push_turn(asst(vec![Block::Text { text: "a".into() }], Usage::unknown(), StopReason::Cancelled), origin());
        assert_eq!(usage_line(&t3, &Stream::new(), Some(1.0)), "tokens unknown · cost unknown");
        assert_eq!(usage_line(&Thread::default(), &Stream::new(), None), "0 tokens · cost unknown");
    }
}
