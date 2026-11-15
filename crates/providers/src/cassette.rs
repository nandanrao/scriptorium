//! Replay cassettes: recorded provider exchanges, for tests and fixtures.
//!
//! A cassette is a sequence of JSON objects, one [`Exchange`] per request, in
//! order. An exchange may span several lines:
//!
//! ```text
//! // A tool round.
//! {"expect": {"path": "/messages", "model": "claude-opus-5-5", "last_user": "tighten"},
//!  "delay_ms": 20,
//!  "lines": [
//!   "event: message_start",
//!   {"type": "message_start", "message": {"usage": {"input_tokens": 12}}},
//!   ""
//!  ]}
//! {"expect": {"results": [{"id": "toolu_1", "contains": "Added 8"}]}, "status": 529, "retry_after": 1, "body": "{...}"}
//! {"offline": "connection reset"}
//! ```
//!
//! - `expect` (optional) checks the request: `path` is a suffix of the URL's
//!   path, `model` equals the body's model, `last_user` is a substring of the
//!   last user message's text, and each of `results` names a tool result by
//!   call id whose content contains `contains`. So a test checks what the
//!   model was told.
//! - `status` (default 200). A success streams `lines`, waiting `delay_ms`
//!   before each. A line is a string, or a JSON object, which streams as
//!   `data: <the object as compact JSON>` (its keys possibly reordered:
//!   cassettes that need exact bytes use strings). An error status answers `body` (a
//!   string, or JSON written inline) and `retry_after` (seconds, number or
//!   string).
//! - `offline` alone: the connection fails before any reply. With `lines`:
//!   they stream, then the connection is cut off.
//! - `bad_line` (with or without `lines`): after the lines, a line that isn't
//!   text arrives (a parse error, as live).
//!
//! Lines starting with `//` are comments. This module is pure: it parses,
//! checks and writes exchanges; [`Replay`](crate::shell::Replay) and the
//! recorder in [`Http`](crate::shell::Http) do the I/O.
//!
//! Recorded cassettes hold what was sent and received: the start of the
//! writer's messages and of tool results (script text), and replies and
//! error bodies in full. Never headers, so never a key.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use scriptorium_agent::Request;

use crate::failure::Failure;

/// One request and the reply to give it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exchange {
    #[serde(default, skip_serializing_if = "Expect::is_empty")]
    pub expect: Expect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub delay_ms: u64,
    /// The body's lines, without line endings.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "lines")]
    pub lines: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "text_or_json")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "text_or_json")]
    pub retry_after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bad_line: Option<String>,
}

/// What a request must look like for an exchange to answer it. Empty
/// matches anything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_user: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<ResultExpect>,
}

/// A tool result the request must carry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultExpect {
    /// The tool call's id.
    pub id: String,
    /// A substring of the result's content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
}

/// What a transport does with an exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply<'a> {
    /// Stream these lines, then end normally (`then` is `None`) or fail.
    Lines { lines: &'a [String], then: Option<Failure> },
    /// Fail without streaming anything.
    Fail(Failure),
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// A string as itself; any other JSON value as its JSON text.
fn text_or_json<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Option::<Value>::deserialize(d)? {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s),
        Some(v) => Some(v.to_string()),
    })
}

/// Lines as strings, or objects that stand for `data: <compact JSON>`.
fn lines<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    Vec::<Value>::deserialize(d)?
        .into_iter()
        .map(|v| match v {
            Value::String(s) => Ok(s),
            Value::Object(_) => Ok(format!("data: {v}")),
            other => Err(serde::de::Error::custom(format!("a line is a string or an object, not {other}"))),
        })
        .collect()
}

/// The exchanges in a cassette's text. Errors name the exchange and the
/// line it starts on.
pub fn parse(text: &str) -> Result<Vec<Exchange>, String> {
    // Comments become blank lines, so line numbers stay right.
    let clean: String = text
        .split_inclusive('\n')
        .map(|l| if l.trim_start().starts_with("//") { if l.ends_with('\n') { "\n" } else { "" } } else { l })
        .collect();
    let line_at = |offset: usize| clean[..offset].matches('\n').count() + 1;
    let mut stream = serde_json::Deserializer::from_str(&clean).into_iter::<Value>();
    let mut out = vec![];
    loop {
        let before = stream.byte_offset();
        let start = before + clean[before..].len() - clean[before..].trim_start().len();
        let Some(next) = stream.next() else { break };
        let n = out.len() + 1;
        let at = |m: String| format!("exchange {n} (line {}): {m}", line_at(start));
        let v = next.map_err(|e| format!("exchange {n}: line {}, column {}: {e}", e.line(), e.column()))?;
        let e: Exchange = serde_json::from_value(v).map_err(|e| at(e.to_string()))?;
        e.check().map_err(at)?;
        out.push(e);
    }
    Ok(out)
}

