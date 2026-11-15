//! Request bodies, headers and URLs for each protocol, and the helpers for
//! models lists and HTTP errors.

mod common;

use scriptorium_agent::protocol::{self, Call, api_error, build_request, models_request, parse_models};
use scriptorium_agent::{
    AgentError, Block, Effort, ErrorKind, Message, Origin, Protocol, Thinking, Thread, ToolCall, ToolResult, ToolSpec,
};
use serde_json::{Value, json};

fn spec() -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "script_info".into(),
        description: "Summary of the script.".into(),
        schema: json!({"type": "object", "properties": {}}),
    }]
}

fn call<'a>(tools: &'a [ToolSpec], api_key: &'a str) -> Call<'a> {
    Call {
        base_url: "https://example.test/v1/",
        api_key,
        model: "m-1",
        system: "Be brief.",
        tools,
        ..Default::default()
    }
}

/// An assistant message written by `model` at `endpoint`.
fn from(endpoint: &str, model: &str, blocks: Vec<Block>) -> Message {
    Message { origin: Some(Origin::new(endpoint, model)), ..Message::assistant(blocks) }
}

/// user → assistant (signed thinking, unsigned thinking, empty text, text, two
/// tool calls, a block for the other protocol) → tool results (one an error) → user.
fn thread() -> Thread {
    let mut t = Thread::default();
    assert!(t.push_user("Tighten scene 2."));
    t.messages.push(from(
        "https://example.test/v1",
        "m-1",
        vec![
            Block::Thinking { text: "Look first.".into(), signature: Some("sig-1".into()) },
            Block::Thinking { text: "from another provider".into(), signature: None },
            Block::Text { text: String::new() },
            Block::Text { text: "Looking.".into() },
            Block::ToolCall(ToolCall {
                id: "call_1".into(),
                name: "list_paragraphs".into(),
                input: json!({"scene": "KITCHEN"}),
                input_error: None,
                raw_input: None,
                extra: None,
            }),
            Block::ToolCall(ToolCall {
                id: "call_2".into(),
                name: "suggest_change".into(),
                input: Value::Null,
                input_error: Some("cut off".into()),
                raw_input: None,
                extra: None,
            }),
            Block::Native { protocol: Protocol::Anthropic, value: json!({"type": "redacted_thinking", "data": "xyz"}) },
        ],
    ));
    t.push_tool_results(vec![
        ToolResult { id: "call_1".into(), content: "[{\"id\": 3}]".into(), is_error: false },
        ToolResult { id: "call_2".into(), content: "invalid arguments".into(), is_error: true },
    ])
    .unwrap();
    assert!(t.push_user("Also the title."));
    t
}

#[test]
fn anthropic_request() {
    let tools = spec();
    let r = build_request(Protocol::Anthropic, &call(&tools, "sk-ant"), &thread()).unwrap();
    assert_eq!(r.url, "https://example.test/v1/messages");
    assert_eq!(
        r.headers,
        vec![
            ("x-api-key".to_string(), "sk-ant".to_string()),
            ("anthropic-version".to_string(), "2026-10-21".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    );
    assert_eq!(
        r.body,
        json!({
            "model": "m-1",
            "max_tokens": 64000,
            "stream": true,
            "cache_control": {"type": "ephemeral"},
            "system": "Be brief.",
            "tools": [{
                "name": "script_info",
                "description": "Summary of the script.",
                "input_schema": {"type": "object", "properties": {}},
                "eager_input_streaming": true,
            }],
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "Tighten scene 2."}]},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "Look first.", "signature": "sig-1"},
                    // Empty text is left out: the API refuses it in every role.
                    {"type": "text", "text": "Looking."},
                    {"type": "tool_use", "id": "call_1", "name": "list_paragraphs", "input": {"scene": "KITCHEN"}},
                    {"type": "tool_use", "id": "call_2", "name": "suggest_change", "input": {}},
                    {"type": "redacted_thinking", "data": "xyz"},
                ]},
                // The tool results and the writer's next message are one user turn.
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "call_1", "content": "[{\"id\": 3}]", "is_error": false},
                    {"type": "tool_result", "tool_use_id": "call_2", "content": "invalid arguments", "is_error": true},
                    {"type": "text", "text": "Also the title."},
                ]},
            ],
        })
    );
}

