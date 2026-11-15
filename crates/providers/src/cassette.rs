//! Replay cassettes: recorded provider exchanges, for tests and fixtures.
//!
//! A cassette is JSON Lines, one [`Exchange`] per request, in order:
//!
//! ```text
//! {"expect": {"path": "/messages", "model": "claude-opus-5-5", "last_user": "tighten"},
//!  "status": 200, "delay_ms": 20, "lines": ["event: message_start", "data: {...}", ""]}
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
//!   before each; an error status answers `body` (a string, or JSON written
//!   inline) and `retry_after` (seconds, number or string).
//! - `offline` alone: the connection fails before any reply. With `lines`:
//!   they stream, then the connection is cut off.
//!
//! Blank lines and lines starting with `//` are skipped. This module is pure:
//! it parses, checks and writes exchanges; [`Replay`](crate::shell::Replay)
//! and the recorder in [`Http`](crate::shell::Http) do the I/O.

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "text_or_json")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "text_or_json")]
    pub retry_after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline: Option<String>,
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

/// The exchanges in a cassette's text. Errors name the line.
pub fn parse(text: &str) -> Result<Vec<Exchange>, String> {
    let mut out = vec![];
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() || t.starts_with("//") {
            continue;
        }
        let e: Exchange = serde_json::from_str(t).map_err(|e| format!("line {}: {e}", i + 1))?;
        e.check().map_err(|m| format!("line {}: {m}", i + 1))?;
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
        if self.offline.is_some() && self.status.is_some_and(|s| !(200..300).contains(&s)) {
            return Err("\"offline\" with an error status".into());
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
        match &self.offline {
            Some(m) if self.lines.is_empty() => Reply::Fail(Failure::Offline(m.clone())),
            Some(m) => Reply::Lines { lines: &self.lines, then: Some(Failure::CutOff(m.clone())) },
            None => Reply::Lines { lines: &self.lines, then: None },
        }
    }

    /// An exchange to record `req` under: its `expect` describes the request
    /// (path, model, the start of the last user text and of each new tool
    /// result) and holds no headers, so no key. The caller fills in the reply.
    pub fn recording(req: &Request) -> Exchange {
        Exchange { expect: Expect::describe(req), ..Exchange::default() }
    }

    /// One cassette line.
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("exchanges always serialize")
    }
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
    fn parse_errors_name_the_line() {
        let e = parse("\n{\"status\": 200}\n{\"nope\": 1}\n").unwrap_err();
        assert!(e.starts_with("line 3:"), "{e}");
        let e = parse(r#"{"status": 500, "lines": ["x"]}"#).unwrap_err();
        assert!(e.contains("line 1") && e.contains("lines"), "{e}");
        assert!(parse(r#"{"body": "x"}"#).is_err());
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
        let line = x.to_line();
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
