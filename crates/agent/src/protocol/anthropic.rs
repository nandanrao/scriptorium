//! Anthropic Messages API (`POST /v1/messages`, streamed).

use serde_json::{Value, json};

use super::{Call, Thinking, checked_index, cut_off, is_sent, parse_args, stream_error};
use crate::sse::Frame;
use crate::thread::{Block, Event, Message, Role, StopReason, Thread, ToolCall, Turn, Usage};
use crate::{AgentError, Protocol};

/// API version 2026-10-21: no sampling parameters (`temperature` etc.) may be sent.
const VERSION: &str = "2026-10-21";
/// The default output limit. Streaming, so a large limit doesn't risk a
/// timeout; long review passes need room.
const MAX_TOKENS: u64 = 64_000;

pub fn headers(api_key: &str) -> Vec<(String, String)> {
    vec![
        ("x-api-key".into(), api_key.into()),
        ("anthropic-version".into(), VERSION.into()),
        ("content-type".into(), "application/json".into()),
    ]
}

pub fn body(call: &Call, thread: &Thread) -> Value {
    let tools: Vec<Value> = call
        .tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.schema,
                // Large arguments (many suggestions) stream as they're written; we validate them ourselves.
                "eager_input_streaming": true,
            })
        })
        .collect();
    let mut body = json!({
        "model": call.model,
        "max_tokens": call.max_tokens.unwrap_or(MAX_TOKENS),
        "stream": true,
        // Caches everything up to the last block, so each turn of a long thread re-reads it cheaply.
        "cache_control": { "type": "ephemeral" },
        "messages": messages(call, thread),
    });
    if !call.system.is_empty() {
        body["system"] = json!(call.system);
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    match call.thinking {
        Thinking::Default => {}
        Thinking::Adaptive => body["thinking"] = json!({ "type": "adaptive", "display": "summarized" }),
    }
    if let Some(effort) = call.effort {
        body["output_config"] = json!({ "effort": effort.name() });
    }
    body
}

