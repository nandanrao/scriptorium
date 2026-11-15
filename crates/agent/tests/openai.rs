//! Reading streamed OpenAI Chat Completions replies (and compatible providers').

mod common;

use common::{content, error, run, text, thinking, turn};
use scriptorium_agent::{AgentError, Block, ErrorKind, Event, Protocol, StopReason, ToolCall, Usage};
use serde_json::json;

const P: Protocol = Protocol::OpenaiChat;

const TEXT: &str = r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-x","choices":[{"index":0,"delta":{"role":"assistant","content":"","refusal":null},"logprobs":null,"finish_reason":null}],"usage":null}

data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-x","choices":[{"index":0,"delta":{"content":"Hello, "},"logprobs":null,"finish_reason":null}],"usage":null}

data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-x","choices":[{"index":0,"delta":{"content":"café — 日本語."},"logprobs":null,"finish_reason":null}],"usage":null}

data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-x","choices":[{"index":0,"delta":{},"logprobs":null,"finish_reason":"stop"}],"usage":null}

data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-x","choices":[],"usage":{"prompt_tokens":900,"completion_tokens":12,"total_tokens":912,"prompt_tokens_details":{"cached_tokens":768,"audio_tokens":0},"completion_tokens_details":{"reasoning_tokens":0}}}

data: [DONE]

"#;

#[test]
fn text_reply_with_usage_chunk() {
    let (events, t) = run(P, TEXT);
    let t = t.unwrap();
    let usage = Usage { input: 900, output: 12, cached: 768, reasoning: Some(0), ..Default::default() };
    assert_eq!(
        events,
        vec![text(0, "Hello, "), text(0, "café — 日本語."), Event::Stop(StopReason::EndTurn), Event::Usage(usage),]
    );
    assert_eq!(t.message.blocks, vec![Block::Text { text: "Hello, café — 日本語.".into() }]);
    assert_eq!(t.stop, StopReason::EndTurn);
    assert_eq!(t.usage, usage);
}

#[test]
fn crlf_line_endings() {
    assert_eq!(turn(P, &TEXT.replace('\n', "\r\n")), turn(P, TEXT));
}

/// Two parallel calls whose pieces interleave, identified by `index`; the id
/// and name come only in each call's first piece.
const PARALLEL: &str = r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","content":null,"tool_calls":[{"index":0,"id":"call_A","type":"function","function":{"name":"list_paragraphs","arguments":""}}]},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_B","type":"function","function":{"name":"suggest_change","arguments":""}}]},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"speaker\": \"MA"}}]},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"paragraph_id\": 7, \"options\": [\"Ni"}}]},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"RÍA\"}"}}]},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"ño.\"]}"}}]},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}

data: {"choices":[],"usage":{"prompt_tokens":50,"completion_tokens":40,"total_tokens":90}}

data: [DONE]
"#;

#[test]
fn parallel_tool_calls_interleaved_by_index() {
    let (events, t) = run(P, PARALLEL);
    let t = t.unwrap();
    assert_eq!(t.stop, StopReason::ToolUse);
    assert_eq!(t.usage, Usage { input: 50, output: 40, ..Default::default() });
    assert_eq!(
        t.message.blocks,
        vec![
            Block::ToolCall(ToolCall {
                id: "call_A".into(),
                name: "list_paragraphs".into(),
                input: json!({"speaker": "MARÍA"}),
                input_error: None,
                raw_input: None,
                extra: None,
            }),
            Block::ToolCall(ToolCall {
                id: "call_B".into(),
                name: "suggest_change".into(),
                input: json!({"paragraph_id": 7, "options": ["Niño."]}),
                input_error: None,
                raw_input: None,
                extra: None,
            }),
        ]
    );
    assert_eq!(
        content(&events),
        vec![
            Event::ToolCallStarted { id: "call_A".into(), name: "list_paragraphs".into() },
            Event::ToolCallStarted { id: "call_B".into(), name: "suggest_change".into() },
        ]
    );
}

#[test]
fn whole_calls_without_index() {
    // Some providers send each call whole, in one piece, with no index.
    let body = r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"id":"c1","type":"function","function":{"name":"script_info","arguments":"{}"}},{"id":"c2","type":"function","function":{"name":"list_reviews","arguments":""}}]},"finish_reason":null}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"id":"c3","type":"function","function":{"name":"list_paragraphs","arguments":"{\"limit\": 5}"}}]},"finish_reason":"stop"}]}

data: [DONE]
"#;
    let t = turn(P, body);
    // "stop" while calling tools: the stop reason says so, the calls still count.
    assert_eq!(t.stop, StopReason::EndTurn);
    let calls: Vec<_> =
        t.message.tool_calls().into_iter().map(|c| (c.id.clone(), c.name.clone(), c.input.clone())).collect();
    assert_eq!(
        calls,
        vec![
            ("c1".into(), "script_info".into(), json!({})),
            ("c2".into(), "list_reviews".into(), json!({})),
            ("c3".into(), "list_paragraphs".into(), json!({"limit": 5})),
        ]
    );
}

