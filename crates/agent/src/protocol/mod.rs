//! Wire protocols: building requests and reading streamed replies.
//!
//! Two protocols cover the providers v1 supports: Anthropic Messages, and
//! OpenAI Chat Completions (OpenAI, Google Gemini's OpenAI endpoint,
//! OpenRouter, Fireworks, local servers…).

mod anthropic;
mod openai;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::sse::{Decoder, Frame};
use crate::thread::{Block, Event, Message, Origin, Role, Thread, Turn};
use crate::{AgentError, ErrorKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Anthropic,
    OpenaiChat,
}

impl Protocol {
    pub fn name(self) -> &'static str {
        match self {
            Protocol::Anthropic => "Anthropic",
            Protocol::OpenaiChat => "OpenAI-compatible",
        }
    }
}

/// A tool the model may call: its name, description and argument schema.
/// Serializes as `{name, description, schema}` (what [`crate::file::tools_hash`] hashes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments (an object).
    pub schema: Value,
}

/// An HTTP request for the caller to send. `body` is `Null` for a GET.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

/// Whether to ask for the model's reasoning. Anthropic only for now; the
/// OpenAI protocol ignores it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Thinking {
    /// Send nothing: the model's own default.
    #[default]
    Default,
    /// `thinking: {type: "adaptive", display: "summarized"}`: the model decides
    /// how much to think, and the summary streams so the writer sees progress.
    ///
    /// Accepted by Claude Opus 4.6+, Sonnet 4.6+, Haiku 5.5 and Fable; Haiku 4.5
    /// and older models reject it, so the caller picks this per model.
    /// (`disabled` and `budget_tokens` are never sent: current models reject them.)
    Adaptive,
}

/// How hard the model should work.
///
/// Anthropic: `output_config.effort`. Accepted by Claude Opus 4.6+, Sonnet
/// 4.6+, Haiku 5.5 and Fable (Opus 4.5 takes `low`/`medium`/`high`); Haiku 4.5
/// and older reject it.
/// OpenAI protocol: `reasoning_effort`, for `Low`/`Medium`/`High` only (the
/// others are not sent). Only reasoning models accept it.
///
/// Which models accept what is the caller's decision (an account setting);
/// the presets don't track model capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
}

impl Effort {
    /// Anthropic's name for it.
    pub fn name(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }

    /// OpenAI's `reasoning_effort`, if it has this level.
    fn openai(self) -> Option<&'static str> {
        match self {
            Effort::Low | Effort::Medium | Effort::High => Some(self.name()),
            Effort::XHigh | Effort::Max => None,
        }
    }
}

/// What a turn needs besides the thread.
#[derive(Debug, Clone, Copy, Default)]
pub struct Call<'a> {
    /// e.g. `https://api.anthropic.com/v1` (a trailing slash is ignored).
    /// With `model`, it is the [`Origin`] replies are stored with.
    pub base_url: &'a str,
    /// Empty for servers that need none (local models).
    pub api_key: &'a str,
    pub model: &'a str,
    pub system: &'a str,
    pub tools: &'a [ToolSpec],
    /// Output limit. Anthropic requires one and defaults to 64000; the OpenAI
    /// protocol sends `max_completion_tokens` only when set.
    pub max_tokens: Option<u64>,
    /// See [`Effort`]; `None` for the model's default.
    pub effort: Option<Effort>,
    /// Anthropic thinking; see [`Thinking`] for which models accept what.
    pub thinking: Thinking,
    /// Added to the request's headers (e.g. OpenRouter's `HTTP-Referer`).
    pub extra_headers: &'a [(String, String)],
}

impl Call<'_> {
    /// The origin a reply to this call is stored with (pass it to
    /// [`Thread::push_turn`](crate::Thread::push_turn)).
    pub fn origin(&self) -> Origin {
        Origin::new(self.base_url, self.model)
    }

    /// Whether `message`'s replay data may be sent with this call.
    fn same_origin(&self, message: &Message) -> bool {
        message.origin.as_ref().is_some_and(|o| *o == self.origin())
    }
}

