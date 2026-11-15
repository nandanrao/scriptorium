//! The pure loop driver, run end to end with fake replies and tools.

mod common;

use scriptorium_agent::turn::{
    Action, CANCELLED, CANCELLED_NOTICE, Input, Limits, MAX_RETRY_AFTER_SECS, Outcome, ROUND_LIMIT, ROUND_LIMIT_NOTICE,
    Run,
};
use scriptorium_agent::{
    AgentError, Block, ErrorKind, Message, Origin, Protocol, Role, StopReason, Thread, ToolCall, ToolResult, Turn,
    Usage,
};
use serde_json::{Value, json};

fn call(id: &str, bad: bool) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "script_info".into(),
        input: if bad { Value::Null } else { json!({}) },
        input_error: bad.then(|| "cut off".to_string()),
        raw_input: None,
        extra: None,
    }
}

fn tools(calls: Vec<ToolCall>) -> Turn {
    Turn {
        message: Message::assistant(calls.into_iter().map(Block::ToolCall).collect()),
        stop: StopReason::ToolUse,
        usage: Usage::default(),
        refusal: None,
    }
}

fn text(t: &str, stop: StopReason) -> Turn {
    Turn {
        message: Message::assistant(vec![Block::Text { text: t.into() }]),
        stop,
        usage: Usage::default(),
        refusal: None,
    }
}

fn origin() -> Origin {
    Origin::new("u", "m")
}

fn ok(calls: &[ToolCall]) -> Vec<ToolResult> {
    calls.iter().map(|c| ToolResult { id: c.id.clone(), content: "ok".into(), is_error: false }).collect()
}

/// Runs the loop the way a shell does: `replies` are what the provider sends
/// in turn, tools always succeed. Returns the outcome and how many requests were sent.
fn drive(thread: &mut Thread, limits: Limits, mut replies: Vec<Input>) -> (Outcome, usize) {
    replies.reverse();
    let mut run = Run::new(limits, Protocol::Anthropic, origin());
    let mut action = run.start(thread);
    let mut sent = 0;
    loop {
        let input = match action {
            Action::Send => {
                assert!(common::sendable(thread).is_ok(), "{:?}", common::sendable(thread));
                sent += 1;
                replies.pop().expect("a reply for every request")
            }
            Action::RunTools(calls) => {
                assert!(calls.iter().all(|c| c.input_error.is_none()));
                Input::Results(ok(&calls))
            }
            Action::Wait { .. } => Input::Retry,
            Action::Done(outcome) => return (outcome, sent),
        };
        action = run.next(thread, input);
    }
}

fn limits() -> Limits {
    Limits { max_tool_rounds: 5, ..Limits::default() }
}

#[test]
fn a_tool_loop_runs_to_the_end() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let replies = vec![
        Input::Reply(tools(vec![call("a", false), call("b", false)])),
        Input::Reply(tools(vec![call("c", false)])),
        Input::Reply(text("Done.", StopReason::EndTurn)),
    ];
    assert_eq!(drive(&mut t, limits(), replies), (Outcome::Finished, 3));
    assert_eq!(t.messages.len(), 6);
    assert_eq!(t.messages[5].text(), "Done.");
}

#[test]
fn calls_with_bad_arguments_are_answered_without_running() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    // Only bad calls: answered at once, straight back to the model.
    let replies = vec![
        Input::Reply(tools(vec![call("a", true)])),
        Input::Reply(tools(vec![call("b", true), call("c", false)])),
        Input::Reply(text("Done.", StopReason::EndTurn)),
    ];
    assert_eq!(drive(&mut t, limits(), replies), (Outcome::Finished, 3));
    let r = t.messages[2].tool_results();
    assert!(r[0].is_error && r[0].content.contains("cut off"));
    // Mixed: the bad call's error goes with the good call's result.
    let r = t.messages[4].tool_results();
    assert_eq!((r[0].id.as_str(), r[0].is_error), ("b", true));
    assert_eq!((r[1].id.as_str(), r[1].is_error), ("c", false));
}