#[test]
fn whole_calls_without_index_or_id() {
    let body = r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"type":"function","function":{"name":"script_info","arguments":"{}"}},{"type":"function","function":{"name":"list_paragraphs","arguments":"{\"kind\": \"Dialogue\"}"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]
"#;
    let t = turn(P, body);
    let calls = t.message.tool_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!((calls[0].id.as_str(), calls[0].name.as_str()), ("call_0", "script_info"));
    assert_eq!((calls[1].id.as_str(), calls[1].name.as_str()), ("call_1", "list_paragraphs"));
    assert_eq!(calls[1].input, json!({"kind": "Dialogue"}));
    assert!(calls.iter().all(|c| c.input_error.is_none()));
}

#[test]
fn streamed_call_without_index_repeating_its_id() {
    // No index, but the id repeated on every piece: one call, not three.
    let body = r#"data: {"choices":[{"delta":{"tool_calls":[{"id":"x","function":{"name":"list_paragraphs","arguments":"{\"scene\""}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"id":"x","function":{"arguments":": \"INT. "}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"id":"x","function":{"arguments":"KITCHEN\"}"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]
"#;
    let calls = turn(P, body).message.tool_calls().into_iter().cloned().collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].input, json!({"scene": "INT. KITCHEN"}));
}

#[test]
fn streamed_call_without_index_or_id_continues_the_last_call() {
    let body = r#"data: {"choices":[{"delta":{"tool_calls":[{"function":{"name":"list_paragraphs","arguments":"{\"limit\""}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"function":{"arguments":": 3}"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]
"#;
    let calls = turn(P, body).message.tool_calls().into_iter().cloned().collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call_0");
    assert_eq!(calls[0].input, json!({"limit": 3}));
}

