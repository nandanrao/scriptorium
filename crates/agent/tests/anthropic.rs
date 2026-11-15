//! Reading streamed Anthropic Messages replies.

mod common;

use common::{content, error, run, streamed_text, text, thinking, turn};
use scriptorium_agent::{AgentError, Block, ErrorKind, Event, Protocol, Role, StopReason, ToolCall, Usage};
use serde_json::json;

const P: Protocol = Protocol::Anthropic;

/// Thinking (with a signature), text, a tool call whose arguments arrive in
/// many pieces (split inside a string, inside an escape, and right after a
/// multi-byte character), a ping, and the usage at both ends.
const FULL: &str = r##"event: message_start
data: {"type":"message_start","message":{"id":"msg_01","type":"message","role":"assistant","model":"claude-opus-5-5","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":120,"cache_creation_input_tokens":300,"cache_read_input_tokens":4000,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"The writer wants "}}

event: ping
data: {"type": "ping"}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"tighter dialogue."}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"EqQBCgIYAhIM1gbcDa9GJwZA2b3h"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"I'll propose "}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"a change — café first."}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: content_block_start
data: {"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_01A","name":"suggest_change","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"paragraph_id\": 4"}}

event: content_block_delta
data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"2, \"options\": [\"Meet me at the café"}}

event: content_block_delta
data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":" at noon.\"]"}}

event: content_block_delta
data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":", \"why\": \"She says \\"}}

event: content_block_delta
data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"noon\\\" — 日本"}}

event: content_block_delta
data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"語.\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":2}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":245}}

event: message_stop
data: {"type":"message_stop"}

"##;

#[test]
fn full_reply() {
    let (events, turn) = run(P, FULL);
    let turn = turn.unwrap();
    assert_eq!(turn.stop, StopReason::ToolUse);
    // Input includes the cached and cache-written tokens.
    // Cache writes are counted separately from reads.
    let usage = Usage { input: 4420, output: 245, cached: 4000, cache_write: 300, ..Default::default() };
    assert_eq!(turn.usage, usage);
    assert_eq!(turn.message.role, Role::Assistant);
    assert_eq!(
        turn.message.blocks,
        vec![
            Block::Thinking {
                text: "The writer wants tighter dialogue.".into(),
                signature: Some("EqQBCgIYAhIM1gbcDa9GJwZA2b3h".into()),
            },
            Block::Text { text: "I'll propose a change — café first.".into() },
            Block::ToolCall(ToolCall {
                id: "toolu_01A".into(),
                name: "suggest_change".into(),
                input: json!({
                    "paragraph_id": 42,
                    "options": ["Meet me at the café at noon."],
                    "why": "She says \"noon\" — 日本語.",
                }),
                input_error: None,
                raw_input: None,
                extra: None,
            }),
        ]
    );
    assert_eq!(
        content(&events),
        vec![
            thinking(0, "The writer wants "),
            thinking(0, "tighter dialogue."),
            text(1, "I'll propose "),
            text(1, "a change — café first."),
            Event::ToolCallStarted { id: "toolu_01A".into(), name: "suggest_change".into() },
        ]
    );
    // Usage at the start (input) and the end (total), the stop reason, and
    // argument progress for the tool call.
    let start = Usage { input: 4420, output: 1, cached: 4000, cache_write: 300, ..Default::default() };
    assert_eq!(events[0], Event::Usage(start));
    assert_eq!(events[events.len() - 2], Event::Stop(StopReason::ToolUse));
    assert_eq!(events[events.len() - 1], Event::Usage(usage));
    let progress: Vec<usize> = events
        .iter()
        .filter_map(|e| match e {
            Event::ToolArgs { id, bytes_so_far } if id == "toolu_01A" => Some(*bytes_so_far),
            _ => None,
        })
        .collect();
    assert!(progress.len() > 1 && progress.windows(2).all(|w| w[0] < w[1]), "{progress:?}");
}

#[test]
fn crlf_line_endings() {
    let crlf = FULL.replace('\n', "\r\n");
    let (events, turn) = run(P, &crlf);
    assert_eq!(turn.unwrap(), common::turn(P, FULL));
    assert_eq!(streamed_text(&events), "I'll propose a change — café first.");
}

#[test]
fn body_without_trailing_blank_line() {
    // The last frame is still read when the body ends right after its data line.
    let trimmed = FULL.trim_end();
    assert_eq!(turn(P, trimmed), turn(P, FULL));
}

const SIMPLE_START: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_02","type":"message","role":"assistant","content":[],"usage":{"input_tokens":10,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}

"#;

