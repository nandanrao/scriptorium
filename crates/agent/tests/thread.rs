//! The provider-neutral thread: taking turns, usage, storage.

mod common;

use scriptorium_agent::protocol::{Call, build_request};
use scriptorium_agent::{
    AgentError, Block, Message, Origin, Protocol, Role, STOPPED, StopReason, Thread, ToolCall, ToolResult, Turn, Usage,
};
use serde_json::json;

fn call(id: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "script_info".into(),
        input: json!({}),
        input_error: None,
        raw_input: None,
        extra: None,
    }
}

fn result(id: &str) -> ToolResult {
    ToolResult { id: id.into(), content: "ok".into(), is_error: false }
}

fn reply(blocks: Vec<Block>, stop: StopReason, usage: Usage) -> Turn {
    Turn { message: Message::assistant(blocks), stop, usage, refusal: None }
}

fn usage(input: u64, output: u64, cached: u64) -> Usage {
    Usage { input, output, cached, ..Default::default() }
}

fn origin() -> Origin {
    Origin::new("u", "m")
}

#[test]
fn a_tool_loop() {
    let mut t = Thread::default();
    assert!(t.push_user("How long is it?"));
    assert!(t.unanswered_calls().is_empty());

    let p = t.push_turn(
        reply(
            vec![Block::Text { text: "Checking.".into() }, Block::ToolCall(call("a")), Block::ToolCall(call("b"))],
            StopReason::ToolUse,
            usage(100, 10, 0),
        ),
        origin(),
    );
    assert_eq!(p.calls, vec![call("a"), call("b")]);
    assert_eq!(p.withdrawn, None);
    let pending: Vec<_> = t.unanswered_calls().iter().map(|c| c.id.clone()).collect();
    assert_eq!(pending, ["a", "b"]);

    // Results go in call order, whatever order they come in.
    t.push_tool_results(vec![ToolResult { id: "b".into(), content: "no".into(), is_error: true }, result("a")])
        .unwrap();
    assert!(t.unanswered_calls().is_empty());
    assert_eq!(t.messages.last().unwrap().role, Role::Tool);
    let ids: Vec<_> = t.messages[2].tool_results().iter().map(|r| r.id.clone()).collect();
    assert_eq!(ids, ["a", "b"]);

    let p = t.push_turn(
        reply(vec![Block::Text { text: "120 pages.".into() }], StopReason::EndTurn, usage(150, 5, 100)),
        origin(),
    );
    assert!(p.calls.is_empty());
    assert_eq!(t.usage(), usage(250, 15, 100));
    assert_eq!(t.messages.len(), 4);
    assert_eq!(t.messages[3].text(), "120 pages.");
    // Each reply keeps its origin and usage.
    assert_eq!(t.messages[3].origin, Some(origin()));
    assert_eq!(t.messages[3].usage, Some(usage(150, 5, 100)));
    assert_eq!(t.messages[1].usage, Some(usage(100, 10, 0)));
}

#[test]
fn calls_count_even_when_the_provider_says_stop() {
    let mut t = Thread::default();
    let p = t.push_turn(reply(vec![Block::ToolCall(call("x"))], StopReason::EndTurn, Usage::default()), origin());
    assert_eq!(p.calls.len(), 1);
}

#[test]
fn message_text_joins_text_blocks_only() {
    let m = Message::assistant(vec![
        Block::Text { text: "a".into() },
        Block::Thinking { text: "hidden".into(), signature: None },
        Block::ToolCall(call("c")),
        Block::Notice { text: "not text".into() },
        Block::Text { text: "b".into() },
    ]);
    assert_eq!(m.text(), "ab");
    assert_eq!(m.tool_calls().len(), 1);
    assert!(!m.is_display_only());
    assert!(Message::assistant(vec![Block::Notice { text: "n".into() }]).is_display_only());
}