impl Exchange {
    /// The status the reply has (200 unless it says).
    pub fn status(&self) -> u16 {
        self.status.unwrap_or(200)
    }

    fn check(&self) -> Result<(), String> {
        let ok = (200..300).contains(&self.status());
        if !ok && !self.lines.is_empty() {
            return Err(format!("status {} with \"lines\" (an error status answers \"body\")", self.status()));
        }
        if ok && self.body.is_some() {
            return Err("\"body\" on a success (a success streams \"lines\")".into());
        }
        if !ok && (self.offline.is_some() || self.bad_line.is_some()) {
            return Err("\"offline\" or \"bad_line\" with an error status".into());
        }
        if self.offline.is_some() && self.bad_line.is_some() {
            return Err("both \"offline\" and \"bad_line\"".into());
        }
        Ok(())
    }

    /// What to answer.
    pub fn reply(&self) -> Reply<'_> {
        let status = self.status();
        if !(200..300).contains(&status) {
            return Reply::Fail(Failure::Status {
                status,
                retry_after: self.retry_after.clone(),
                body: self.body.clone().unwrap_or_default(),
            });
        }
        let lines = &self.lines[..];
        match (&self.offline, &self.bad_line) {
            (Some(m), _) if lines.is_empty() => Reply::Fail(Failure::Offline(m.clone())),
            (Some(m), _) => Reply::Lines { lines, then: Some(Failure::CutOff(m.clone())) },
            (None, Some(m)) => Reply::Lines { lines, then: Some(Failure::BadLine(m.clone())) },
            (None, None) => Reply::Lines { lines, then: None },
        }
    }

    /// An exchange to record `req` under: its `expect` describes the request
    /// (path, model, the start of the last user text and of each new tool
    /// result) and holds no headers, so no key. The caller fills in the reply.
    pub fn recording(req: &Request) -> Exchange {
        Exchange { expect: Expect::describe(req), ..Exchange::default() }
    }

    /// The exchange as cassette text, for reading and editing: one field per
    /// line, one body line per line, `data:` lines holding a JSON object
    /// written as the object. Ends with a newline.
    pub fn to_text(&self) -> String {
        let mut fields = vec![];
        if !self.expect.is_empty() {
            fields.push(format!("\"expect\": {}", json(&self.expect)));
        }
        if let Some(s) = self.status {
            fields.push(format!("\"status\": {s}"));
        }
        if self.delay_ms != 0 {
            fields.push(format!("\"delay_ms\": {}", self.delay_ms));
        }
        for (k, v) in [("retry_after", &self.retry_after), ("offline", &self.offline), ("bad_line", &self.bad_line)] {
            if let Some(v) = v {
                fields.push(format!("\"{k}\": {}", json(v)));
            }
        }
        if let Some(b) = &self.body {
            // An error body that is JSON is written as JSON.
            let body = serde_json::from_str::<Value>(b)
                .ok()
                .filter(|v| v.is_object() && json(v) == *b)
                .map_or_else(|| json(b), |v| v.to_string());
            fields.push(format!("\"body\": {body}"));
        }
        if !self.lines.is_empty() {
            let items: Vec<String> = self.lines.iter().map(|l| format!("  {}", line_text(l))).collect();
            fields.push(format!("\"lines\": [\n{}\n ]", items.join(",\n")));
        }
        format!("{{{}}}\n", fields.join(",\n "))
    }
}

/// A body line as cassette JSON: `data: {object}` as the object when that
/// replays as the same text, else a string.
fn line_text(line: &str) -> String {
    if let Some(payload) = line.strip_prefix("data: ")
        && let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(payload)
        && json(&v) == payload
    {
        return v.to_string();
    }
    Value::String(line.to_string()).to_string()
}

/// Compact JSON.
fn json<T: Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string(v).expect("cassette values always serialize")
}