#[test]
fn error_frame_mid_stream() {
    let body = format!(
        "{SIMPLE_START}event: error\ndata: {{\"type\":\"error\",\"error\":{{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}}}\n\n"
    );
    let (events, result) = run(P, &body);
    assert_eq!(content(&events), vec![text(0, "Hello")]);
    assert_eq!(
        result.unwrap_err(),
        AgentError::Api {
            status: None,
            kind: ErrorKind::Overloaded,
            retryable: true,
            message: "Overloaded".into(),
            retry_after: None
        }
    );
}

#[test]
fn stream_cut_off_before_message_stop_is_an_error() {
    let e = error(P, SIMPLE_START);
    assert!(matches!(e, AgentError::Api { kind: ErrorKind::Offline, retryable: true, .. }), "{e:?}");
    // Even with the stop reason in, the reply isn't complete until message_stop.
    let body = format!(
        "{SIMPLE_START}event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
         event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":3}}}}\n\n"
    );
    assert!(matches!(error(P, &body), AgentError::Api { .. }));
    let complete = format!("{body}event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n");
    let t = turn(P, &complete);
    assert_eq!(t.message.text(), "Hello");
    assert_eq!(t.stop, StopReason::EndTurn);
    assert_eq!(t.usage, Usage { input: 10, output: 3, ..Default::default() });
}

#[test]
fn empty_body_is_an_error() {
    assert!(error(P, "").to_string().contains("cut off"));
}

#[test]
fn garbage_data_is_a_parse_error() {
    assert!(matches!(error(P, "event: message_start\ndata: {not json\n\n"), AgentError::Parse(_)));
}