#[test]
fn serde_round_trip_is_lossless() {
    let mut t = Thread::default();
    assert!(t.push_user("Hi — 日本語"));
    t.push_turn(
        reply(
            vec![
                Block::Thinking { text: "t".into(), signature: Some("s".into()) },
                Block::Thinking { text: "u".into(), signature: None },
                Block::Native {
                    protocol: Protocol::Anthropic,
                    value: json!({"type": "redacted_thinking", "data": "d"}),
                },
                Block::Text { text: "Hello".into() },
                Block::Notice { text: "note".into() },
                Block::ToolCall(ToolCall {
                    id: "a".into(),
                    name: "suggest_change".into(),
                    input: json!({"paragraph_id": 3, "options": ["x"], "nested": {"k": [1, 2.5, null, true]}}),
                    input_error: None,
                    raw_input: None,
                    extra: None,
                }),
                Block::ToolCall(ToolCall {
                    id: "b".into(),
                    name: "list_paragraphs".into(),
                    input: serde_json::Value::Null,
                    input_error: Some("cut off".into()),
                    raw_input: None,
                    extra: None,
                }),
            ],
            StopReason::ToolUse,
            Usage { input: 9, output: 8, cached: 7, cache_write: 1, reasoning: Some(3), known: true, cost: None },
        ),
        origin(),
    );
    t.push_tool_results(vec![result("a"), ToolResult { id: "b".into(), content: "bad".into(), is_error: true }])
        .unwrap();
    t.push_turn(reply(vec![Block::Text { text: "x".into() }], StopReason::EndTurn, Usage::unknown()), origin());
    let s = serde_json::to_string(&t).unwrap();
    let back: Thread = serde_json::from_str(&s).unwrap();
    assert_eq!(back, t);
    // Stable on a second pass too.
    assert_eq!(serde_json::to_string(&back).unwrap(), s);
}

#[test]
fn stored_shape() {
    let mut t = Thread::default();
    assert!(t.push_user("Hi"));
    t.push_turn(reply(vec![Block::ToolCall(call("a"))], StopReason::ToolUse, usage(3, 2, 1)), origin());
    t.push_tool_results(vec![result("a")]).unwrap();
    assert_eq!(
        serde_json::to_value(&t).unwrap(),
        json!({
            "messages": [
                {"role": "user", "blocks": [{"type": "text", "text": "Hi"}]},
                {"role": "assistant",
                 "blocks": [{"type": "tool_call", "id": "a", "name": "script_info", "input": {}}],
                 "origin": {"endpoint": "u", "model": "m"},
                 "usage": {"input": 3, "output": 2, "cached": 1, "cache_write": 0}},
                {"role": "tool", "blocks": [{"type": "tool_result", "id": "a", "content": "ok"}]},
            ],
        })
    );
}

#[test]
fn minimal_threads_load() {
    // Messages without origin or usage, usage without `cached`.
    let old: Thread = serde_json::from_value(json!({"messages": []})).unwrap();
    assert_eq!(old, Thread::default());
    let old: Thread = serde_json::from_value(json!({
        "messages": [
            {"role": "user", "blocks": [{"type": "text", "text": "Hi"}]},
            {"role": "assistant", "blocks": [{"type": "thinking", "text": "t", "signature": "s"}, {"type": "text", "text": "Yo"}],
             "usage": {"input": 1, "output": 2}},
        ],
    }))
    .unwrap();
    assert_eq!(old.usage(), usage(1, 2, 0));
    assert_eq!(old.messages[1].origin, None);
    let mut t: Thread = serde_json::from_str(&serde_json::to_string(&old).unwrap()).unwrap();
    assert_eq!(t, old);
    assert!(t.push_user("More"));
    t.push_turn(reply(vec![Block::Text { text: "ok".into() }], StopReason::EndTurn, usage(10, 1, 0)), origin());
    assert_eq!(t.usage(), usage(11, 3, 0));
}