/// Whether a request in `protocol` to `target` (the endpoint and model it
/// goes to, [`Call::origin`]) includes `message`. `None` stands for a target
/// that wrote none of the thread, so no replay data counts.
///
/// The one rule both request builders and [`Thread::check_sendable`]
/// follow, so what is checked is what is sent. A message is left out when it
/// is display-only, or when nothing in it would go to this target:
/// - writer's messages: no text but whitespace (only text counts: a tool
///   result or call there is in the wrong role, which
///   [`Thread::check_sendable`] refuses when the message is sent);
/// - tool messages: no text but whitespace and no tool results;
/// - assistant messages: no text but whitespace, no tool calls, and no block
///   replayable to this target (signed thinking and Anthropic native blocks
///   go only to the Anthropic origin that wrote them; OpenRouter's
///   `reasoning_details` only travel alongside text or calls). For example a
///   reply that is only unsigned thinking, or thinking from another model.
///
/// A message left out is display-only for that request.
pub fn is_sent(protocol: Protocol, target: Option<&Origin>, message: &Message) -> bool {
    if message.is_display_only() {
        return false;
    }
    let has_text = !message.text().trim().is_empty();
    match message.role {
        Role::User => has_text,
        Role::Tool => has_text || !message.tool_results().is_empty(),
        Role::Assistant => {
            let same_origin = target.is_some_and(|t| message.origin.as_ref() == Some(t));
            has_text || message.blocks.iter().any(|b| replayed_alone(protocol, same_origin, b))
        }
    }
}

/// Whether an assistant's block is sent on its own (not only alongside text or calls).
fn replayed_alone(protocol: Protocol, same_origin: bool, b: &Block) -> bool {
    match b {
        Block::ToolCall(_) => true,
        Block::Thinking { signature: Some(_), .. } | Block::Native { protocol: Protocol::Anthropic, .. } => {
            same_origin && protocol == Protocol::Anthropic
        }
        Block::Text { .. }
        | Block::Thinking { signature: None, .. }
        | Block::Native { protocol: Protocol::OpenaiChat, .. }
        | Block::ToolResult(_)
        | Block::Notice { .. } => false,
    }
}

/// Provider-supplied block and call indexes above this are refused rather
/// than allocated for: no real reply comes near it.
const MAX_INDEX: u64 = 1024;

fn checked_index(i: u64) -> Result<usize, AgentError> {
    if i > MAX_INDEX {
        return Err(AgentError::Parse(format!("index {i} is out of range")));
    }
    Ok(i as usize)
}

/// The streaming request for the thread's next turn.
///
/// Contract for callers:
/// - Keep `call.system` and `call.tools` byte-identical for the whole thread
///   (store the system prompt with it: [`crate::file::ThreadFile`]).
///   Anthropic checks that a replayed thinking block still follows the same
///   prefix it was produced after (and the prompt cache keys on it); changing
///   either mid-thread silently drops the model's earlier reasoning.
/// - Only send a thread that passes [`Thread::check_sendable`] for this
///   protocol and [`Call::origin`]. This returns [`AgentError::NotReady`]
///   rather than build a request the provider would refuse.
/// - Store the reply with [`Call::origin`]: signatures, encrypted reasoning
///   and other replay data go back only to the endpoint and model that wrote them.
///
/// Switching endpoint or model in the middle of a tool loop (between a reply
/// with tool calls and the next turn) drops that reply's signed thinking,
/// since it may only go back to its own [`Origin`]. With adaptive thinking on,
/// some Anthropic API versions reject a `tool_use` turn sent without its
/// thinking while the loop is still active. Switch between runs, not inside one.
pub fn build_request(protocol: Protocol, call: &Call, thread: &Thread) -> Result<Request, AgentError> {
    thread.check_sendable(protocol, &call.origin())?;
    let base = call.base_url.trim_end_matches('/');
    let mut request = match protocol {
        Protocol::Anthropic => Request {
            url: format!("{base}/messages"),
            headers: anthropic::headers(call.api_key),
            body: anthropic::body(call, thread),
        },
        Protocol::OpenaiChat => Request {
            url: format!("{base}/chat/completions"),
            headers: openai::headers(call.api_key),
            body: openai::body(call, thread),
        },
    };
    request.headers.extend(call.extra_headers.iter().cloned());
    Ok(request)
}

