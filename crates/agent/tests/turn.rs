//! The loop helpers: invalid calls, cancelling, and the thread file.

mod common;

use scriptorium_agent::file::{ThreadFile, ToolsCheck, VERSION, check_tools, tools_hash};
use scriptorium_agent::turn::{CANCELLED, CANCELLED_NOTICE, cancel};
use scriptorium_agent::{
    AgentError, Block, Message, Origin, Role, StopReason, Thread, ToolCall, ToolResult, Turn, Usage,
};
use serde_json::{Value, json};

fn call(id: &str, error: Option<&str>) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "suggest_change".into(),
        input: if error.is_some() { Value::Null } else { json!({}) },
        input_error: error.map(String::from),
        raw_input: None,
        extra: None,
    }
}

fn tool_turn(calls: Vec<ToolCall>) -> Turn {
    Turn {
        message: Message::assistant(calls.into_iter().map(Block::ToolCall).collect()),
        stop: StopReason::ToolUse,
        usage: Usage::default(),
        refusal: None,
    }
}

#[test]
fn cancel_answers_waiting_calls() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    assert_eq!(cancel(&mut t), 0);
    assert_eq!(t.messages.len(), 1);
    t.push_turn(tool_turn(vec![call("a", None), call("b", None)]), Origin::new("u", "m"));
    assert!(common::sendable(&t).is_err());
    assert_eq!(cancel(&mut t), 2);
    let m = t.messages.last().unwrap();
    assert_eq!(m.role, Role::Tool);
    assert!(m.tool_results().iter().all(|r| r.is_error && r.content == CANCELLED));
    assert_eq!(m.blocks.last(), Some(&Block::Notice { text: CANCELLED_NOTICE.into() }));
    assert!(common::sendable(&t).is_ok());
    assert!(t.unanswered_calls().is_empty());
    assert_eq!(cancel(&mut t), 0);
}

fn file() -> ThreadFile {
    let mut thread = Thread::default();
    assert!(thread.push_user("Hi"));
    ThreadFile {
        version: VERSION,
        id: "t1".into(),
        title: "Tighten act one".into(),
        created: "2026-11-14T10:00:00Z".into(),
        updated: "2026-11-14T10:05:00Z".into(),
        account: "acct-1".into(),
        model: "claude-opus-5-5".into(),
        review_id: None,
        review_path: None,
        whole_script: None,
        system: "You are the assistant.".into(),
        tools_hash: tools_hash(&[]),
        thread,
    }
}

#[test]
fn thread_file_round_trips() {
    let f = file();
    let back = ThreadFile::from_json(&f.to_json()).unwrap();
    assert_eq!(back, f);
    let f = ThreadFile { review_id: Some("r9".into()), ..file() };
    assert_eq!(ThreadFile::from_json(&f.to_json()).unwrap(), f);
    let f = ThreadFile { review_path: Some("Tighten act one.review.txt".into()), whole_script: Some(false), ..file() };
    assert_eq!(ThreadFile::from_json(&f.to_json()).unwrap(), f);
    // Unset, the new fields aren't written, so older readers see the same file.
    let j = file().to_json();
    assert!(!j.contains("review_path") && !j.contains("whole_script"), "{j}");
}

#[test]
fn thread_file_refuses_other_versions_and_garbage() {
    for v in [0, VERSION + 1] {
        let mut j: Value = serde_json::from_str(&file().to_json()).unwrap();
        j["version"] = json!(v);
        let e = ThreadFile::from_json(&j.to_string()).unwrap_err();
        assert!(matches!(&e, AgentError::File(m) if m.contains("version")), "{e:?}");
    }
    assert!(matches!(ThreadFile::from_json("{}"), Err(AgentError::File(_))));
    assert!(matches!(ThreadFile::from_json("not json"), Err(AgentError::File(_))));
    // A thread-wide `usage` (never written by any release) is ignored.
    let mut j: Value = serde_json::from_str(&file().to_json()).unwrap();
    j["thread"]["usage"] = json!({"input": 5, "output": 1});
    let f = ThreadFile::from_json(&j.to_string()).unwrap();
    assert_eq!(f.thread.usage(), Usage::default());
}

#[test]
fn tools_hash_is_stable_and_changes_with_the_tools() {
    use scriptorium_agent::ToolSpec;
    let spec = |name: &str, desc: &str| ToolSpec {
        name: name.into(),
        description: desc.into(),
        schema: json!({"type": "object"}),
    };
    let a = vec![spec("a", "one"), spec("b", "two")];
    let h = tools_hash(&a);
    assert_eq!(h.len(), 16);
    assert_eq!(h, tools_hash(&a.clone()));
    assert_ne!(h, tools_hash(&[spec("a", "one"), spec("b", "two!")]));
    assert_ne!(h, tools_hash(&[spec("b", "two"), spec("a", "one")]));
    assert_ne!(h, tools_hash(&[]));
}