impl Expect {
    pub fn is_empty(&self) -> bool {
        self == &Expect::default()
    }

    /// Whether `req` fits. The error says which part doesn't, with what
    /// the request had.
    pub fn check(&self, req: &Request) -> Result<(), String> {
        if let Some(p) = &self.path {
            let actual = url_path(&req.url);
            if !actual.ends_with(p.as_str()) {
                return Err(format!("expected a path ending \"{p}\", got \"{actual}\""));
            }
        }
        if let Some(m) = &self.model {
            let actual = req.body.get("model").and_then(Value::as_str).unwrap_or("");
            if actual != m {
                return Err(format!("expected model \"{m}\", got \"{actual}\""));
            }
        }
        if let Some(u) = &self.last_user {
            let actual = last_user_text(&req.body).unwrap_or_default();
            if !actual.contains(u.as_str()) {
                return Err(format!("expected the last user message to contain \"{u}\", got \"{actual}\""));
            }
        }
        let results = tool_results(&req.body, false);
        for r in &self.results {
            let Some((_, content)) = results.iter().rev().find(|(id, _)| *id == r.id) else {
                let ids: Vec<&str> = results.iter().map(|(id, _)| id.as_str()).collect();
                return Err(format!("expected a tool result for \"{}\"; the request has {ids:?}", r.id));
            };
            if let Some(c) = &r.contains
                && !content.contains(c.as_str())
            {
                return Err(format!("expected the result for \"{}\" to contain \"{c}\", got \"{content}\"", r.id));
            }
        }
        Ok(())
    }

    /// An expectation `req` meets, for recording.
    pub fn describe(req: &Request) -> Expect {
        let model = req.body.get("model").and_then(Value::as_str).map(String::from);
        Expect {
            path: Some(url_path(&req.url).to_string()),
            model,
            last_user: last_user_text(&req.body).map(|t| prefix(t.trim(), 60)).filter(|t| !t.is_empty()),
            results: tool_results(&req.body, true)
                .into_iter()
                .map(|(id, content)| {
                    let c = prefix(content.trim(), 40);
                    ResultExpect { id, contains: (!c.is_empty()).then_some(c) }
                })
                .collect(),
        }
    }
}