#[test]
fn stopping_while_tools_run_cancels_the_calls_without_results() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let mut run = Run::new(limits(), Protocol::Anthropic, origin());
    assert_eq!(run.start(&mut t), Action::Send);
    let a = run.next(&mut t, Input::Reply(tools(vec![call("a", false), call("b", true)])));
    assert!(matches!(&a, Action::RunTools(c) if c.len() == 1));
    assert_eq!(run.next(&mut t, Input::Stop(vec![])), Action::Done(Outcome::Stopped));
    let r = t.messages[2].tool_results();
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].content, CANCELLED);
    // The bad call keeps the error it was already answered with.
    assert!(r[1].is_error && r[1].content.contains("cut off"));
    assert_eq!(t.messages[2].blocks.last(), Some(&Block::Notice { text: CANCELLED_NOTICE.into() }));
    assert!(common::sendable(&t).is_ok());
}

#[test]
fn stopping_keeps_the_results_of_tools_that_ran() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let mut run = Run::new(limits(), Protocol::Anthropic, origin());
    run.start(&mut t);
    let a = run.next(&mut t, Input::Reply(tools(vec![call("a", false), call("b", false), call("c", false)])));
    let Action::RunTools(calls) = a else { panic!("{a:?}") };
    let mut finished = ok(&calls[..1]);
    // Results for calls that aren't waiting are dropped.
    finished.push(ToolResult { id: "zzz".into(), content: "stray".into(), is_error: false });
    assert_eq!(run.next(&mut t, Input::Stop(finished)), Action::Done(Outcome::Stopped));
    let r = t.messages[2].tool_results();
    let got: Vec<_> = r.iter().map(|r| (r.id.as_str(), r.content.as_str())).collect();
    assert_eq!(got, [("a", "ok"), ("b", CANCELLED), ("c", CANCELLED)]);
    assert!(common::sendable(&t).is_ok());
    assert!(!run.is_active());

    // Every tool finished: no cancelled notice.
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let mut run = Run::new(limits(), Protocol::Anthropic, origin());
    run.start(&mut t);
    let Action::RunTools(calls) = run.next(&mut t, Input::Reply(tools(vec![call("a", false)]))) else { panic!() };
    assert_eq!(run.next(&mut t, Input::Stop(ok(&calls))), Action::Done(Outcome::Stopped));
    assert!(t.messages[2].blocks.iter().all(|b| !matches!(b, Block::Notice { .. })));
    assert!(common::sendable(&t).is_ok());
}

#[test]
fn stopping_while_a_request_is_sent_leaves_the_thread_unchanged() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let mut run = Run::new(limits(), Protocol::Anthropic, origin());
    assert_eq!(run.start(&mut t), Action::Send);
    let before = t.clone();
    assert_eq!(run.next(&mut t, Input::Stop(vec![])), Action::Done(Outcome::Stopped));
    assert_eq!(t, before);
}

#[test]
fn stopping_while_streaming_keeps_the_text() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let replies = vec![Input::Reply(text("Half", StopReason::Cancelled))];
    assert_eq!(drive(&mut t, limits(), replies), (Outcome::Stopped, 1));
    assert_eq!(t.messages[1].text(), "Half");
}

#[test]
fn a_refusal_withdraws_the_writers_message() {
    let mut t = Thread::default();
    assert!(t.push_user("Do something bad."));
    let replies = vec![Input::Reply(tools(vec![call("a", false)])), Input::Reply(text("", StopReason::Refusal))];
    assert_eq!(drive(&mut t, limits(), replies), (Outcome::Withdrawn("Do something bad.".into()), 2));
    assert!(t.unanswered_calls().is_empty());
    assert!(t.messages.iter().all(|m| m.role == Role::Assistant));
}