#[test]
fn usage_totals_add_cache_writes_reasoning_and_unknowns() {
    let mut a = Usage { input: 10, output: 5, cached: 2, cache_write: 3, reasoning: None, known: true, cost: None };
    a.add(Usage { input: 1, output: 1, cached: 0, cache_write: 1, reasoning: Some(4), known: true, cost: None });
    assert_eq!(
        a,
        Usage { input: 11, output: 6, cached: 2, cache_write: 4, reasoning: Some(4), known: true, cost: None }
    );
    a.add(Usage::unknown());
    assert!(!a.known);
    // A thread with one unknown reply has an unknown total.
    let mut t = Thread::default();
    assert!(t.push_user("Hi"));
    t.push_turn(reply(vec![Block::Text { text: "a".into() }], StopReason::EndTurn, usage(5, 1, 0)), origin());
    assert!(t.usage().known);
    assert!(t.push_user("Again"));
    t.push_turn(reply(vec![Block::Text { text: "b".into() }], StopReason::EndTurn, Usage::unknown()), origin());
    assert!(!t.usage().known);
    assert_eq!(t.usage().input, 5);
}

#[test]
fn a_parsed_reply_round_trips_through_storage_and_back_into_a_request() {
    // Anthropic reply with redacted thinking → thread → JSON → thread → request:
    // the native block is replayed to its own origin only, and never to OpenAI.
    let body = r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"abc"}}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":"Done."}}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}

event: message_stop
data: {"type":"message_stop"}
"#;
    let c = Call { base_url: "u/", api_key: "", model: "m", system: "", tools: &[], ..Default::default() };
    let mut t = Thread::default();
    assert!(t.push_user("Go"));
    t.push_turn(common::turn(Protocol::Anthropic, body), c.origin());
    assert!(t.push_user("Next"));
    let t: Thread = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();

    let a = build_request(Protocol::Anthropic, &c, &t).unwrap().body;
    assert_eq!(
        a["messages"][1]["content"],
        json!([{"type": "redacted_thinking", "data": "abc"}, {"type": "text", "text": "Done."}])
    );
    // Another model on the same endpoint gets the text only.
    let other = Call { model: "m2", ..c };
    let a = build_request(Protocol::Anthropic, &other, &t).unwrap().body;
    assert_eq!(a["messages"][1]["content"], json!([{"type": "text", "text": "Done."}]));
    let o = build_request(Protocol::OpenaiChat, &c, &t).unwrap().body;
    assert_eq!(o["messages"][1], json!({"role": "assistant", "content": "Done."}));
}

#[test]
fn a_refused_turn_withdraws_the_writers_message_and_leaves_a_notice() {
    let mut t = Thread::default();
    assert!(t.push_user("Hello."));
    t.push_turn(reply(vec![Block::Text { text: "Hi.".into() }], StopReason::EndTurn, usage(1, 1, 0)), origin());
    assert!(t.push_user("Do something bad."));
    let p = t.push_turn(
        Turn {
            message: Message::assistant(vec![Block::Text { text: "Sure, here".into() }, Block::ToolCall(call("x"))]),
            stop: StopReason::Refusal,
            usage: usage(10, 2, 0),
            refusal: Some("No.".into()),
        },
        origin(),
    );
    assert!(p.calls.is_empty());
    // Offered back to the writer to rephrase; gone from the thread.
    assert_eq!(p.withdrawn.as_deref(), Some("Do something bad."));
    assert_eq!(t.messages.len(), 3);
    let notice = &t.messages[2];
    assert_eq!(notice.blocks, vec![Block::Notice { text: "The model declined to answer: No.".into() }]);
    assert!(notice.is_display_only());
    // Its usage still counts.
    assert_eq!(t.usage(), usage(11, 3, 0));
    // Nothing to send until the writer says something.
    assert!(common::sendable(&t).is_err());
    assert!(t.push_user("Something else."));
    assert!(common::sendable(&t).is_ok());
    // The notice is never sent, to either protocol.
    let c = Call { base_url: "u", model: "m", ..Default::default() };
    let a = build_request(Protocol::Anthropic, &c, &t).unwrap().body;
    assert_eq!(a["messages"].as_array().unwrap().len(), 3);
    assert!(!a.to_string().contains("declined"));
    let o = build_request(Protocol::OpenaiChat, &c, &t).unwrap().body;
    assert!(!o.to_string().contains("declined"));
    // The notice survives storage, so a reopened thread shows the refusal.
    let back: Thread = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
    assert_eq!(back.messages[2], t.messages[2]);
}