#[test]
fn a_crash_before_tool_results_does_not_strand_the_thread() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    t.push_turn(tool_turn(vec![call("a", None), call("b", None)]), Origin::new("u", "m"));
    // Crash: the thread was saved with its calls unanswered, then reloaded.
    let mut t: Thread = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
    assert!(t.push_user("Hello again?"));
    assert!(common::sendable(&t).is_ok(), "{:?}", common::sendable(&t));
    assert_eq!(t.messages.len(), 4);
    assert_eq!(t.messages[2].role, Role::Tool);
    assert!(t.messages[2].tool_results().iter().all(|r| r.content == CANCELLED));
    assert_eq!(t.messages[3].text(), "Hello again?");
}

#[test]
fn cancel_repairs_calls_stranded_behind_a_later_message() {
    // As an older build could leave it: calls, then the writer's next message.
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    t.push_turn(tool_turn(vec![call("a", None)]), Origin::new("u", "m"));
    t.messages.push(Message::user("Hello?"));
    assert!(common::sendable(&t).is_err());
    let pending: Vec<_> = t.unanswered_calls().iter().map(|c| c.id.clone()).collect();
    assert_eq!(pending, ["a"]);
    assert_eq!(cancel(&mut t), 1);
    assert!(common::sendable(&t).is_ok(), "{:?}", common::sendable(&t));
    assert_eq!(t.messages[2].role, Role::Tool);
    assert_eq!(t.messages[3].text(), "Hello?");
    assert_eq!(cancel(&mut t), 0);

    // Partly answered: the missing result joins the tool message, in call order.
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    t.push_turn(tool_turn(vec![call("a", None), call("b", None)]), Origin::new("u", "m"));
    t.messages.push(Message::new(
        Role::Tool,
        vec![Block::ToolResult(ToolResult { id: "b".into(), content: "ok".into(), is_error: false })],
    ));
    assert_eq!(cancel(&mut t), 1);
    assert!(common::sendable(&t).is_ok(), "{:?}", common::sendable(&t));
    let ids: Vec<_> = t.messages[2].tool_results().iter().map(|r| r.id.clone()).collect();
    assert_eq!(ids, ["a", "b"]);
    assert_eq!(t.messages.len(), 3);
}

#[test]
fn blank_writers_messages_are_refused_and_never_sendable() {
    let mut t = Thread::default();
    assert!(!t.push_user(""));
    assert!(!t.push_user(" \n\t"));
    assert!(t.messages.is_empty());
    assert!(t.push_user("Hi"));
    t.push_turn(
        Turn {
            message: Message::assistant(vec![Block::Text { text: "Hello.".into() }]),
            stop: StopReason::EndTurn,
            usage: Usage::default(),
            refusal: None,
        },
        Origin::new("u", "m"),
    );
    // One in a stored thread still doesn't make it sendable.
    t.messages.push(Message::user("  "));
    assert!(matches!(common::sendable(&t), Err(AgentError::NotReady(m)) if m.contains("assistant")));
}

#[test]
fn replies_list_each_origin_with_its_usage_and_costs_sum_when_all_known() {
    let mut t = Thread::default();
    let reply = |cost: Option<f64>| Turn {
        message: Message::assistant(vec![Block::Text { text: "ok".into() }]),
        stop: StopReason::EndTurn,
        usage: Usage { input: 10, output: 1, cost, ..Default::default() },
        refusal: None,
    };
    assert!(t.push_user("a"));
    t.push_turn(reply(Some(0.5)), Origin::new("u", "m1"));
    assert!(t.push_user("b"));
    t.push_turn(reply(Some(0.25)), Origin::new("u", "m2"));
    let r: Vec<_> = t.replies().map(|(o, u)| (o.model.clone(), u.input)).collect();
    assert_eq!(r, [("m1".to_string(), 10), ("m2".to_string(), 10)]);
    assert_eq!(t.usage().cost, Some(0.75));
    assert_eq!(t.usage().input, 20);
    assert!(t.push_user("c"));
    t.push_turn(reply(None), Origin::new("u", "m1"));
    assert_eq!(t.usage().cost, None);
    assert_eq!(Thread::default().usage(), Usage::default());
}

#[test]
fn check_tools_compares_with_the_tools_the_thread_started_with() {
    use scriptorium_agent::ToolSpec;
    let tools = vec![ToolSpec { name: "a".into(), description: "one".into(), schema: json!({"type": "object"}) }];
    let f = ThreadFile { tools_hash: tools_hash(&tools), ..file() };
    assert_eq!(check_tools(&f, &tools), ToolsCheck::Same);
    assert_eq!(check_tools(&f, &[]), ToolsCheck::Changed);
    // A file from before tools were recorded has nothing to compare.
    let old = ThreadFile { tools_hash: String::new(), ..file() };
    assert_eq!(check_tools(&old, &tools), ToolsCheck::Same);
}