/// Lists the models an account can use.
pub fn models_request(protocol: Protocol, base_url: &str, api_key: &str) -> Request {
    let base = base_url.trim_end_matches('/');
    let headers = match protocol {
        Protocol::Anthropic => anthropic::headers(api_key),
        Protocol::OpenaiChat => openai::headers(api_key),
    };
    Request { url: format!("{base}/models"), headers, body: Value::Null }
}

/// Model ids from a `models_request` reply. Both protocols answer `{"data": [{"id": …}, …]}`.
pub fn parse_models(body: &str) -> Result<Vec<String>, AgentError> {
    let v: Value = serde_json::from_str(body).map_err(|e| AgentError::Parse(e.to_string()))?;
    let list = v.get("data").and_then(Value::as_array).ok_or_else(|| AgentError::Parse("no model list".into()))?;
    let mut ids: Vec<String> =
        list.iter().filter_map(|m| m.get("id").and_then(Value::as_str)).map(String::from).collect();
    ids.sort();
    Ok(ids)
}

/// The error for a failed HTTP request, from its status, the provider's
/// error body if it has one, and its `retry-after` header if it sent one
/// (seconds; the HTTP-date form is not read, as this has no clock).
pub fn api_error(status: u16, body: &str, retry_after: Option<&str>) -> AgentError {
    let v = serde_json::from_str::<Value>(body).ok().map(|v| {
        // Anthropic/OpenAI: {"error": {"message", "type"}}; Gemini: [{"error": …}]; some: {"message"}.
        v.as_array().and_then(|a| a.first().cloned()).unwrap_or(v)
    });
    let detail = v.as_ref().and_then(|v| {
        v.pointer("/error/message").or_else(|| v.get("message")).and_then(Value::as_str).map(String::from)
    });
    let what = match status {
        401 | 403 => "the API key was refused",
        404 => "not found (check the model name and the address)",
        429 => "rate limited or out of credit",
        500..=599 => "the provider had a problem",
        _ => "request failed",
    };
    let message = match detail {
        Some(d) => format!("{what} ({status}): {d}"),
        None => format!("{what} ({status})"),
    };
    let error = v.as_ref().and_then(|v| v.get("error")).cloned().unwrap_or(Value::Null);
    let mut e = classify(Some(status), &error, message);
    if let AgentError::Api { retry_after: r, .. } = &mut e {
        *r = retry_after.and_then(parse_retry_after);
    }
    e
}

/// Whole seconds from a `retry-after` value (`"30"`, or `"1.5"` rounded up).
fn parse_retry_after(v: &str) -> Option<u64> {
    let v = v.trim();
    if let Ok(n) = v.parse::<u64>() {
        return Some(n);
    }
    let f = v.parse::<f64>().ok().filter(|f| f.is_finite() && *f >= 0.0)?;
    Some(f.ceil() as u64)
}

/// FNV-1a: a hash stable across builds and platforms, unlike std's hasher.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

/// An error object that arrived inside a reply stream.
fn stream_error(error: &Value) -> AgentError {
    let message = error.get("message").and_then(Value::as_str).map(String::from).unwrap_or_else(|| error.to_string());
    classify(None, error, message)
}