#[test]
fn a_refusal_after_tool_results_withdraws_the_whole_tool_loop() {
    let mut t = Thread::default();
    assert!(t.push_user("Hello."));
    t.push_turn(reply(vec![Block::Text { text: "Hi.".into() }], StopReason::EndTurn, usage(1, 1, 0)), origin());
    assert!(t.push_user("Go."));
    t.push_turn(reply(vec![Block::ToolCall(call("a"))], StopReason::ToolUse, usage(5, 2, 0)), origin());
    t.push_tool_results(vec![result("a")]).unwrap();
    let p = t.push_turn(
        Turn { message: Message::assistant(vec![]), stop: StopReason::Refusal, usage: usage(7, 1, 0), refusal: None },
        origin(),
    );
    assert_eq!(p.withdrawn.as_deref(), Some("Go."));
    // Back to before "Go.": the tool call and its results are gone; the
    // withdrawn reply stays only as its cost, then the notice.
    assert_eq!(t.messages.len(), 4);
    assert_eq!(t.messages[1].text(), "Hi.");
    assert_eq!(t.messages[2].blocks, vec![]);
    assert_eq!(t.messages[2].usage, Some(usage(5, 2, 0)));
    assert_eq!(t.messages[3].blocks, vec![Block::Notice { text: "The model declined to answer.".into() }]);
    assert_eq!(t.usage(), usage(13, 4, 0));
    assert!(t.unanswered_calls().is_empty());
    assert!(common::sendable(&t).is_err());
    assert!(t.push_user("Rephrased."));
    assert!(common::sendable(&t).is_ok());
}

#[test]
fn a_refusal_with_no_writers_message_keeps_the_thread() {
    let mut t = Thread::default();
    t.push_turn(reply(vec![Block::ToolCall(call("a"))], StopReason::ToolUse, Usage::default()), origin());
    t.push_tool_results(vec![result("a")]).unwrap();
    let p = t.push_turn(
        Turn { message: Message::assistant(vec![]), stop: StopReason::Refusal, usage: Usage::default(), refusal: None },
        origin(),
    );
    assert_eq!(p.withdrawn, None);
    assert_eq!(t.messages.len(), 3);
    assert_eq!(t.messages[1].role, Role::Tool);
    assert_eq!(t.messages[2].blocks, vec![Block::Notice { text: "The model declined to answer.".into() }]);
}

#[test]
fn a_stopped_reply_keeps_its_text_and_drops_its_calls() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let p = t.push_turn(
        reply(
            vec![
                Block::Thinking { text: "x".into(), signature: Some("s".into()) },
                Block::Text { text: "Half".into() },
                Block::ToolCall(call("a")),
            ],
            StopReason::Cancelled,
            Usage::unknown(),
        ),
        origin(),
    );
    assert!(p.calls.is_empty());
    assert_eq!(t.messages[1].blocks, vec![Block::Text { text: "Half".into() }, Block::Notice { text: STOPPED.into() }]);
    assert!(t.unanswered_calls().is_empty());
    assert!(t.push_user("Try again."));
    assert!(common::sendable(&t).is_ok());
}

#[test]
fn sendable_only_when_not_ending_on_the_assistant() {
    let mut t = Thread::default();
    assert!(common::sendable(&t).is_err());
    assert!(t.push_user("Hi"));
    assert!(common::sendable(&t).is_ok());
    t.push_turn(reply(vec![Block::ToolCall(call("a"))], StopReason::ToolUse, Usage::default()), origin());
    assert!(common::sendable(&t).is_err());
    t.push_tool_results(vec![result("a")]).unwrap();
    assert!(common::sendable(&t).is_ok());
    t.push_turn(reply(vec![Block::Text { text: "Done.".into() }], StopReason::EndTurn, Usage::default()), origin());
    assert!(common::sendable(&t).is_err());
}