fn messages(call: &Call, thread: &Thread) -> Vec<Value> {
    let mut out: Vec<(&'static str, Vec<Value>)> = vec![];
    let target = call.origin();
    for m in &thread.messages {
        // The same rule `Thread::check_sendable` checks.
        if !is_sent(Protocol::Anthropic, Some(&target), m) {
            continue;
        }
        let role = match m.role {
            Role::Assistant => "assistant",
            Role::User | Role::Tool => "user",
        };
        let same_origin = call.same_origin(m);
        let content: Vec<Value> = m.blocks.iter().filter_map(|b| block(b, same_origin)).collect();
        if content.is_empty() {
            continue;
        }
        // Tool results followed by the writer's message are one user turn to the API.
        match out.last_mut() {
            Some((r, c)) if *r == role => c.extend(content),
            _ => out.push((role, content)),
        }
    }
    out.into_iter().map(|(role, content)| json!({ "role": role, "content": content })).collect()
}

/// One stored block as the API wants it, or `None` to leave it out.
/// `same_origin`: the message was written by this call's endpoint and model,
/// so its signed thinking and native blocks are still valid.
fn block(b: &Block, same_origin: bool) -> Option<Value> {
    Some(match b {
        // The API refuses empty (and whitespace-only) text blocks in every
        // role; only thinking blocks must go back unchanged.
        Block::Text { text } if text.trim().is_empty() => return None,
        Block::Text { text } => json!({ "type": "text", "text": text }),
        // Sent back unchanged, as the API requires, to the model that wrote it.
        // Unsigned thinking, or thinking from another endpoint or model, is dropped.
        Block::Thinking { text, signature: Some(sig) } if same_origin => {
            json!({ "type": "thinking", "thinking": text, "signature": sig })
        }
        Block::Thinking { .. } => return None,
        // Arguments that weren't valid JSON (kept in `raw_input`) can't go back
        // as they were: `input` must be an object. `{}` keeps the block in place.
        Block::ToolCall(c) => json!({
            "type": "tool_use", "id": tool_id(&c.id), "name": c.name,
            "input": if c.input.is_object() { c.input.clone() } else { json!({}) },
        }),
        Block::ToolResult(r) => json!({
            "type": "tool_result", "tool_use_id": tool_id(&r.id), "content": r.content, "is_error": r.is_error,
        }),
        Block::Native { protocol: Protocol::Anthropic, value } if same_origin => value.clone(),
        Block::Native { .. } | Block::Notice { .. } => return None,
    })
}

/// A tool call id the API accepts (`^[a-zA-Z0-9_-]+$`). Ids from other
/// providers may contain other characters (e.g. `functions.list:0`); those
/// get them replaced and a hash of the original appended, so the mapping is
/// deterministic (a call and its result map alike) and distinct ids stay distinct.
pub fn tool_id(id: &str) -> String {
    let ok = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    if !id.is_empty() && id.chars().all(ok) {
        return id.to_string();
    }
    let clean: String = id.chars().map(|c| if ok(c) { c } else { '_' }).collect();
    let hash = super::fnv1a(id.as_bytes());
    format!("{clean}_{hash:016x}")
}

#[derive(Debug)]
enum Partial {
    Text(String),
    Thinking { text: String, signature: String },
    Tool { id: String, name: String, json: String },
    Native(Value),
}

/// Anthropic's input counts: three disjoint parts.
#[derive(Debug, Default)]
struct RawInput {
    /// `input_tokens`: input neither read from nor written to the cache.
    uncached: u64,
    cache_read: u64,
    cache_write: u64,
}

#[derive(Debug, Default)]
pub struct Parser {
    blocks: Vec<Option<Partial>>,
    /// `None` until the stream reports any.
    usage: Option<Usage>,
    /// The input counts as the API last reported them.
    raw_usage: RawInput,
    stop: Option<StopReason>,
    refusal: Option<String>,
    done: bool,
}

impl Parser {
    pub fn frame(&mut self, frame: &Frame) -> Result<Vec<Event>, AgentError> {
        let v: Value =
            serde_json::from_str(&frame.data).map_err(|e| AgentError::Parse(format!("{e}: {}", frame.data)))?;
        let kind = v.get("type").and_then(Value::as_str).unwrap_or(&frame.event);
        let index = checked_index(v.get("index").and_then(Value::as_u64).unwrap_or(0))?;
        let mut events = vec![];
        match kind {
            "message_start" => {
                if let Some(u) = v.pointer("/message/usage") {
                    events.push(Event::Usage(self.read_usage(u)));
                }
            }
            "content_block_start" => {
                let b = v.get("content_block").cloned().unwrap_or(Value::Null);
                let s = |k: &str| b.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                let partial = match b.get("type").and_then(Value::as_str) {
                    Some("text") => Partial::Text(s("text")),
                    Some("thinking") => Partial::Thinking { text: s("thinking"), signature: s("signature") },
                    Some("tool_use") => {
                        events.push(Event::ToolCallStarted { id: s("id"), name: s("name") });
                        Partial::Tool { id: s("id"), name: s("name"), json: String::new() }
                    }
                    _ => Partial::Native(b),
                };
                if self.blocks.len() <= index {
                    self.blocks.resize_with(index + 1, || None);
                }
                self.blocks[index] = Some(partial);
            }
            "content_block_delta" => {
                let d = v.get("delta").cloned().unwrap_or(Value::Null);
                let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                match (self.blocks.get_mut(index).and_then(Option::as_mut), d.get("type").and_then(Value::as_str)) {
                    (Some(Partial::Text(t)), Some("text_delta")) => {
                        t.push_str(&s("text"));
                        events.push(Event::Text { index, text: s("text") });
                    }
                    (Some(Partial::Thinking { text, .. }), Some("thinking_delta")) => {
                        let t = s("thinking");
                        if !t.is_empty() {
                            text.push_str(&t);
                            events.push(Event::Thinking { index, text: t });
                        }
                    }
                    (Some(Partial::Thinking { signature, .. }), Some("signature_delta")) => {
                        signature.push_str(&s("signature"))
                    }
                    (Some(Partial::Tool { id, json, .. }), Some("input_json_delta")) => {
                        json.push_str(&s("partial_json"));
                        events.push(Event::ToolArgs { id: id.clone(), bytes_so_far: json.len() });
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(r) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    let stop = stop_reason(r);
                    events.push(Event::Stop(stop.clone()));
                    self.stop = Some(stop);
                }
                if let Some(e) = v.pointer("/delta/stop_details/explanation").and_then(Value::as_str) {
                    self.refusal = Some(e.to_string());
                }
                if let Some(u) = v.get("usage") {
                    events.push(Event::Usage(self.read_usage(u)));
                }
            }
            "message_stop" => self.done = true,
            "error" => return Err(stream_error(v.get("error").unwrap_or(&Value::Null))),
            // ping, content_block_stop, and event types added later.
            _ => {}
        }
        Ok(events)
    }

    /// Usage arrives in pieces: input at the start, output (cumulative) at the
    /// end, and any piece may repeat or leave out any count. The raw counts
    /// are kept as last reported and `input` recomputed from them after every
    /// piece, so `cached` and `cache_write` never exceed it.
    fn read_usage(&mut self, u: &Value) -> Usage {
        let n = |k: &str| u.get(k).and_then(Value::as_u64);
        let raw = &mut self.raw_usage;
        // The API counts cached input separately from `input_tokens`; ours includes it.
        if let Some(v) = n("input_tokens") {
            raw.uncached = v;
        }
        if let Some(v) = n("cache_read_input_tokens") {
            raw.cache_read = v;
        }
        if let Some(v) = n("cache_creation_input_tokens") {
            raw.cache_write = v;
        }
        let usage = self.usage.get_or_insert_with(Usage::default);
        usage.cached = raw.cache_read;
        usage.cache_write = raw.cache_write;
        usage.input = raw.uncached.saturating_add(raw.cache_read).saturating_add(raw.cache_write);
        if let Some(o) = n("output_tokens") {
            usage.output = o;
        }
        *usage
    }

    pub fn finish(self) -> Result<Turn, AgentError> {
        // `message_stop` is the only proof the reply is complete.
        if !self.done {
            return Err(cut_off());
        }
        let blocks = self
            .blocks
            .into_iter()
            .flatten()
            .map(|p| match p {
                // Empty text blocks are kept in storage (requests leave them out).
                Partial::Text(text) => Block::Text { text },
                Partial::Thinking { text, signature } => {
                    Block::Thinking { text, signature: (!signature.is_empty()).then_some(signature) }
                }
                Partial::Tool { id, name, json } => {
                    let (input, input_error, raw_input) = parse_args(json);
                    Block::ToolCall(ToolCall { id, name, input, input_error, raw_input, extra: None })
                }
                Partial::Native(value) => Block::Native { protocol: Protocol::Anthropic, value },
            })
            .collect();
        Ok(Turn {
            message: Message::assistant(blocks),
            stop: self.stop.unwrap_or(StopReason::EndTurn),
            usage: self.usage.unwrap_or_else(Usage::unknown),
            refusal: self.refusal,
        })
    }

    pub fn abort(self) -> Turn {
        let blocks = self
            .blocks
            .into_iter()
            .flatten()
            .filter_map(|p| match p {
                Partial::Text(text) if !text.is_empty() => Some(Block::Text { text }),
                _ => None,
            })
            .collect();
        Turn {
            message: Message::assistant(blocks),
            stop: StopReason::Cancelled,
            usage: Usage { known: false, ..self.usage.unwrap_or_default() },
            refusal: None,
        }
    }
}

fn stop_reason(r: &str) -> StopReason {
    match r {
        "end_turn" | "stop_sequence" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "refusal" => StopReason::Refusal,
        other => StopReason::Other(other.to_string()),
    }
}