#[test]
fn the_round_limit_answers_the_calls_and_ends() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let replies = vec![
        Input::Reply(tools(vec![call("a", false)])),
        Input::Reply(tools(vec![call("b", false)])),
        Input::Reply(tools(vec![call("c", false), call("d", true)])),
    ];
    assert_eq!(drive(&mut t, Limits { max_tool_rounds: 2, ..Limits::default() }, replies), (Outcome::RoundLimit, 3));
    let last = t.messages.last().unwrap();
    assert_eq!(last.role, Role::Tool);
    assert!(last.tool_results().iter().all(|r| r.is_error && r.content == ROUND_LIMIT));
    assert_eq!(last.tool_results().len(), 2);
    assert_eq!(last.blocks.last(), Some(&Block::Notice { text: ROUND_LIMIT_NOTICE.into() }));
    // The writer can carry on.
    assert!(t.push_user("Keep going."));
    assert!(common::sendable(&t).is_ok());
}

fn offline() -> AgentError {
    AgentError::offline("the reply was cut off")
}

fn api(retryable: bool, retry_after: Option<u64>) -> AgentError {
    AgentError::Api { status: Some(529), kind: ErrorKind::Overloaded, retryable, message: "busy".into(), retry_after }
}

#[test]
fn limits_default_to_25_rounds_and_3_retries() {
    assert_eq!(Limits::default(), Limits { max_tool_rounds: 25, max_retries: 3 });
}

#[test]
fn a_retryable_failure_waits_and_resends_until_retries_run_out() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let mut run = Run::new(limits(), Protocol::Anthropic, origin());
    assert_eq!(run.start(&mut t), Action::Send);
    let a = run.next(&mut t, Input::Reply(tools(vec![call("a", false)])));
    let Action::RunTools(calls) = a else { panic!("{a:?}") };
    assert_eq!(run.next(&mut t, Input::Results(ok(&calls))), Action::Send);
    let before = t.clone();
    // Backoff 1, 2, 4 seconds, then the error.
    for secs in [1, 2, 4] {
        assert_eq!(run.next(&mut t, Input::Failed(offline())), Action::Wait { secs });
        assert!(run.is_active());
        assert_eq!(run.next(&mut t, Input::Retry), Action::Send);
    }
    assert_eq!(run.next(&mut t, Input::Failed(offline())), Action::Done(Outcome::Error(offline())));
    assert!(!run.is_active());
    assert_eq!(t, before);
    // Resend by hand: the run carries on where it was, with fresh retries.
    assert_eq!(run.start(&mut t), Action::Send);
    assert_eq!(run.next(&mut t, Input::Failed(offline())), Action::Wait { secs: 1 });
    assert_eq!(run.next(&mut t, Input::Retry), Action::Send);
    assert_eq!(run.next(&mut t, Input::Reply(text("Done.", StopReason::EndTurn))), Action::Done(Outcome::Finished));
    assert_eq!(run.rounds(), 1);
}

#[test]
fn retries_honour_retry_after_cap_the_backoff_and_reset_after_a_reply() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let mut run = Run::new(Limits { max_retries: 8, ..limits() }, Protocol::Anthropic, origin());
    run.start(&mut t);
    assert_eq!(run.next(&mut t, Input::Failed(api(true, Some(7)))), Action::Wait { secs: 7 });
    let mut waits = vec![];
    for _ in 0..7 {
        run.next(&mut t, Input::Retry);
        let Action::Wait { secs } = run.next(&mut t, Input::Failed(offline())) else { panic!() };
        waits.push(secs);
    }
    assert_eq!(waits, [2, 4, 8, 16, 30, 30, 30]);
    // A reply resets the count for the next request.
    run.next(&mut t, Input::Retry);
    let Action::RunTools(calls) = run.next(&mut t, Input::Reply(tools(vec![call("a", false)]))) else { panic!() };
    run.next(&mut t, Input::Results(ok(&calls)));
    assert_eq!(run.next(&mut t, Input::Failed(offline())), Action::Wait { secs: 1 });
    // Stopping during the wait ends the run.
    assert_eq!(run.next(&mut t, Input::Stop(vec![])), Action::Done(Outcome::Stopped));
    assert!(common::sendable(&t).is_ok());
}