#[test]
fn anthropic_body_has_no_sampling_parameters() {
    let tools = spec();
    let body = build_request(Protocol::Anthropic, &call(&tools, "k"), &thread()).unwrap().body;
    for k in ["temperature", "top_p", "top_k"] {
        assert!(body.get(k).is_none(), "{k}");
    }
}

#[test]
fn anthropic_body_without_system_or_tools() {
    let mut t = Thread::default();
    assert!(t.push_user("Hi"));
    let c = Call {
        base_url: "https://api.anthropic.com/v1",
        api_key: "k",
        model: "m",
        system: "",
        tools: &[],
        ..Default::default()
    };
    let body = build_request(Protocol::Anthropic, &c, &t).unwrap().body;
    assert!(body.get("system").is_none());
    assert!(body.get("tools").is_none());
    assert_eq!(body["messages"], json!([{"role": "user", "content": [{"type": "text", "text": "Hi"}]}]));
}

#[test]
fn anthropic_skips_assistant_turn_left_empty() {
    // An assistant message from another provider with only unsigned thinking
    // sends nothing; the user turns either side merge rather than alternate badly.
    let mut t = Thread::default();
    assert!(t.push_user("One."));
    t.messages.push(Message::assistant(vec![Block::Thinking { text: "x".into(), signature: None }]));
    assert!(t.push_user("Two."));
    let c = Call { base_url: "u", api_key: "", model: "m", system: "", tools: &[], ..Default::default() };
    assert_eq!(
        build_request(Protocol::Anthropic, &c, &t).unwrap().body["messages"],
        json!([{"role": "user", "content": [{"type": "text", "text": "One."}, {"type": "text", "text": "Two."}]}])
    );
}

#[test]
fn openai_request() {
    let tools = spec();
    let r = build_request(Protocol::OpenaiChat, &call(&tools, "sk-oa"), &thread()).unwrap();
    assert_eq!(r.url, "https://example.test/v1/chat/completions");
    assert_eq!(
        r.headers,
        vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("authorization".to_string(), "Bearer sk-oa".to_string()),
        ]
    );
    assert_eq!(
        r.body,
        json!({
            "model": "m-1",
            "stream": true,
            "stream_options": {"include_usage": true},
            "tools": [{"type": "function", "function": {
                "name": "script_info",
                "description": "Summary of the script.",
                "parameters": {"type": "object", "properties": {}},
            }}],
            "messages": [
                {"role": "system", "content": "Be brief."},
                {"role": "user", "content": "Tighten scene 2."},
                {"role": "assistant", "content": "Looking.", "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "list_paragraphs", "arguments": "{\"scene\":\"KITCHEN\"}"}},
                    {"id": "call_2", "type": "function", "function": {"name": "suggest_change", "arguments": "{}"}},
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "[{\"id\": 3}]"},
                {"role": "tool", "tool_call_id": "call_2", "content": "Error: invalid arguments"},
                {"role": "user", "content": "Also the title."},
            ],
        })
    );
}

#[test]
fn openai_assistant_without_text_has_null_content() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    t.messages.push(Message::assistant(vec![
        Block::Thinking { text: "hidden".into(), signature: None },
        Block::ToolCall(ToolCall {
            id: "a".into(),
            name: "script_info".into(),
            input: json!({}),
            input_error: None,
            raw_input: None,
            extra: None,
        }),
    ]));
    t.push_tool_results(vec![ToolResult { id: "a".into(), content: "ok".into(), is_error: false }]).unwrap();
    let c = Call {
        base_url: "http://localhost:8080/v1",
        api_key: "",
        model: "local",
        system: "",
        tools: &[],
        ..Default::default()
    };
    let r = build_request(Protocol::OpenaiChat, &c, &t).unwrap();
    assert_eq!(r.headers, vec![("content-type".to_string(), "application/json".to_string())]);
    assert!(r.body.get("tools").is_none());
    assert_eq!(
        r.body["messages"],
        json!([
            {"role": "user", "content": "Go."},
            {"role": "assistant", "content": null, "tool_calls": [
                {"id": "a", "type": "function", "function": {"name": "script_info", "arguments": "{}"}},
            ]},
            {"role": "tool", "tool_call_id": "a", "content": "ok"},
        ])
    );
    // The arguments are a JSON string, not an object.
    assert!(r.body["messages"][1]["tool_calls"][0]["function"]["arguments"].is_string());
}