fn tool_reply(chunks: &[&str]) -> String {
    let mut s = String::from(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n\
         event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_9\",\"name\":\"list_paragraphs\",\"input\":{}}}\n\n",
    );
    for c in chunks {
        let delta = json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": c}});
        s.push_str(&format!("event: content_block_delta\ndata: {delta}\n\n"));
    }
    s.push_str(
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
         event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"output_tokens\":64000}}\n\n\
         event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    s
}

fn only_call(chunks: &[&str]) -> ToolCall {
    let t = turn(P, &tool_reply(chunks));
    assert_eq!(t.stop, StopReason::MaxTokens);
    match t.message.blocks.as_slice() {
        [Block::ToolCall(c)] => c.clone(),
        other => panic!("expected one tool call, got {other:?}"),
    }
}

#[test]
fn cut_off_tool_args_set_input_error() {
    let c = only_call(&["{\"contains\": \"MAR", "Y"]);
    assert_eq!(c.input, serde_json::Value::Null);
    assert!(c.input_error.as_deref().unwrap().contains("cut off"));
}

#[test]
fn invalid_and_non_object_tool_args_set_input_error() {
    assert!(only_call(&["{contains: 1}"]).input_error.is_some());
    let c = only_call(&["[1, 2]"]);
    assert!(c.input_error.as_deref().unwrap().contains("object"));
}

#[test]
fn empty_tool_args_are_an_empty_object() {
    for chunks in [&[][..], &[""][..], &["{}"][..], &["{", "}"][..]] {
        let c = only_call(chunks);
        assert_eq!(c.input, json!({}), "{chunks:?}");
        assert_eq!(c.input_error, None);
    }
}

#[test]
fn redacted_thinking_becomes_native() {
    let body = r#"event: message_start
data: {"type":"message_start","message":{"usage":{"input_tokens":7,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"EmwKAhgBEgy3va3pzix/LafPsn4aDFIT"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":"","signature":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"signature_delta","signature":"sigOmitted"}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: content_block_start
data: {"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}

event: content_block_stop
data: {"type":"content_block_stop","index":2}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":20}}

event: message_stop
data: {"type":"message_stop"}
"#;
    let t = turn(P, body);
    assert_eq!(
        t.message.blocks,
        vec![
            Block::Native {
                protocol: Protocol::Anthropic,
                value: json!({"type": "redacted_thinking", "data": "EmwKAhgBEgy3va3pzix/LafPsn4aDFIT"}),
            },
            // Thinking with display "omitted": empty text, but the signature must survive to be replayed.
            Block::Thinking { text: String::new(), signature: Some("sigOmitted".into()) },
            // The empty text block is kept in storage (requests leave it out).
            Block::Text { text: String::new() },
        ]
    );
}

#[test]
fn stop_reasons() {
    for (wire, ours) in [
        ("end_turn", StopReason::EndTurn),
        ("stop_sequence", StopReason::EndTurn),
        ("tool_use", StopReason::ToolUse),
        ("max_tokens", StopReason::MaxTokens),
        ("refusal", StopReason::Refusal),
        ("pause_turn", StopReason::Other("pause_turn".into())),
    ] {
        let body = format!(
            "event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"{wire}\"}},\"usage\":{{\"output_tokens\":1}}}}\n\n\
             event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
        );
        assert_eq!(turn(P, &body).stop, ours, "{wire}");
    }
}

#[test]
fn unknown_events_and_comments_are_ignored() {
    let body = format!(
        ": keep-alive\n\nevent: some_future_event\ndata: {{\"type\":\"some_future_event\",\"x\":1}}\n\n{SIMPLE_START}\
         event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
    );
    assert_eq!(turn(P, &body).message.text(), "Hello");
}

#[test]
fn empty_thinking_deltas_emit_no_events() {
    let body = r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"s"}}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}

event: message_stop
data: {"type":"message_stop"}
"#;
    let (events, t) = run(P, body);
    assert!(content(&events).is_empty(), "{events:?}");
    assert_eq!(t.unwrap().message.blocks, vec![Block::Thinking { text: String::new(), signature: Some("s".into()) }]);
}

#[test]
fn huge_block_index_is_a_parse_error() {
    let body = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":99999999999,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n";
    assert!(matches!(error(P, body), AgentError::Parse(_)));
}

#[test]
fn invalid_tool_args_keep_their_raw_text() {
    let c = only_call(&["{\"contains\": \"MAR", "Y"]);
    assert_eq!(c.raw_input.as_deref(), Some("{\"contains\": \"MARY"));
    assert_eq!(only_call(&["{}"]).raw_input, None);
}

#[test]
fn refusal_explanation_is_surfaced() {
    let body = format!(
        "{SIMPLE_START}event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"refusal\",\"stop_details\":{{\"type\":\"refusal\",\"category\":\"cyber\",\"explanation\":\"Declined.\"}}}},\"usage\":{{\"output_tokens\":3}}}}\n\n\
         event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
    );
    let t = turn(P, &body);
    assert_eq!(t.stop, StopReason::Refusal);
    assert_eq!(t.refusal.as_deref(), Some("Declined."));
}

#[test]
fn abort_keeps_only_the_text() {
    let mut p = scriptorium_agent::ReplyParser::new(P);
    let body = FULL.split("event: message_delta").next().unwrap();
    for line in body.split_inclusive('\n') {
        p.line(line).unwrap();
    }
    let t = p.abort();
    assert_eq!(t.stop, StopReason::Cancelled);
    // No thinking, no tool call: nothing half-done is replayed or run.
    assert_eq!(t.message.blocks, vec![Block::Text { text: "I'll propose a change — café first.".into() }]);
    // The input count arrived at the start; the output count never did.
    assert_eq!(
        t.usage,
        Usage { input: 4420, output: 1, cached: 4000, cache_write: 300, known: false, reasoning: None, cost: None }
    );
}

#[test]
fn stream_errors_map_their_kind() {
    for (kind_name, kind, retryable) in [
        ("overloaded_error", ErrorKind::Overloaded, true),
        ("rate_limit_error", ErrorKind::RateLimit, true),
        ("api_error", ErrorKind::Other, true),
        ("invalid_request_error", ErrorKind::BadRequest, false),
        ("authentication_error", ErrorKind::Auth, false),
    ] {
        let body = format!(
            "event: error\ndata: {{\"type\":\"error\",\"error\":{{\"type\":\"{kind_name}\",\"message\":\"m\"}}}}\n\n"
        );
        assert_eq!(
            error(P, &body),
            AgentError::Api { status: None, kind, retryable, message: "m".into(), retry_after: None },
            "{kind_name}"
        );
    }
}

#[test]
fn a_reply_without_usage_has_unknown_usage() {
    let body = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    assert!(!turn(P, body).usage.known);
}

#[test]
fn a_later_input_count_without_cache_fields_keeps_the_cache_counts() {
    let body = "event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":20,\"cache_read_input_tokens\":4000,\"cache_creation_input_tokens\":300,\"output_tokens\":1}}}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":20,\"output_tokens\":9}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    assert_eq!(
        turn(P, body).usage,
        Usage { input: 4320, output: 9, cached: 4000, cache_write: 300, ..Default::default() }
    );
}

#[test]
fn cache_counts_without_an_input_count_never_exceed_the_input() {
    // The cache counts arrive in a later piece than `input_tokens`, or alone.
    let body = "event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"cache_read_input_tokens\":50,\"cache_creation_input_tokens\":5,\"output_tokens\":9}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let (events, t) = run(P, body);
    let u = t.unwrap().usage;
    assert_eq!(u, Usage { input: 65, output: 9, cached: 50, cache_write: 5, ..Default::default() });
    assert_eq!(u.uncached_input(), 10);
    for e in events {
        if let Event::Usage(u) = e {
            assert!(u.cached + u.cache_write <= u.input, "{u:?}");
        }
    }
    let body = "event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"cache_read_input_tokens\":40}}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let u = turn(P, body).usage;
    assert_eq!((u.input, u.cached, u.uncached_input()), (40, 40, 0));
}