#[test]
fn every_tool_call_must_be_answered_by_the_next_message() {
    let not_ready = |t: &Thread| match common::sendable(t) {
        Err(AgentError::NotReady(m)) => m,
        other => panic!("{other:?}"),
    };
    // Calls answered by a writer's message instead of results.
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    t.messages.push(Message::assistant(vec![Block::ToolCall(call("a"))]));
    // Pushed directly: push_user would answer the call as cancelled first.
    t.messages.push(Message::user("Never mind."));
    assert!(not_ready(&t).contains("aren't answered"), "{}", not_ready(&t));
    // Results for other ids.
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    t.messages.push(Message::assistant(vec![Block::ToolCall(call("a")), Block::ToolCall(call("b"))]));
    t.messages.push(Message::new(Role::Tool, vec![Block::ToolResult(result("a"))]));
    assert!(not_ready(&t).contains("a, b"), "{}", not_ready(&t));
    // Results for no calls.
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    t.messages.push(Message::new(Role::Tool, vec![Block::ToolResult(result("a"))]));
    assert!(not_ready(&t).contains("no tool calls"), "{}", not_ready(&t));
    // A repeated id.
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    t.messages.push(Message::assistant(vec![Block::ToolCall(call("a")), Block::ToolCall(call("a"))]));
    t.messages.push(Message::new(Role::Tool, vec![Block::ToolResult(result("a")), Block::ToolResult(result("a"))]));
    assert!(not_ready(&t).contains("repeats"), "{}", not_ready(&t));
    // build_request refuses the same way.
    let c = Call { base_url: "u", model: "m", ..Default::default() };
    assert!(matches!(build_request(Protocol::Anthropic, &c, &t), Err(AgentError::NotReady(_))));
}

#[test]
fn blocks_in_the_wrong_role_are_refused() {
    let c = Call { base_url: "u", model: "m", ..Default::default() };
    let not_ready = |t: &Thread| match t.check_sendable(Protocol::OpenaiChat, &c.origin()) {
        Err(AgentError::NotReady(m)) => m,
        other => panic!("{other:?}"),
    };
    let with = |role: Role, block: Block| {
        let mut t = Thread::default();
        assert!(t.push_user("Go."));
        // Text keeps the message sent, so the block in it would go out.
        t.messages.push(Message::new(role, vec![Block::Text { text: "x".into() }, block]));
        t.messages.push(Message::user("And?"));
        t
    };
    for role in [Role::User, Role::Assistant] {
        let m = not_ready(&with(role, Block::ToolResult(result("a"))));
        assert!(m.contains("tool results but isn't a tool message"), "{role:?}: {m}");
    }
    for role in [Role::User, Role::Tool] {
        let m = not_ready(&with(role, Block::ToolCall(call("a"))));
        assert!(m.contains("tool calls but isn't the assistant's"), "{role:?}: {m}");
    }
    // build_request refuses the same way, for both protocols.
    for p in [Protocol::Anthropic, Protocol::OpenaiChat] {
        let t = with(Role::User, Block::ToolResult(result("a")));
        assert!(matches!(build_request(p, &c, &t), Err(AgentError::NotReady(_))));
    }
}