#[test]
fn a_retry_after_of_zero_still_waits() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let mut run = Run::new(limits(), Protocol::Anthropic, origin());
    run.start(&mut t);
    assert_eq!(run.next(&mut t, Input::Failed(api(true, Some(0)))), Action::Wait { secs: 1 });
}

#[test]
fn a_retry_after_beyond_the_cap_ends_the_run_with_the_error() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let mut run = Run::new(limits(), Protocol::Anthropic, origin());
    run.start(&mut t);
    let at_cap = api(true, Some(MAX_RETRY_AFTER_SECS));
    assert_eq!(run.next(&mut t, Input::Failed(at_cap)), Action::Wait { secs: MAX_RETRY_AFTER_SECS });
    assert_eq!(run.next(&mut t, Input::Retry), Action::Send);
    let beyond = api(true, Some(MAX_RETRY_AFTER_SECS + 1));
    assert_eq!(run.next(&mut t, Input::Failed(beyond.clone())), Action::Done(Outcome::Error(beyond)));
    assert!(!run.is_active());
    let huge = api(true, Some(u64::MAX));
    run.start(&mut t);
    assert_eq!(run.next(&mut t, Input::Failed(huge.clone())), Action::Done(Outcome::Error(huge)));
}

#[test]
fn a_failure_that_is_not_retryable_ends_the_run_at_once() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let mut run = Run::new(limits(), Protocol::Anthropic, origin());
    run.start(&mut t);
    for e in [api(false, Some(5)), AgentError::Parse("bad".into())] {
        assert_eq!(run.next(&mut t, Input::Failed(e.clone())), Action::Done(Outcome::Error(e)));
    }
    let mut run = Run::new(Limits { max_retries: 0, ..limits() }, Protocol::Anthropic, origin());
    run.start(&mut t);
    assert_eq!(run.next(&mut t, Input::Failed(offline())), Action::Done(Outcome::Error(offline())));
}

#[test]
fn is_active_from_the_first_action_until_done() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let mut run = Run::new(limits(), Protocol::Anthropic, origin());
    assert!(!run.is_active());
    run.start(&mut t);
    assert!(run.is_active());
    assert!(matches!(run.next(&mut t, Input::Reply(tools(vec![call("a", false)]))), Action::RunTools(_)));
    assert!(run.is_active());
    run.next(&mut t, Input::Stop(vec![]));
    assert!(!run.is_active());
    assert!(!Run::new(limits(), Protocol::Anthropic, origin()).start(&mut Thread::default()).eq(&Action::Send));
}

#[test]
fn start_cancels_calls_left_waiting_and_sends() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    // Saved mid-run: a reply with calls and no results.
    let _ = t.push_turn(tools(vec![call("a", false), call("b", false)]), origin());
    let mut run = Run::new(limits(), Protocol::Anthropic, origin());
    assert_eq!(run.start(&mut t), Action::Send);
    assert!(run.is_active());
    assert!(t.messages[2].tool_results().iter().all(|r| r.content == CANCELLED));
    assert_eq!(t.messages[2].blocks.last(), Some(&Block::Notice { text: CANCELLED_NOTICE.into() }));
    assert_eq!(run.next(&mut t, Input::Reply(text("Done.", StopReason::EndTurn))), Action::Done(Outcome::Finished));
    // A finished thread has nothing to send, and is left as it is.
    let before = t.clone();
    let a = run.start(&mut t);
    assert!(matches!(&a, Action::Done(Outcome::Error(AgentError::NotReady(m))) if m.contains("assistant")), "{a:?}");
    assert_eq!(t, before);
}

