//! OpenAI Chat Completions (`POST /chat/completions`, streamed), as spoken by
//! OpenAI and the many providers compatible with it.

use serde_json::{Value, json};

use serde_json::Map;

use super::{Call, checked_index, cut_off, is_sent, parse_args, stream_error};
use crate::sse::Frame;
use crate::thread::{Block, Event, Message, Role, StopReason, Thread, ToolCall, Turn, Usage};
use crate::{AgentError, Protocol};

/// Key of the native block holding OpenRouter's `reasoning_details` for one reply.
const REASONING_DETAILS: &str = "reasoning_details";

pub fn headers(api_key: &str) -> Vec<(String, String)> {
    let mut h = vec![("content-type".to_string(), "application/json".to_string())];
    if !api_key.is_empty() {
        h.push(("authorization".into(), format!("Bearer {api_key}")));
    }
    h
}

pub fn body(call: &Call, thread: &Thread) -> Value {
    let mut messages = vec![];
    if !call.system.is_empty() {
        messages.push(json!({ "role": "system", "content": call.system }));
    }
    let target = call.origin();
    for m in &thread.messages {
        // The same rule `Thread::check_sendable` checks.
        if !is_sent(Protocol::OpenaiChat, Some(&target), m) {
            continue;
        }
        match m.role {
            Role::User => {
                let text = m.text();
                if !text.trim().is_empty() {
                    messages.push(json!({ "role": "user", "content": text }));
                }
            }
            Role::Assistant => {
                // Provider replay data goes back only to the endpoint and model that wrote it.
                let same_origin = call.same_origin(m);
                let text = m.text();
                let calls: Vec<Value> = m
                    .tool_calls()
                    .iter()
                    .map(|c| {
                        // Arguments that didn't parse go back as `{}`: some compatible
                        // servers re-parse them and refuse invalid JSON.
                        let args = if c.input.is_object() { c.input.to_string() } else { "{}".into() };
                        let mut v = json!({ "id": c.id, "type": "function", "function": { "name": c.name, "arguments": args } });
                        if let Some(extra) = c.extra.as_ref().filter(|_| same_origin) {
                            v["extra_content"] = extra.clone();
                        }
                        v
                    })
                    .collect();
                // Whitespace-only text is no content (some servers refuse it).
                let has_text = !text.trim().is_empty();
                if !has_text && calls.is_empty() {
                    continue;
                }
                let mut msg =
                    json!({ "role": "assistant", "content": if has_text { json!(text) } else { Value::Null } });
                if !calls.is_empty() {
                    msg["tool_calls"] = json!(calls);
                }
                // OpenRouter wants the reasoning back unmodified next to the calls.
                let details = m.blocks.iter().find_map(|b| match b {
                    Block::Native { protocol: Protocol::OpenaiChat, value } if same_origin => {
                        value.get(REASONING_DETAILS)
                    }
                    _ => None,
                });
                if let Some(d) = details {
                    msg[REASONING_DETAILS] = d.clone();
                }
                messages.push(msg);
            }
            Role::Tool => {
                for b in &m.blocks {
                    if let Block::ToolResult(r) = b {
                        // No error flag in this protocol: say so in the content.
                        let content = if r.is_error { format!("Error: {}", r.content) } else { r.content.clone() };
                        messages.push(json!({ "role": "tool", "tool_call_id": r.id, "content": content }));
                    }
                }
                // A message from the writer after tool results goes as its own user message.
                let text = m.text();
                if !text.trim().is_empty() {
                    messages.push(json!({ "role": "user", "content": text }));
                }
            }
        }
    }
    let mut body = json!({
        "model": call.model,
        "stream": true,
        "stream_options": { "include_usage": true },
        "messages": messages,
    });
    if let Some(n) = call.max_tokens {
        body["max_completion_tokens"] = json!(n);
    }
    if let Some(effort) = call.effort.and_then(|e| e.openai()) {
        body["reasoning_effort"] = json!(effort);
    }
    if !call.tools.is_empty() {
        body["tools"] = json!(
            call.tools
                .iter()
                .map(|t| json!({ "type": "function", "function": { "name": t.name, "description": t.description, "parameters": t.schema } }))
                .collect::<Vec<_>>()
        );
    }
    body
}

#[derive(Debug, Default)]
struct PartialCall {
    /// The provider's `index` for it, when it sends one.
    index: Option<u64>,
    id: String,
    name: String,
    args: String,
    extra: Option<Value>,
}

/// What part of the reply the stream is on, for [`Event`] indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Segment {
    Text,
    Thinking,
    Tools,
}