#[test]
fn a_writers_message_counts_only_its_text() {
    use scriptorium_agent::protocol::is_sent;
    let target = Origin::new("u", "m");
    // Only a (misplaced) tool result: nothing a provider would see.
    let m = Message::new(Role::User, vec![Block::ToolResult(result("a"))]);
    for p in [Protocol::Anthropic, Protocol::OpenaiChat] {
        assert!(!is_sent(p, Some(&target), &m));
        assert!(!is_sent(p, None, &m));
    }
    // So a thread ending on it ends on what came before.
    let mut t = Thread::default();
    assert!(t.push_user("Hi"));
    t.push_turn(reply(vec![Block::Text { text: "Hello.".into() }], StopReason::EndTurn, Usage::default()), origin());
    t.messages.push(m);
    let e = common::sendable(&t);
    assert!(matches!(&e, Err(AgentError::NotReady(m)) if m.contains("assistant")), "{e:?}");
    // A tool message's results still count.
    assert!(is_sent(Protocol::Anthropic, None, &Message::new(Role::Tool, vec![Block::ToolResult(result("a"))])));
}

#[test]
fn mismatched_tool_results_are_refused() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    assert!(matches!(t.push_tool_results(vec![result("a")]), Err(AgentError::Mismatch(_))));
    t.push_turn(
        reply(vec![Block::ToolCall(call("a")), Block::ToolCall(call("b"))], StopReason::ToolUse, Usage::default()),
        origin(),
    );
    for bad in [vec![result("a")], vec![result("a"), result("c")], vec![result("a"), result("b"), result("b")]] {
        assert!(matches!(t.push_tool_results(bad), Err(AgentError::Mismatch(_))));
    }
    assert_eq!(t.messages.len(), 2);
    t.push_tool_results(vec![result("a"), result("b")]).unwrap();
}

#[test]
fn tool_call_ids_are_unique_within_a_thread() {
    // A server that numbers calls per reply (or sends none) repeats ids across turns.
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let p1 = t.push_turn(
        reply(vec![Block::ToolCall(call("call_0")), Block::ToolCall(call(""))], StopReason::ToolUse, Usage::default()),
        origin(),
    );
    assert_eq!(p1.calls[0].id, "call_0");
    assert!(!p1.calls[1].id.is_empty());
    t.push_tool_results(p1.calls.iter().map(|c| result(&c.id)).collect()).unwrap();
    let p2 = t.push_turn(
        reply(
            vec![Block::ToolCall(call("call_0")), Block::ToolCall(call("call_0"))],
            StopReason::ToolUse,
            Usage::default(),
        ),
        origin(),
    );
    let mut all: Vec<String> = p1.calls.iter().chain(&p2.calls).map(|c| c.id.clone()).collect();
    assert_eq!(all.len(), 4);
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 4, "{all:?}");
    // The stored calls carry the new ids, so results pair up.
    t.push_tool_results(p2.calls.iter().map(|c| result(&c.id)).collect()).unwrap();
    assert!(common::sendable(&t).is_ok());
}

#[test]
fn new_tool_call_fields_round_trip_and_old_files_load() {
    let c = ToolCall {
        id: "a".into(),
        name: "x".into(),
        input: serde_json::Value::Null,
        input_error: Some("bad".into()),
        raw_input: Some("{\"a\": ".into()),
        extra: Some(json!({"google": {"thought_signature": "S"}})),
    };
    let back: ToolCall = serde_json::from_value(serde_json::to_value(&c).unwrap()).unwrap();
    assert_eq!(back, c);
    let old: ToolCall = serde_json::from_value(json!({"id": "a", "name": "x", "input": {}})).unwrap();
    assert_eq!((old.raw_input, old.extra), (None, None));
    assert!(!serde_json::to_string(&call("a")).unwrap().contains("extra"));
}

#[test]
fn uncached_input_and_sums_never_overflow_or_underflow() {
    let wrong = Usage { input: 5, cached: 9, cache_write: 3, ..Default::default() };
    assert_eq!(wrong.uncached_input(), 0);
    assert_eq!(Usage { input: 100, cached: 60, cache_write: 15, ..Default::default() }.uncached_input(), 25);
    let mut big = Usage { input: u64::MAX, ..Default::default() };
    big.add(Usage { input: 1, ..Default::default() });
    assert_eq!(big.input, u64::MAX);
}