#[test]
fn start_mid_run_keeps_the_bad_argument_answers() {
    // The shell abandons the tool run and starts again: the call whose
    // arguments didn't parse keeps its error, only the other is cancelled.
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let mut run = Run::new(limits(), Protocol::Anthropic, origin());
    run.start(&mut t);
    let a = run.next(&mut t, Input::Reply(tools(vec![call("a", true), call("b", false)])));
    assert!(matches!(&a, Action::RunTools(c) if c.len() == 1));
    assert_eq!(run.start(&mut t), Action::Send);
    let r = t.messages[2].tool_results();
    assert_eq!((r[0].id.as_str(), r[0].is_error), ("a", true));
    assert!(r[0].content.contains("cut off"));
    assert_eq!((r[1].id.as_str(), r[1].content.as_str()), ("b", CANCELLED));
    assert!(common::sendable(&t).is_ok());
    // Nothing is left over for a later stop to add.
    assert_eq!(run.next(&mut t, Input::Stop(vec![])), Action::Done(Outcome::Stopped));
    assert_eq!(t.messages[2].tool_results().len(), 2);
}

#[test]
fn mismatched_results_end_the_run_and_start_recovers() {
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let mut run = Run::new(limits(), Protocol::Anthropic, origin());
    run.start(&mut t);
    let a = run.next(&mut t, Input::Reply(tools(vec![call("a", true), call("b", false)])));
    let Action::RunTools(_) = a else { panic!("{a:?}") };
    let before = t.clone();
    let wrong = vec![ToolResult { id: "zzz".into(), content: "ok".into(), is_error: false }];
    let a = run.next(&mut t, Input::Results(wrong));
    assert!(matches!(a, Action::Done(Outcome::Error(AgentError::Mismatch(_)))));
    assert!(!run.is_active());
    assert_eq!(t, before);
    // Recovery is `start`: the bad-argument call keeps its parse error, the
    // call that should have run is answered as cancelled.
    assert_eq!(run.start(&mut t), Action::Send);
    let r = t.messages[2].tool_results();
    assert_eq!(r.len(), 2);
    let a = r.iter().find(|r| r.id == "a").unwrap();
    assert!(a.is_error && a.content.contains("cut off"), "{a:?}");
    let b = r.iter().find(|r| r.id == "b").unwrap();
    assert!(b.is_error && b.content == CANCELLED);
    assert!(common::sendable(&t).is_ok());
}

#[test]
fn the_run_checks_the_thread_against_its_own_target() {
    // A reply that is only signed thinking is sent back to its own origin
    // (so the thread would end on it) but to nothing else.
    let mut t = Thread::default();
    assert!(t.push_user("Go."));
    let turn = Turn {
        message: Message::assistant(vec![Block::Thinking { text: "hm".into(), signature: Some("s".into()) }]),
        stop: StopReason::EndTurn,
        usage: Usage::default(),
        refusal: None,
    };
    let _ = t.push_turn(turn, origin());
    let mut same = Run::new(limits(), Protocol::Anthropic, origin());
    assert!(matches!(same.start(&mut t), Action::Done(Outcome::Error(AgentError::NotReady(_)))));
    let mut other = Run::new(limits(), Protocol::Anthropic, Origin::new("u", "other"));
    assert_eq!(other.start(&mut t), Action::Send);
    let mut openai = Run::new(limits(), Protocol::OpenaiChat, origin());
    assert_eq!(openai.start(&mut t), Action::Send);
}

#[test]
fn wrong_results_and_unsendable_threads_are_errors() {
    let mut t = Thread::default();
    let mut run = Run::new(limits(), Protocol::Anthropic, origin());
    assert!(matches!(run.start(&mut t), Action::Done(Outcome::Error(AgentError::NotReady(_)))));
    assert!(t.push_user("Go."));
    run.next(&mut t, Input::Reply(tools(vec![call("a", false)])));
    let wrong = vec![ToolResult { id: "zzz".into(), content: "ok".into(), is_error: false }];
    assert!(matches!(run.next(&mut t, Input::Results(wrong)), Action::Done(Outcome::Error(AgentError::Mismatch(_)))));
}