#[derive(Debug, Default)]
pub struct Parser {
    text: String,
    thinking: String,
    refusal: String,
    reasoning_details: Vec<Value>,
    calls: Vec<PartialCall>,
    /// `None` until the usage chunk arrives (it comes last, after the finish reason).
    usage: Option<Usage>,
    stop: Option<StopReason>,
    done: bool,
    segment: Option<(Segment, usize)>,
}

impl Parser {
    /// The event index for a piece of `kind`: it moves on when the kind changes.
    fn index(&mut self, kind: Segment) -> usize {
        let i = match self.segment {
            Some((k, i)) if k == kind => i,
            Some((_, i)) => i + 1,
            None => 0,
        };
        self.segment = Some((kind, i));
        i
    }

    pub fn frame(&mut self, frame: &Frame) -> Result<Vec<Event>, AgentError> {
        if frame.data.trim() == "[DONE]" {
            self.done = true;
            return Ok(vec![]);
        }
        let v: Value =
            serde_json::from_str(&frame.data).map_err(|e| AgentError::Parse(format!("{e}: {}", frame.data)))?;
        if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
            return Err(stream_error(err));
        }
        let mut events = vec![];
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            let n = |p: &str| u.pointer(p).and_then(Value::as_u64);
            let input = n("/prompt_tokens").unwrap_or(0);
            let usage = Usage {
                input,
                output: n("/completion_tokens").unwrap_or(0),
                // Part of `input`: a server reporting more is wrong, and must
                // not make `input - cached` underflow downstream.
                cached: n("/prompt_tokens_details/cached_tokens").unwrap_or(0).min(input),
                reasoning: n("/completion_tokens_details/reasoning_tokens"),
                // OpenRouter reports what the request cost.
                cost: u.get("cost").and_then(Value::as_f64),
                ..Usage::default()
            };
            self.usage = Some(usage);
            events.push(Event::Usage(usage));
        }
        let Some(choice) = v.pointer("/choices/0") else { return Ok(events) };
        let delta = choice.get("delta").cloned().unwrap_or(Value::Null);
        if let Some(t) = delta.get("content").and_then(Value::as_str).filter(|t| !t.is_empty()) {
            self.text.push_str(t);
            events.push(Event::Text { index: self.index(Segment::Text), text: t.to_string() });
        }
        // OpenAI says why it declined as `refusal` text instead of content.
        if let Some(t) = delta.get("refusal").and_then(Value::as_str) {
            self.refusal.push_str(t);
        }
        for d in delta.get(REASONING_DETAILS).and_then(Value::as_array).into_iter().flatten() {
            merge_detail(&mut self.reasoning_details, d);
        }
        // Reasoning text, under the names various providers use. Shown, never sent back
        // (OpenRouter's replayable form is `reasoning_details`, above).
        let reasoning = delta.get("reasoning_content").or_else(|| delta.get("reasoning")).and_then(Value::as_str);
        if let Some(t) = reasoning.filter(|t| !t.is_empty()) {
            self.thinking.push_str(t);
            events.push(Event::Thinking { index: self.index(Segment::Thinking), text: t.to_string() });
        }
        for tc in delta.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
            self.index(Segment::Tools);
            let at = self.call_for(tc)?;
            let call = &mut self.calls[at];
            if let Some(id) = tc.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                call.id = id.to_string();
            }
            // The name comes whole; providers that repeat it on every piece must not double it.
            if let Some(name) = tc.pointer("/function/name").and_then(Value::as_str).filter(|s| !s.is_empty())
                && call.name.is_empty()
            {
                call.name = name.to_string();
                events.push(Event::ToolCallStarted { id: call.id.clone(), name: call.name.clone() });
            }
            if let Some(a) = tc.pointer("/function/arguments").and_then(Value::as_str).filter(|a| !a.is_empty()) {
                call.args.push_str(a);
                events.push(Event::ToolArgs { id: call.id.clone(), bytes_so_far: call.args.len() });
            }
            if let Some(e) = tc.get("extra_content").filter(|e| e.is_object()) {
                match &mut call.extra {
                    Some(old) => merge(old, e),
                    None => call.extra = Some(e.clone()),
                }
            }
        }
        if let Some(r) = choice.get("finish_reason").and_then(Value::as_str) {
            let stop = match r {
                "stop" => StopReason::EndTurn,
                "tool_calls" | "function_call" => StopReason::ToolUse,
                "length" => StopReason::MaxTokens,
                "content_filter" => StopReason::Refusal,
                other => StopReason::Other(other.to_string()),
            };
            events.push(Event::Stop(stop.clone()));
            self.stop = Some(stop);
        }
        Ok(events)
    }

    /// Which call a streamed piece belongs to, starting a new one if needed.
    fn call_for(&mut self, tc: &Value) -> Result<usize, AgentError> {
        let id = tc.get("id").and_then(Value::as_str).filter(|s| !s.is_empty());
        let named = tc.pointer("/function/name").and_then(Value::as_str).is_some_and(|s| !s.is_empty());
        let found = match tc.get("index").and_then(Value::as_u64) {
            // `index` says which call a piece belongs to, unless it brings a
            // different id than that call has: then it's a new call reusing the index.
            Some(i) => {
                checked_index(i)?;
                self.calls
                    .iter()
                    .rposition(|c| c.index == Some(i))
                    .filter(|&at| id.is_none_or(|id| self.calls[at].id.is_empty() || self.calls[at].id == id))
                    .ok_or(Some(i))
            }
            // No index (some providers send whole calls): an id we've seen continues that call.
            None if id.is_some() => self.calls.iter().position(|c| Some(c.id.as_str()) == id).ok_or(None),
            // No index and no id: a name starts a new call once the last one has its name.
            None if named && self.calls.last().is_some_and(|c| !c.name.is_empty()) => Err(None),
            None => self.calls.len().checked_sub(1).ok_or(None),
        };
        Ok(match found {
            Ok(at) => at,
            Err(index) => {
                self.calls.push(PartialCall { index, ..Default::default() });
                self.calls.len() - 1
            }
        })
    }

    pub fn finish(self) -> Result<Turn, AgentError> {
        if !self.done && self.stop.is_none() {
            return Err(cut_off());
        }
        let refusal = (!self.refusal.is_empty()).then_some(self.refusal);
        let mut blocks = vec![];
        if !self.reasoning_details.is_empty() {
            blocks.push(Block::Native {
                protocol: Protocol::OpenaiChat,
                value: json!({ REASONING_DETAILS: self.reasoning_details }),
            });
        }
        if !self.thinking.is_empty() {
            blocks.push(Block::Thinking { text: self.thinking, signature: None });
        }
        if !self.text.is_empty() {
            blocks.push(Block::Text { text: self.text });
        }
        for (i, c) in self.calls.into_iter().enumerate().filter(|(_, c)| !c.name.is_empty()) {
            let (input, input_error, raw_input) = parse_args(c.args);
            // Some compatible servers leave ids out; the protocol needs one to pair
            // results. `Thread::push_turn` makes these unique within the thread.
            let id = if c.id.is_empty() { format!("call_{i}") } else { c.id };
            blocks.push(Block::ToolCall(ToolCall { id, name: c.name, input, input_error, raw_input, extra: c.extra }));
        }
        // Refusal text means a refusal even when the finish reason says "stop".
        let stop = if refusal.is_some() { StopReason::Refusal } else { self.stop.unwrap_or(StopReason::EndTurn) };
        // A stream that ended without its usage chunk (some servers ignore
        // `include_usage`) is accepted, with the usage marked unknown.
        let usage = self.usage.unwrap_or_else(Usage::unknown);
        Ok(Turn { message: Message::assistant(blocks), stop, usage, refusal })
    }

    pub fn abort(self) -> Turn {
        let blocks = if self.text.is_empty() { vec![] } else { vec![Block::Text { text: self.text }] };
        Turn {
            message: Message::assistant(blocks),
            stop: StopReason::Cancelled,
            usage: Usage { known: false, ..self.usage.unwrap_or_default() },
            refusal: None,
        }
    }
}