#[test]
fn every_tool_is_sent() {
    let mut tools = spec();
    tools.push(ToolSpec {
        name: "list_paragraphs".into(),
        description: "The script's paragraphs.".into(),
        schema: json!({"type": "object", "properties": {"limit": {"type": "integer"}}}),
    });
    let c = Call { base_url: "u", api_key: "", model: "m", system: "", tools: &tools, ..Default::default() };
    let mut t = Thread::default();
    assert!(t.push_user("Hi"));
    let a = build_request(Protocol::Anthropic, &c, &t).unwrap().body;
    let o = build_request(Protocol::OpenaiChat, &c, &t).unwrap().body;
    assert_eq!(a["tools"].as_array().unwrap().len(), tools.len());
    assert_eq!(o["tools"].as_array().unwrap().len(), tools.len());
    for t in &tools {
        assert_eq!(t.schema["type"], "object", "{}", t.name);
    }
}

#[test]
fn models_requests() {
    let r = models_request(Protocol::Anthropic, "https://api.anthropic.com/v1/", "k");
    assert_eq!(r.url, "https://api.anthropic.com/v1/models");
    assert_eq!(r.body, Value::Null);
    assert!(r.headers.iter().any(|(k, v)| k == "x-api-key" && v == "k"));
    let r = models_request(Protocol::OpenaiChat, "http://localhost:11434/v1", "");
    assert_eq!(r.url, "http://localhost:11434/v1/models");
    assert!(!r.headers.iter().any(|(k, _)| k == "authorization"));
}

#[test]
fn parse_models_lists_sorted_ids() {
    let body = r#"{"data":[{"id":"gpt-b","object":"model"},{"id":"gpt-a"},{"name":"no id"}],"object":"list"}"#;
    assert_eq!(parse_models(body).unwrap(), ["gpt-a", "gpt-b"]);
    let anthropic = r#"{"data":[{"type":"model","id":"claude-opus-5-5","display_name":"Claude Opus 5.5"}],"has_more":false,"first_id":"claude-opus-5-5","last_id":"claude-opus-5-5"}"#;
    assert_eq!(parse_models(anthropic).unwrap(), ["claude-opus-5-5"]);
    assert!(matches!(parse_models("{}"), Err(AgentError::Parse(_))));
    assert!(matches!(parse_models("<html>"), Err(AgentError::Parse(_))));
}