/// Kind and retryability, from the provider's error type when it gives one
/// (`error.type`, `error.code` or `error.status`), else from the HTTP status.
fn classify(status: Option<u16>, error: &Value, message: String) -> AgentError {
    let names: Vec<String> = ["type", "code", "status"]
        .iter()
        .filter_map(|k| error.get(*k).and_then(Value::as_str))
        .map(str::to_ascii_lowercase)
        .collect();
    let has = |n: &str| names.iter().any(|x| x == n);
    // Gemini puts the HTTP status in `error.code` mid-stream.
    let status_like = status.or_else(|| error.get("code").and_then(Value::as_u64).map(|c| c as u16));
    let by_name = if has("overloaded_error") || has("overloaded") {
        Some(ErrorKind::Overloaded)
    } else if has("rate_limit_error")
        || has("rate_limit_exceeded")
        || has("resource_exhausted")
        || has("insufficient_quota")
    {
        Some(ErrorKind::RateLimit)
    } else if ["authentication_error", "permission_error", "invalid_api_key", "unauthenticated", "permission_denied"]
        .iter()
        .any(|n| has(n))
    {
        Some(ErrorKind::Auth)
    } else if ["invalid_request_error", "not_found_error", "request_too_large", "invalid_argument", "model_not_found"]
        .iter()
        .any(|n| has(n))
    {
        Some(ErrorKind::BadRequest)
    } else if has("api_error") || has("server_error") {
        Some(ErrorKind::Other)
    } else {
        None
    };
    let kind = by_name.unwrap_or(match status_like {
        Some(401 | 403) => ErrorKind::Auth,
        Some(429) => ErrorKind::RateLimit,
        Some(503 | 529) => ErrorKind::Overloaded,
        Some(408) => ErrorKind::Other,
        Some(400..=499) => ErrorKind::BadRequest,
        _ => ErrorKind::Other,
    });
    let retryable = match kind {
        // Out of credit won't fix itself by waiting.
        ErrorKind::RateLimit => !has("insufficient_quota"),
        ErrorKind::Overloaded | ErrorKind::Offline => true,
        ErrorKind::Other => status_like.is_none_or(|s| s >= 500 || s == 408),
        ErrorKind::Auth | ErrorKind::BadRequest => false,
    };
    AgentError::Api { status, kind, retryable, message, retry_after: None }
}

/// The error for a stream that ended before the reply was complete.
fn cut_off() -> AgentError {
    AgentError::offline("the reply was cut off (connection closed early)")
}

/// Reads one streamed reply. Feed it the response body line by line.
#[derive(Debug)]
pub struct ReplyParser {
    sse: Decoder,
    inner: Inner,
}

#[derive(Debug)]
enum Inner {
    Anthropic(anthropic::Parser),
    OpenAi(openai::Parser),
}

impl ReplyParser {
    pub fn new(protocol: Protocol) -> Self {
        let inner = match protocol {
            Protocol::Anthropic => Inner::Anthropic(anthropic::Parser::default()),
            Protocol::OpenaiChat => Inner::OpenAi(openai::Parser::default()),
        };
        ReplyParser { sse: Decoder::default(), inner }
    }

    /// One line of the body. Returns what arrived, for showing progress.
    pub fn line(&mut self, line: &str) -> Result<Vec<Event>, AgentError> {
        match self.sse.line(line) {
            Some(frame) => self.frame(&frame),
            None => Ok(vec![]),
        }
    }

    /// The finished reply, once the body has ended.
    pub fn finish(mut self) -> Result<Turn, AgentError> {
        if let Some(frame) = self.sse.finish() {
            self.frame(&frame)?;
        }
        match self.inner {
            Inner::Anthropic(p) => p.finish(),
            Inner::OpenAi(p) => p.finish(),
        }
    }

    /// The writer stopped the reply: what streamed so far, text only (no
    /// thinking, no tool calls, so nothing half-done is replayed or run), with
    /// [`StopReason::Cancelled`](crate::StopReason::Cancelled) and the usage
    /// marked unknown. Keeps what the writer saw; store it with
    /// [`Thread::push_turn`].
    pub fn abort(self) -> Turn {
        match self.inner {
            Inner::Anthropic(p) => p.abort(),
            Inner::OpenAi(p) => p.abort(),
        }
    }

    fn frame(&mut self, frame: &Frame) -> Result<Vec<Event>, AgentError> {
        match &mut self.inner {
            Inner::Anthropic(p) => p.frame(frame),
            Inner::OpenAi(p) => p.frame(frame),
        }
    }
}

/// A tool call's accumulated argument text, parsed: `(input, input_error, raw_input)`.
/// The raw text is kept only when it didn't parse to an object.
fn parse_args(raw: String) -> (Value, Option<String>, Option<String>) {
    if raw.trim().is_empty() {
        return (Value::Object(Default::default()), None, None);
    }
    let error = match serde_json::from_str::<Value>(&raw) {
        Ok(v @ Value::Object(_)) => return (v, None, None),
        Ok(_) => "the arguments weren't a JSON object".to_string(),
        Err(e) => format!("the arguments weren't valid JSON ({e}); they may have been cut off"),
    };
    (Value::Null, Some(error), Some(raw))
}