/// Adds one streamed `reasoning_details` entry. Entries carrying an `index`
/// are pieces of one detail, the way tool calls stream: their text fields
/// are joined and their other fields filled in. Entries without one are kept
/// as they came.
fn merge_detail(details: &mut Vec<Value>, d: &Value) {
    let index = d.get("index").filter(|i| !i.is_null());
    let same = |old: &&mut Value| index.is_some() && old.get("index") == index && old.get("type") == d.get("type");
    let (Some(old), Some(new)) = (details.iter_mut().rfind(same).and_then(Value::as_object_mut), d.as_object()) else {
        details.push(d.clone());
        return;
    };
    for (k, v) in new {
        match (old.get_mut(k), v) {
            (Some(Value::String(a)), Value::String(b)) if matches!(k.as_str(), "text" | "summary" | "data") => {
                a.push_str(b)
            }
            (Some(old_v), _) if !old_v.is_null() && v.is_null() => {}
            _ => {
                old.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Merges object `b` into `a`, recursively; other values in `b` replace `a`'s.
fn merge(a: &mut Value, b: &Value) {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => merge_maps(a, b),
        (a, b) => *a = b.clone(),
    }
}

fn merge_maps(a: &mut Map<String, Value>, b: &Map<String, Value>) {
    for (k, v) in b {
        match a.get_mut(k) {
            Some(old) => merge(old, v),
            None => {
                a.insert(k.clone(), v.clone());
            }
        }
    }
}