fn api(status: u16, body: &str) -> (ErrorKind, bool, String) {
    match api_error(status, body, None) {
        AgentError::Api { status: Some(s), kind, retryable, message, retry_after: None } if s == status => {
            (kind, retryable, message)
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn api_errors() {
    use ErrorKind::*;
    let anthropic = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
    assert_eq!(api(401, anthropic), (Auth, false, "the API key was refused (401): invalid x-api-key".into()));
    let openai = r#"{"error":{"message":"The model `x` does not exist","type":"invalid_request_error","code":"model_not_found"}}"#;
    assert_eq!(
        api(404, openai),
        (
            BadRequest,
            false,
            "not found (check the model name and the address) (404): The model `x` does not exist".into()
        )
    );
    let gemini = r#"[{"error":{"code":429,"message":"Resource has been exhausted","status":"RESOURCE_EXHAUSTED"}}]"#;
    assert_eq!(
        api(429, gemini),
        (RateLimit, true, "rate limited or out of credit (429): Resource has been exhausted".into())
    );
    // Out of credit: waiting won't help.
    let quota = r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota"}}"#;
    assert_eq!(api(429, quota).0, RateLimit);
    assert!(!api(429, quota).1);
    let overloaded = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
    assert_eq!(api(529, overloaded), (Overloaded, true, "the provider had a problem (529): Overloaded".into()));
    assert_eq!(api(503, r#"{"message":"busy"}"#), (Overloaded, true, "the provider had a problem (503): busy".into()));
    assert_eq!(api(529, "<html>overloaded</html>"), (Overloaded, true, "the provider had a problem (529)".into()));
    assert_eq!(api(500, ""), (Other, true, "the provider had a problem (500)".into()));
    assert_eq!(api(400, ""), (BadRequest, false, "request failed (400)".into()));
    assert_eq!(api(403, "{}"), (Auth, false, "the API key was refused (403)".into()));
    // The shell's network failures.
    let e = AgentError::offline("no connection");
    assert_eq!(e.kind(), Some(Offline));
    assert_eq!(e.to_string(), "no connection");
}

#[test]
fn protocol_names_and_serde() {
    assert_eq!(Protocol::Anthropic.name(), "Anthropic");
    assert_eq!(serde_json::to_value(Protocol::OpenaiChat).unwrap(), json!("openai_chat"));
    assert_eq!(serde_json::from_value::<Protocol>(json!("anthropic")).unwrap(), Protocol::Anthropic);
    let _ = protocol::ReplyParser::new(Protocol::OpenaiChat);
}

#[test]
fn unsendable_threads_are_refused() {
    let c = Call { base_url: "u", model: "m", ..Default::default() };
    for p in [Protocol::Anthropic, Protocol::OpenaiChat] {
        let empty = build_request(p, &c, &Thread::default());
        assert!(matches!(empty, Err(AgentError::NotReady(_))));
        let mut t = Thread::default();
        assert!(t.push_user("Hi"));
        t.messages.push(Message::assistant(vec![Block::Text { text: "Hello".into() }]));
        assert!(matches!(build_request(p, &c, &t), Err(AgentError::NotReady(m)) if m.contains("assistant")));
        let mut t = thread();
        t.messages.pop();
        t.messages.pop();
        assert!(matches!(build_request(p, &c, &t), Err(AgentError::NotReady(m)) if m.contains("tool")));
    }
}

#[test]
fn anthropic_thinking_and_effort() {
    let mut t = Thread::default();
    assert!(t.push_user("Hi"));
    let c = Call { base_url: "u", model: "m", ..Default::default() };
    let body = build_request(Protocol::Anthropic, &c, &t).unwrap().body;
    assert!(body.get("thinking").is_none());
    assert!(body.get("output_config").is_none());

    let c = Call { effort: Some(Effort::High), thinking: Thinking::Adaptive, ..c };
    let body = build_request(Protocol::Anthropic, &c, &t).unwrap().body;
    assert_eq!(body["thinking"], json!({"type": "adaptive", "display": "summarized"}));
    assert_eq!(body["output_config"], json!({"effort": "high"}));
    // The OpenAI protocol ignores thinking; effort becomes `reasoning_effort`.
    let body = build_request(Protocol::OpenaiChat, &c, &t).unwrap().body;
    for k in ["thinking", "output_config"] {
        assert!(body.get(k).is_none(), "{k}");
    }
    assert_eq!(body["reasoning_effort"], "high");
}

#[test]
fn anthropic_never_sends_empty_or_blank_text_in_any_role() {
    let mut t = Thread::default();
    // Blank writer's messages (push_user refuses them; older threads may have them).
    t.messages.push(Message::user(""));
    t.messages.push(Message::user(" \n\t"));
    assert!(t.push_user("Go."));
    t.messages.push(from(
        "u",
        "m",
        vec![
            Block::Thinking { text: String::new(), signature: Some("s".into()) },
            Block::Text { text: String::new() },
            Block::Text { text: "\n\n".into() },
            Block::ToolCall(bad_call()),
        ],
    ));
    t.push_tool_results(vec![ToolResult { id: "b".into(), content: "invalid".into(), is_error: true }]).unwrap();
    // A reply that was only whitespace (e.g. stopped early) leaves no assistant turn.
    assert!(t.push_user("Next."));
    t.messages.push(from("u", "m", vec![Block::Text { text: " ".into() }]));
    assert!(t.push_user("Last."));
    let c = Call { base_url: "u", model: "m", ..Default::default() };
    let body = build_request(Protocol::Anthropic, &c, &t).unwrap().body;
    assert_eq!(body["messages"][0]["content"], json!([{"type": "text", "text": "Go."}]));
    assert_eq!(
        body["messages"][1]["content"],
        json!([
            // Thinking goes back unchanged, empty or not.
            {"type": "thinking", "thinking": "", "signature": "s"},
            // Invalid streamed JSON can't go back in `input`; `{}` keeps the block in place.
            {"type": "tool_use", "id": "b", "name": "suggest_change", "input": {}},
        ])
    );
    assert_eq!(body["messages"].as_array().unwrap().len(), 3);
    assert_eq!(body["messages"][2]["content"].as_array().unwrap().iter().filter(|b| b["type"] == "text").count(), 2);
    for m in body["messages"].as_array().unwrap() {
        for b in m["content"].as_array().unwrap() {
            if b["type"] == "text" {
                assert!(!b["text"].as_str().unwrap().trim().is_empty(), "{b}");
            }
        }
    }
}

fn bad_call() -> ToolCall {
    ToolCall {
        id: "b".into(),
        name: "suggest_change".into(),
        input: Value::Null,
        input_error: Some("cut off".into()),
        raw_input: Some("{\"options\": [".into()),
        extra: None,
    }
}

/// An assistant turn with OpenRouter reasoning details and a Gemini thought signature.
fn provider_extras_thread() -> Thread {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    t.messages.push(from(
        "u",
        "m",
        vec![
            Block::Native {
                protocol: Protocol::OpenaiChat,
                value: json!({"reasoning_details": [{"type": "reasoning.encrypted", "data": "enc", "index": 0}]}),
            },
            Block::ToolCall(ToolCall {
                id: "g1".into(),
                name: "script_info".into(),
                input: json!({}),
                input_error: None,
                raw_input: None,
                extra: Some(json!({"google": {"thought_signature": "SIG=="}})),
            }),
            Block::ToolCall(bad_call()),
        ],
    ));
    t.push_tool_results(vec![
        ToolResult { id: "g1".into(), content: "ok".into(), is_error: false },
        ToolResult { id: "b".into(), content: "invalid".into(), is_error: true },
    ])
    .unwrap();
    t
}

#[test]
fn openai_replays_extra_content_and_reasoning_details() {
    let c = Call { base_url: "u", model: "m", ..Default::default() };
    let body = build_request(Protocol::OpenaiChat, &c, &provider_extras_thread()).unwrap().body;
    assert_eq!(
        body["messages"][1],
        json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [
                {"id": "g1", "type": "function", "function": {"name": "script_info", "arguments": "{}"},
                 "extra_content": {"google": {"thought_signature": "SIG=="}}},
                {"id": "b", "type": "function", "function": {"name": "suggest_change", "arguments": "{}"}},
            ],
            "reasoning_details": [{"type": "reasoning.encrypted", "data": "enc", "index": 0}],
        })
    );
}

#[test]
fn anthropic_drops_openai_only_extras() {
    let c = Call { base_url: "u", model: "m", ..Default::default() };
    let body = build_request(Protocol::Anthropic, &c, &provider_extras_thread()).unwrap().body;
    assert_eq!(
        body["messages"][1]["content"],
        json!([
            {"type": "tool_use", "id": "g1", "name": "script_info", "input": {}},
            {"type": "tool_use", "id": "b", "name": "suggest_change", "input": {}},
        ])
    );
}

#[test]
fn a_parsed_gemini_call_replays_its_signature() {
    let body = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"g\",\"function\":{\"name\":\"script_info\",\"arguments\":\"{}\"},\"extra_content\":{\"google\":{\"thought_signature\":\"S1\"}}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n";
    let mut p = protocol::ReplyParser::new(Protocol::OpenaiChat);
    for line in body.split_inclusive('\n') {
        p.line(line).unwrap();
    }
    let c = Call { base_url: "u", model: "m", ..Default::default() };
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let calls = t.push_turn(p.finish().unwrap(), c.origin()).calls;
    t.push_tool_results(vec![ToolResult { id: calls[0].id.clone(), content: "ok".into(), is_error: false }]).unwrap();
    let t: Thread = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
    let body = build_request(Protocol::OpenaiChat, &c, &t).unwrap().body;
    assert_eq!(body["messages"][1]["tool_calls"][0]["extra_content"], json!({"google": {"thought_signature": "S1"}}));
}

#[test]
fn sse_event_without_data_is_not_dispatched() {
    use scriptorium_agent::ReplyParser;
    // Anthropic falls back to the `event:` name when the data has no `type`,
    // so a leaked name would show: a dataless `message_stop` frame must neither
    // complete the reply nor name the next frame.
    let mut p = ReplyParser::new(Protocol::Anthropic);
    for line in ["event: message_stop\n", "\n", "data: {}\n", "\n", "event: message_stop\n"] {
        assert_eq!(p.line(line).unwrap(), vec![]);
    }
    assert!(p.finish().unwrap_err().to_string().contains("cut off"));
    // With data, the same name does complete it.
    let mut p = ReplyParser::new(Protocol::Anthropic);
    for line in ["event: message_stop\n", "data: {}\n", "\n"] {
        p.line(line).unwrap();
    }
    assert!(p.finish().is_ok());
}

#[test]
fn replay_data_goes_only_to_its_own_endpoint_and_model() {
    // The extras thread was written by model "m" at "u".
    let t = provider_extras_thread();
    for c in [
        Call { base_url: "other", model: "m", ..Default::default() },
        Call { base_url: "u", model: "m2", ..Default::default() },
    ] {
        let body = build_request(Protocol::OpenaiChat, &c, &t).unwrap().body;
        let m = &body["messages"][1];
        assert!(m.get("reasoning_details").is_none(), "{m}");
        // The calls themselves always go.
        assert_eq!(m["tool_calls"].as_array().unwrap().len(), 2);
        assert!(m["tool_calls"][0].get("extra_content").is_none(), "{m}");
    }
    // A trailing slash doesn't change the endpoint.
    let c = Call { base_url: "u/", model: "m", ..Default::default() };
    let body = build_request(Protocol::OpenaiChat, &c, &t).unwrap().body;
    assert!(body["messages"][1].get("reasoning_details").is_some());

    // Anthropic: signed thinking and redacted thinking need the same origin;
    // text and tool calls always go.
    let tools = spec();
    let mut other = call(&tools, "k");
    other.model = "m-2";
    let body = build_request(Protocol::Anthropic, &other, &thread()).unwrap().body;
    assert_eq!(
        body["messages"][1]["content"],
        json!([
            {"type": "text", "text": "Looking."},
            {"type": "tool_use", "id": "call_1", "name": "list_paragraphs", "input": {"scene": "KITCHEN"}},
            {"type": "tool_use", "id": "call_2", "name": "suggest_change", "input": {}},
        ])
    );
    // A message without an origin (older files) replays nothing either.
    let mut t = thread();
    t.messages[1].origin = None;
    let body = build_request(Protocol::Anthropic, &call(&tools, "k"), &t).unwrap().body;
    assert!(!body.to_string().contains("sig-1"));
    assert!(!body.to_string().contains("redacted_thinking"));
}

#[test]
fn anthropic_tool_ids_are_sanitized_alike_for_calls_and_results() {
    let ids = ["functions.list_paragraphs:0", "functions.list_paragraphs_0", "ok_id-1", ""];
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let calls = ids.iter().map(|id| {
        Block::ToolCall(ToolCall {
            id: (*id).into(),
            name: "x".into(),
            input: json!({}),
            input_error: None,
            raw_input: None,
            extra: None,
        })
    });
    t.messages.push(Message::assistant(calls.collect()));
    t.push_tool_results(
        ids.iter().map(|id| ToolResult { id: (*id).into(), content: "ok".into(), is_error: false }).collect(),
    )
    .unwrap();
    let c = Call { base_url: "u", model: "m", ..Default::default() };
    let body = build_request(Protocol::Anthropic, &c, &t).unwrap().body;
    let sent: Vec<String> = body["messages"][1]["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["id"].as_str().unwrap().to_string())
        .collect();
    let answered: Vec<String> = body["messages"][2]["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["tool_use_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(sent, answered);
    assert_eq!(sent[2], "ok_id-1");
    let valid = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    assert!(sent.iter().all(|s| valid(s)), "{sent:?}");
    // Distinct ids stay distinct, and the mapping is stable.
    let mut d = sent.clone();
    d.sort();
    d.dedup();
    assert_eq!(d.len(), ids.len());
    assert_eq!(build_request(Protocol::Anthropic, &c, &t).unwrap().body, body);
    // OpenAI takes ids as they are.
    let o = build_request(Protocol::OpenaiChat, &c, &t).unwrap().body;
    assert_eq!(o["messages"][1]["tool_calls"][0]["id"], "functions.list_paragraphs:0");
}

#[test]
fn output_limit_effort_and_extra_headers() {
    let mut t = Thread::default();
    assert!(t.push_user("Hi"));
    let headers = [("HTTP-Referer".to_string(), "https://scriptorium.test".to_string())];
    let c = Call { base_url: "u", model: "m", ..Default::default() };
    // Defaults: Anthropic's limit is 64000; OpenAI sends none.
    assert_eq!(build_request(Protocol::Anthropic, &c, &t).unwrap().body["max_tokens"], 64000);
    let o = build_request(Protocol::OpenaiChat, &c, &t).unwrap().body;
    assert!(o.get("max_completion_tokens").is_none());
    assert!(o.get("max_tokens").is_none());
    assert!(o.get("reasoning_effort").is_none());

    let c = Call { max_tokens: Some(1234), extra_headers: &headers, ..c };
    for p in [Protocol::Anthropic, Protocol::OpenaiChat] {
        let r = build_request(p, &c, &t).unwrap();
        assert_eq!(r.headers.last(), Some(&headers[0]), "{p:?}");
    }
    assert_eq!(build_request(Protocol::Anthropic, &c, &t).unwrap().body["max_tokens"], 1234);
    // Other OpenAI-compatible servers get `max_tokens`; OpenAI itself the newer name.
    let o = build_request(Protocol::OpenaiChat, &c, &t).unwrap().body;
    assert_eq!((o["max_tokens"].as_u64(), o.get("max_completion_tokens")), (Some(1234), None));
    let to_openai = Call { base_url: "https://api.openai.com/v1/", ..c };
    let o = build_request(Protocol::OpenaiChat, &to_openai, &t).unwrap().body;
    assert_eq!((o["max_completion_tokens"].as_u64(), o.get("max_tokens")), (Some(1234), None));
    let lookalike = Call { base_url: "https://api.openai.com.example.org/v1", ..c };
    assert_eq!(build_request(Protocol::OpenaiChat, &lookalike, &t).unwrap().body["max_tokens"], 1234);

    for (effort, anthropic, openai) in [
        (Effort::Low, "low", Some("low")),
        (Effort::Medium, "medium", Some("medium")),
        (Effort::High, "high", Some("high")),
        (Effort::XHigh, "xhigh", Some("high")),
        (Effort::Max, "max", Some("high")),
    ] {
        let c = Call { effort: Some(effort), ..c };
        let a = build_request(Protocol::Anthropic, &c, &t).unwrap().body;
        assert_eq!(a["output_config"], json!({"effort": anthropic}));
        let o = build_request(Protocol::OpenaiChat, &c, &t).unwrap().body;
        assert_eq!(o.get("reasoning_effort").and_then(Value::as_str), openai, "{effort:?}");
        assert_eq!(serde_json::to_value(effort).unwrap(), json!(anthropic));
    }
}

#[test]
fn notices_are_never_sent() {
    let mut t = Thread::default();
    assert!(t.push_user("Hi"));
    t.messages[0].blocks.push(Block::Notice { text: "secret notice".into() });
    t.messages.push(Message::assistant(vec![Block::Notice { text: "secret notice".into() }]));
    let c = Call { base_url: "u", model: "m", ..Default::default() };
    for p in [Protocol::Anthropic, Protocol::OpenaiChat] {
        let body = build_request(p, &c, &t).unwrap().body;
        assert!(!body.to_string().contains("secret"), "{p:?}");
    }
}

#[test]
fn api_errors_carry_retry_after() {
    let retry = |v: Option<&str>| match api_error(429, "", v) {
        AgentError::Api { retry_after, .. } => retry_after,
        other => panic!("{other:?}"),
    };
    assert_eq!(retry(Some("30")), Some(30));
    assert_eq!(retry(Some(" 1.5 ")), Some(2));
    assert_eq!(retry(Some("Wed, 21 Oct 2026 07:28:00 GMT")), None);
    assert_eq!(retry(Some("-3")), None);
    assert_eq!(retry(None), None);
}

/// Assistant messages that only some targets (or none) would be sent.
fn nothing_to_send() -> Vec<Message> {
    vec![
        from("u", "m", vec![Block::Thinking { text: "unsigned".into(), signature: None }]),
        from("other", "m", vec![Block::Thinking { text: "signed elsewhere".into(), signature: Some("s".into()) }]),
        from("u", "m", vec![Block::Text { text: "  \n".into() }]),
        from("u", "m", vec![Block::Native { protocol: Protocol::OpenaiChat, value: json!({"reasoning_details": []}) }]),
        from("other", "m", vec![Block::Native { protocol: Protocol::Anthropic, value: json!({"type": "x"}) }]),
        Message::assistant(vec![Block::Thinking { text: "no origin".into(), signature: Some("s".into()) }]),
    ]
}

/// The bodies' messages without the system prompt.
fn sent(p: Protocol, c: &Call, t: &Thread) -> Vec<Value> {
    let body = build_request(p, c, t).unwrap().body;
    body["messages"].as_array().unwrap().iter().filter(|m| m["role"] != "system").cloned().collect()
}

#[test]
fn replies_with_nothing_for_the_target_are_neither_sent_nor_counted() {
    let c = Call { base_url: "u", model: "m", ..Default::default() };
    for m in nothing_to_send() {
        for p in [Protocol::Anthropic, Protocol::OpenaiChat] {
            assert!(!protocol::is_sent(p, Some(&c.origin()), &m), "{p:?} {m:?}");
            // Ending on it: the thread still ends on the writer's message.
            let mut t = Thread::default();
            assert!(t.push_user("Hi"));
            t.messages.push(m.clone());
            assert!(t.check_sendable(p, &c.origin()).is_ok(), "{p:?} {m:?}");
            assert_eq!(sent(p, &c, &t).len(), 1, "{p:?} {m:?}");
            // Between two writer's messages: no empty assistant turn goes out.
            assert!(t.push_user("Again"));
            let msgs = sent(p, &c, &t);
            assert!(msgs.iter().all(|m| m["role"] == "user"), "{p:?} {msgs:?}");
        }
    }
}

#[test]
fn replay_only_replies_count_for_their_own_anthropic_origin() {
    let c = Call { base_url: "u", model: "m", ..Default::default() };
    let signed = from("u", "m", vec![Block::Thinking { text: "t".into(), signature: Some("s".into()) }]);
    let native = from("u", "m", vec![Block::Native { protocol: Protocol::Anthropic, value: json!({"type": "x"}) }]);
    for m in [signed, native] {
        assert!(protocol::is_sent(Protocol::Anthropic, Some(&c.origin()), &m));
        assert!(!protocol::is_sent(Protocol::OpenaiChat, Some(&c.origin()), &m));
        assert!(!protocol::is_sent(Protocol::Anthropic, None, &m));
        let mut t = Thread::default();
        assert!(t.push_user("Hi"));
        t.messages.push(m);
        // Sent to its origin, the thread would end on the assistant.
        assert!(matches!(build_request(Protocol::Anthropic, &c, &t), Err(AgentError::NotReady(_))));
        assert!(common::sendable(&t).is_ok());
        assert_eq!(sent(Protocol::OpenaiChat, &c, &t).len(), 1);
        assert!(t.push_user("Again"));
        assert_eq!(sent(Protocol::Anthropic, &c, &t)[1]["role"], "assistant");
    }
}

#[test]
fn openai_never_sends_whitespace_as_content() {
    let c = Call { base_url: "u", model: "m", ..Default::default() };
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    t.messages.push(from("u", "m", vec![Block::Text { text: "  ".into() }, Block::ToolCall(bad_call())]));
    t.push_tool_results(vec![ToolResult { id: "b".into(), content: "invalid".into(), is_error: true }]).unwrap();
    t.messages.last_mut().unwrap().blocks.push(Block::Text { text: " \n".into() });
    let msgs = sent(Protocol::OpenaiChat, &c, &t);
    let roles: Vec<_> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["user", "assistant", "tool"]);
    assert_eq!(msgs[1]["content"], Value::Null);
}