#[test]
fn missing_ids_with_index_get_generated_ones() {
    let body = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"script_info","arguments":""}},{"index":1,"id":"","function":{"name":"list_reviews","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]
"#;
    let t = turn(P, body);
    let ids: Vec<_> = t.message.tool_calls().iter().map(|c| c.id.clone()).collect();
    assert_eq!(ids, ["call_0", "call_1"]);
}

#[test]
fn reasoning_content_becomes_unsigned_thinking() {
    let body = r#"data: {"choices":[{"delta":{"role":"assistant","reasoning_content":"Think "}}]}

data: {"choices":[{"delta":{"reasoning_content":"hard."}}]}

data: {"choices":[{"delta":{"reasoning":"More.","content":"Answer."}}]}

data: {"choices":[{"delta":{},"finish_reason":"stop"}]}

data: [DONE]
"#;
    let (events, t) = run(P, body);
    // The index moves on whenever the stream switches between reasoning and text.
    assert_eq!(
        content(&events),
        vec![thinking(0, "Think "), thinking(0, "hard."), text(1, "Answer."), thinking(2, "More.")]
    );
    assert_eq!(
        t.unwrap().message.blocks,
        vec![
            Block::Thinking { text: "Think hard.More.".into(), signature: None },
            Block::Text { text: "Answer.".into() }
        ]
    );
}

#[test]
fn finish_reasons() {
    for (wire, ours) in [
        ("stop", StopReason::EndTurn),
        ("tool_calls", StopReason::ToolUse),
        ("function_call", StopReason::ToolUse),
        ("length", StopReason::MaxTokens),
        ("content_filter", StopReason::Refusal),
        ("MALFORMED_FUNCTION_CALL", StopReason::Other("MALFORMED_FUNCTION_CALL".into())),
    ] {
        let body =
            format!("data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"{wire}\"}}]}}\n\ndata: [DONE]\n\n");
        assert_eq!(turn(P, &body).stop, ours, "{wire}");
    }
}

#[test]
fn done_without_finish_reason_is_end_turn() {
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\ndata: [DONE]\n\n";
    let t = turn(P, body);
    assert_eq!(t.stop, StopReason::EndTurn);
    assert_eq!(t.message.text(), "Hi");
}

#[test]
fn error_object_in_stream() {
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n\
                data: {\"error\":{\"message\":\"Rate limit exceeded\",\"code\":429}}\n\n";
    let (events, r) = run(P, body);
    assert_eq!(content(&events), vec![text(0, "Hi")]);
    // Gemini's numeric code is read as the HTTP status it is.
    assert_eq!(
        r.unwrap_err(),
        AgentError::Api {
            status: None,
            kind: ErrorKind::RateLimit,
            retryable: true,
            message: "Rate limit exceeded".into(),
            retry_after: None
        }
    );
    // An error without a message still reads as one.
    let body = "data: {\"error\":{\"code\":\"overloaded\"}}\n\n";
    let e = error(P, body);
    assert!(
        matches!(&e, AgentError::Api { kind: ErrorKind::Overloaded, message, .. } if message.contains("overloaded"))
    );
}

#[test]
fn null_error_field_is_not_an_error() {
    let body = "data: {\"error\":null,\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    assert_eq!(turn(P, body).message.text(), "ok");
}

#[test]
fn cut_off_stream_is_an_error() {
    let body = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"function\":{\"name\":\"script_info\",\"arguments\":\"{\"}}]}}]}\n\n";
    assert!(error(P, body).to_string().contains("cut off"));
}

#[test]
fn cut_off_tool_args_at_length() {
    let body = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"function\":{\"name\":\"suggest_change\",\"arguments\":\"{\\\"options\\\": [\\\"Hel\"}}]},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n";
    let t = turn(P, body);
    assert_eq!(t.stop, StopReason::MaxTokens);
    let c = t.message.tool_calls()[0].clone();
    assert_eq!(c.input, serde_json::Value::Null);
    assert!(c.input_error.is_some());
}

#[test]
fn comments_and_processing_notices_are_ignored() {
    // OpenRouter sends `: OPENROUTER PROCESSING` comments while it waits.
    let body = format!(": OPENROUTER PROCESSING\n\n: OPENROUTER PROCESSING\n\n{TEXT}");
    assert_eq!(turn(P, &body), turn(P, TEXT));
}

#[test]
fn gemini_extra_content_is_captured_and_merged() {
    // Gemini 3 attaches its thought signature to the call; a provider might split the object.
    let body = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"g1","type":"function","function":{"name":"script_info","arguments":""},"extra_content":{"google":{"thought_signature":"SIG=="}}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{}"},"extra_content":{"google":{"other":1}}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"g2","type":"function","function":{"name":"list_reviews","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]
"#;
    let t = turn(P, body);
    let calls = t.message.tool_calls();
    assert_eq!(calls[0].extra, Some(json!({"google": {"thought_signature": "SIG==", "other": 1}})));
    // Only the first parallel call carries one; none is invented for the rest.
    assert_eq!(calls[1].extra, None);
}

#[test]
fn refusal_text_is_surfaced_and_marks_a_refusal() {
    let body = r#"data: {"choices":[{"delta":{"role":"assistant","content":null,"refusal":""}}]}

data: {"choices":[{"delta":{"refusal":"I can't help "}}]}

data: {"choices":[{"delta":{"refusal":"with that."},"finish_reason":"stop"}]}

data: [DONE]
"#;
    let t = turn(P, body);
    assert_eq!(t.stop, StopReason::Refusal);
    assert_eq!(t.refusal.as_deref(), Some("I can't help with that."));
    assert!(t.message.blocks.is_empty());
    // A plain reply has none.
    assert_eq!(turn(P, TEXT).refusal, None);
}

#[test]
fn openrouter_reasoning_details_are_kept_as_a_native_block() {
    let body = r#"data: {"choices":[{"delta":{"reasoning":"Let me ","reasoning_details":[{"type":"reasoning.text","text":"Let me ","signature":null,"id":"r1","format":"anthropic-claude-v1","index":0}]}}]}

data: {"choices":[{"delta":{"reasoning":"think.","reasoning_details":[{"type":"reasoning.text","text":"think.","signature":null,"index":0}]}}]}

data: {"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","text":"","signature":"sig-r","index":0},{"type":"reasoning.encrypted","data":"enc","index":1}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"script_info","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]
"#;
    let t = turn(P, body);
    assert_eq!(
        t.message.blocks[0],
        Block::Native {
            protocol: P,
            value: json!({"reasoning_details": [
                {"type": "reasoning.text", "text": "Let me think.", "signature": "sig-r", "id": "r1", "format": "anthropic-claude-v1", "index": 0},
                {"type": "reasoning.encrypted", "data": "enc", "index": 1},
            ]}),
        }
    );
    assert_eq!(t.message.blocks[1], Block::Thinking { text: "Let me think.".into(), signature: None });
    assert_eq!(t.message.tool_calls().len(), 1);
}

#[test]
fn tool_name_repeated_on_every_piece_is_not_doubled() {
    let body = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"list_paragraphs","arguments":"{\"limit\""}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"list_paragraphs","arguments":": 2}"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]
"#;
    let (events, t) = run(P, body);
    let t = t.unwrap();
    let c = t.message.tool_calls()[0].clone();
    assert_eq!(c.name, "list_paragraphs");
    assert_eq!(c.input, json!({"limit": 2}));
    assert_eq!(content(&events), vec![Event::ToolCallStarted { id: "a".into(), name: "list_paragraphs".into() }]);
}