/// The first `n` characters.
fn prefix(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// The path of a URL, without scheme, host or query.
fn url_path(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let path = rest.find('/').map_or("", |i| &rest[i..]);
    path.split(['?', '#']).next().unwrap_or("")
}

/// A message's text: a string, or the `text` of its text parts.
fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str).is_none_or(|t| t == "text"))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn messages(body: &Value) -> &[Value] {
    body.get("messages").and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// The text of the last user message that has any (Anthropic sends tool
/// results as user messages without text; they're skipped).
fn last_user_text(body: &Value) -> Option<String> {
    messages(body)
        .iter()
        .rev()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        .map(|m| text_of(m.get("content").unwrap_or(&Value::Null)))
        .find(|t| !t.is_empty())
}

/// Tool results as (call id, content) in both protocols' shapes; with
/// `trailing`, only those after the last assistant message (the new ones).
fn tool_results(body: &Value, trailing: bool) -> Vec<(String, String)> {
    let all = messages(body);
    let start = if trailing {
        all.iter().rposition(|m| m.get("role").and_then(Value::as_str) == Some("assistant")).map_or(0, |i| i + 1)
    } else {
        0
    };
    let mut out = vec![];
    for m in &all[start..] {
        let content = m.get("content").unwrap_or(&Value::Null);
        match m.get("role").and_then(Value::as_str) {
            // OpenAI: {"role": "tool", "tool_call_id", "content"}.
            Some("tool") => {
                if let Some(id) = m.get("tool_call_id").and_then(Value::as_str) {
                    out.push((id.to_string(), text_of(content)));
                }
            }
            // Anthropic: user content blocks {"type": "tool_result", "tool_use_id", "content"}.
            Some("user") => {
                for b in content.as_array().map_or(&[][..], Vec::as_slice) {
                    if b.get("type").and_then(Value::as_str) == Some("tool_result")
                        && let Some(id) = b.get("tool_use_id").and_then(Value::as_str)
                    {
                        out.push((id.to_string(), text_of(b.get("content").unwrap_or(&Value::Null))));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn req(url: &str, body: Value) -> Request {
        Request { url: url.into(), headers: vec![("x-api-key".into(), "secret".into())], body }
    }

    fn anthropic() -> Request {
        req(
            "https://api.anthropic.com/v1/messages",
            json!({
                "model": "claude-opus-5-5",
                "messages": [
                    {"role": "user", "content": [{"type": "text", "text": "Please tighten scene 3."}]},
                    {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_1", "name": "read_scene", "input": {}}]},
                    {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": "toolu_1", "content": "INT. KITCHEN - NIGHT\nAdded 8 lines", "is_error": false}
                    ]}
                ]
            }),
        )
    }

    fn openai() -> Request {
        req(
            "http://localhost:11434/v1/chat/completions?x=1",
            json!({
                "model": "gpt-6",
                "messages": [
                    {"role": "system", "content": "You help."},
                    {"role": "user", "content": "hello there"},
                    {"role": "assistant", "content": null, "tool_calls": [{"id": "call_9"}]},
                    {"role": "tool", "tool_call_id": "call_9", "content": "Added 8 suggestions"}
                ]
            }),
        )
    }

    #[test]
    fn parses_the_documented_shapes() {
        let text = r#"
// a comment
{"expect": {"path": "/messages", "model": "claude-opus-5-5", "last_user": "tighten"}, "status": 200, "delay_ms": 20, "lines": ["event: ping", "data: {}", ""]}
{"expect": {"results": [{"id": "toolu_1", "contains": "Added 8"}]}, "status": 529, "retry_after": 1, "body": {"type": "error"}}
{"offline": "connection reset"}
"#;
        let x = parse(text).unwrap();
        assert_eq!(x.len(), 3);
        assert_eq!(x[0].delay_ms, 20);
        assert_eq!(x[1].retry_after.as_deref(), Some("1"));
        assert_eq!(x[1].body.as_deref(), Some(r#"{"type":"error"}"#));
        assert_eq!(
            x[1].reply(),
            Reply::Fail(Failure::Status {
                status: 529,
                retry_after: Some("1".into()),
                body: r#"{"type":"error"}"#.into()
            })
        );
        assert_eq!(x[2].reply(), Reply::Fail(Failure::Offline("connection reset".into())));
        let cut = Exchange { lines: vec!["a".into()], offline: Some("reset".into()), ..Exchange::default() };
        assert_eq!(cut.reply(), Reply::Lines { lines: &cut.lines, then: Some(Failure::CutOff("reset".into())) });
    }

    #[test]
    fn parse_errors_name_the_exchange_and_line() {
        let e = parse("\n{\"status\": 200}\n// c\n{\"nope\": 1}\n").unwrap_err();
        assert!(e.starts_with("exchange 2 (line 4):"), "{e}");
        let e = parse(r#"{"status": 500, "lines": ["x"]}"#).unwrap_err();
        assert!(e.contains("line 1") && e.contains("lines"), "{e}");
        assert!(parse(r#"{"body": "x"}"#).is_err());
        let e = parse("{\"lines\": [\n\"a\",\n oops]}").unwrap_err();
        assert!(e.contains("exchange 1") && e.contains("line 3"), "{e}");
        assert!(parse(r#"{"lines": [1]}"#).is_err());
        assert!(parse(r#"{"lines": ["a"], "offline": "x", "bad_line": "y"}"#).is_err());
    }

    #[test]
    fn exchanges_span_lines_and_lines_can_be_objects() {
        let text = r#"
// first
{"expect": {"model": "m"},
 "lines": [
  "event: message_start",
  {"type": "message_start", "n": 1},
  ""
 ]}
{"lines": ["data: 1"], "bad_line": "not UTF-8"}
"#;
        let x = parse(text).unwrap();
        // Key order follows serde_json's map, so compare as JSON.
        assert_eq!((x[0].lines.len(), &x[0].lines[0][..], &x[0].lines[2][..]), (3, "event: message_start", ""));
        let data: Value = serde_json::from_str(x[0].lines[1].strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(data, json!({"type": "message_start", "n": 1}));
        assert_eq!(x[1].reply(), Reply::Lines { lines: &x[1].lines, then: Some(Failure::BadLine("not UTF-8".into())) });
    }

    #[test]
    fn to_text_is_readable_and_round_trips() {
        let x = Exchange {
            expect: Expect { model: Some("m".into()), ..Expect::default() },
            lines: vec!["event: ping".into(), r#"data: {"a":1}"#.into(), "data: [DONE]".into(), "".into()],
            ..Exchange::default()
        };
        let text = x.to_text();
        assert_eq!(
            text,
            "{\"expect\": {\"model\":\"m\"},\n \"lines\": [\n  \"event: ping\",\n  {\"a\":1},\n  \"data: [DONE]\",\n  \"\"\n ]}\n"
        );
        let e = Exchange {
            status: Some(529),
            retry_after: Some("3".into()),
            body: Some(r#"{"error":{"type":"overloaded_error"}}"#.into()),
            ..Exchange::default()
        };
        let both = format!("{text}{}", e.to_text());
        assert!(both.contains("\"body\": {\"error\""), "{both}");
        let b = Exchange { lines: vec!["x".into()], bad_line: Some("bad".into()), ..Exchange::default() };
        assert_eq!(parse(&format!("{both}{}", b.to_text())).unwrap(), vec![x, e, b]);
    }

    #[test]
    fn expect_matches_anthropic_requests() {
        let r = anthropic();
        let ok = |e: &str| serde_json::from_str::<Expect>(e).unwrap().check(&r);
        ok(r#"{"path": "/messages", "model": "claude-opus-5-5", "last_user": "tighten"}"#).unwrap();
        ok(r#"{"results": [{"id": "toolu_1", "contains": "Added 8"}]}"#).unwrap();
        let e = ok(r#"{"model": "claude-haiku"}"#).unwrap_err();
        assert!(e.contains("claude-opus-5-5"), "{e}");
        let e = ok(r#"{"results": [{"id": "toolu_1", "contains": "Removed"}]}"#).unwrap_err();
        assert!(e.contains("toolu_1") && e.contains("Removed"), "{e}");
        let e = ok(r#"{"results": [{"id": "toolu_2"}]}"#).unwrap_err();
        assert!(e.contains("toolu_1"), "lists the ids it has: {e}");
        assert!(ok(r#"{"path": "/chat/completions"}"#).is_err());
        assert!(ok(r#"{"last_user": "INT. KITCHEN"}"#).is_err(), "tool results aren't user text");
    }

    #[test]
    fn expect_matches_openai_requests() {
        let r = openai();
        let ok = |e: &str| serde_json::from_str::<Expect>(e).unwrap().check(&r);
        ok(r#"{"path": "/chat/completions", "last_user": "hello", "results": [{"id": "call_9", "contains": "Added"}]}"#)
            .unwrap();
        assert!(ok(r#"{"path": "x=1"}"#).is_err(), "the query is not the path");
    }

    #[test]
    fn recording_describes_the_request_without_headers() {
        let x = Exchange::recording(&anthropic());
        assert_eq!(x.expect.path.as_deref(), Some("/v1/messages"));
        assert_eq!(x.expect.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(x.expect.last_user.as_deref(), Some("Please tighten scene 3."));
        assert_eq!(x.expect.results.len(), 1);
        assert_eq!(x.expect.results[0].contains.as_deref(), Some("INT. KITCHEN - NIGHT\nAdded 8 lines"));
        x.expect.check(&anthropic()).unwrap();
        let line = x.to_text();
        assert!(!line.contains("secret"));
        assert_eq!(parse(&line).unwrap(), vec![x]);

        // Only the new results: none after the assistant asked nothing.
        let o = Exchange::recording(&openai());
        assert_eq!(
            o.expect.results,
            vec![ResultExpect { id: "call_9".into(), contains: Some("Added 8 suggestions".into()) }]
        );
        o.expect.check(&openai()).unwrap();
    }

    #[test]
    fn long_texts_are_cut_at_characters() {
        let long = "é".repeat(100);
        let r = req("https://h/v1/messages", json!({"model": "m", "messages": [{"role": "user", "content": long}]}));
        let x = Exchange::recording(&r);
        assert_eq!(x.expect.last_user.as_ref().unwrap().chars().count(), 60);
        x.expect.check(&r).unwrap();
    }

    #[test]
    fn urls() {
        assert_eq!(url_path("https://api.anthropic.com/v1/messages"), "/v1/messages");
        assert_eq!(url_path("http://h:1/a/b?c=/d"), "/a/b");
        assert_eq!(url_path("https://h"), "");
    }
}