#[test]
fn a_new_id_at_a_used_index_starts_a_new_call() {
    // Some servers number every call 0.
    let body = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"script_info","arguments":"{}"}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"b","function":{"name":"list_reviews","arguments":"{"}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"}"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]
"#;
    let t = turn(P, body);
    let calls: Vec<_> =
        t.message.tool_calls().iter().map(|c| (c.id.clone(), c.name.clone(), c.input.clone())).collect();
    assert_eq!(
        calls,
        vec![("a".into(), "script_info".into(), json!({})), ("b".into(), "list_reviews".into(), json!({}))]
    );
}

#[test]
fn huge_tool_call_index_is_a_parse_error() {
    let body = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":4000000000,\"id\":\"a\",\"function\":{\"name\":\"x\"}}]}}]}\n\n";
    assert!(matches!(error(P, body), AgentError::Parse(_)));
    let ok = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1024,\"id\":\"a\",\"function\":{\"name\":\"x\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n";
    assert_eq!(turn(P, ok).message.tool_calls().len(), 1);
}

#[test]
fn invalid_args_keep_their_raw_text() {
    let body = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"function\":{\"name\":\"suggest_change\",\"arguments\":\"{\\\"options\\\": [\"}}]},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n";
    let c = turn(P, body).message.tool_calls()[0].clone();
    assert_eq!(c.raw_input.as_deref(), Some("{\"options\": ["));
    // Valid arguments don't keep a copy.
    assert_eq!(turn(P, PARALLEL).message.tool_calls()[0].raw_input, None);
}

#[test]
fn stream_without_usage_chunk_is_accepted_with_usage_unknown() {
    // Some servers ignore `include_usage`: the finish reason and [DONE] arrive, the usage never does.
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    let t = turn(P, body);
    assert_eq!(t.message.text(), "Hi");
    assert!(!t.usage.known);
    assert_eq!(t.usage, Usage::unknown());
    // With the chunk, it's known.
    assert!(turn(P, TEXT).usage.known);
}

#[test]
fn reasoning_tokens_are_reported_when_present() {
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"},\"finish_reason\":\"stop\"}]}\n\n\
                data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":30,\"completion_tokens_details\":{\"reasoning_tokens\":25}}}\n\ndata: [DONE]\n\n";
    assert_eq!(turn(P, body).usage, Usage { input: 5, output: 30, reasoning: Some(25), ..Default::default() });
    // Without the details, there's no reasoning count (not zero).
    assert_eq!(turn(P, PARALLEL).usage.reasoning, None);
}

#[test]
fn tool_args_events_report_progress() {
    let (events, _) = run(P, PARALLEL);
    let a: Vec<usize> = events
        .iter()
        .filter_map(|e| match e {
            Event::ToolArgs { id, bytes_so_far } if id == "call_A" => Some(*bytes_so_far),
            _ => None,
        })
        .collect();
    // Bytes of the argument text so far, as streamed.
    assert_eq!(a, vec!["{\"speaker\": \"MA".len(), "{\"speaker\": \"MARÍA\"}".len()]);
}

#[test]
fn abort_keeps_only_the_text() {
    let body = r#"data: {"choices":[{"delta":{"reasoning_content":"Hmm."}}]}

data: {"choices":[{"delta":{"content":"Half a sen"}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"script_info","arguments":"{"}}]}}]}

"#;
    let mut p = scriptorium_agent::ReplyParser::new(P);
    for line in body.split_inclusive('\n') {
        p.line(line).unwrap();
    }
    let t = p.abort();
    assert_eq!(t.stop, StopReason::Cancelled);
    assert_eq!(t.message.blocks, vec![Block::Text { text: "Half a sen".into() }]);
    assert!(!t.usage.known);
    // Nothing streamed: an empty reply.
    let t = scriptorium_agent::ReplyParser::new(P).abort();
    assert!(t.message.blocks.is_empty());
}

#[test]
fn openrouter_cost_is_kept() {
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"},\"finish_reason\":\"stop\"}]}\n\n\
                data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3,\"cost\":0.0125}}\n\ndata: [DONE]\n\n";
    assert_eq!(turn(P, body).usage, Usage { input: 5, output: 3, cost: Some(0.0125), ..Default::default() });
    assert_eq!(turn(P, TEXT).usage.cost, None);
}

#[test]
fn cached_tokens_above_the_prompt_count_are_clamped() {
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"},\"finish_reason\":\"stop\"}]}\n\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":50}}}\n\n\
data: [DONE]\n\n";
    let u = turn(P, body).usage;
    assert_eq!((u.input, u.cached, u.uncached_input()), (10, 10, 0));
}
