//! Whole runs of the agent loop against cassettes: request assembly, the
//! replay transport, the reply parser, retries with scaled waits, and the
//! shared error mapping. No network.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use scriptorium_agent::turn::{Action, Input, Limits, Outcome, Run};
use scriptorium_agent::{AgentError, ErrorKind, Protocol, ReplyParser, Thread, ToolResult};
use scriptorium_providers::shell::{Flow, Replay, TimeScale, Transport};
use scriptorium_providers::{Account, Provider, Resolved, build_request, resolve};

fn cassette(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cassettes").join(name)
}

/// One request through `transport`, parsed.
fn send(transport: &dyn Transport, resolved: &Resolved, thread: &Thread, texts: &mut Vec<String>) -> Input {
    let req = build_request(resolved, Some("sk-test"), "You help.", &[], thread).unwrap();
    let mut parser = ReplyParser::new(resolved.protocol);
    let mut failed = None;
    let sent = transport.send(&req, &mut |line| match parser.line(line) {
        Ok(events) => {
            for e in events {
                if let scriptorium_agent::Event::Text { text, .. } = e {
                    texts.push(text);
                }
            }
            Flow::Continue
        }
        Err(e) => {
            failed = Some(e);
            Flow::Stop
        }
    });
    match (sent, failed) {
        (_, Some(e)) | (Err(e), None) => Input::Failed(e),
        (Ok(()), None) => match parser.finish() {
            Ok(turn) => Input::Reply(turn),
            Err(e) => Input::Failed(e),
        },
    }
}

/// Runs the loop to the end; tools answer from `tool`. Returns the outcome
/// and the waits it was asked for.
fn run(
    transport: &dyn Transport,
    resolved: &Resolved,
    thread: &mut Thread,
    time: TimeScale,
    tool: impl Fn(&str) -> String,
) -> (Outcome, Vec<u64>, Vec<String>) {
    let mut r = Run::new(Limits::default(), resolved.protocol, resolved.origin());
    let mut action = r.start(thread);
    let mut waits = vec![];
    let mut texts = vec![];
    let stop = AtomicBool::new(false);
    loop {
        let input = match action {
            Action::Send => send(transport, resolved, thread, &mut texts),
            Action::Wait { secs } => {
                waits.push(secs);
                assert!(time.sleep_unless(secs, &stop));
                Input::Retry
            }
            Action::RunTools(calls) => Input::Results(
                calls
                    .iter()
                    .map(|c| ToolResult { id: c.id.clone(), content: tool(&c.name), is_error: false })
                    .collect(),
            ),
            Action::Done(outcome) => return (outcome, waits, texts),
        };
        action = r.next(thread, input);
    }
}

#[test]
fn anthropic_tool_round_with_an_overloaded_retry() {
    let replay = Replay::open(&cassette("anthropic_tool_round.jsonl"), TimeScale::INSTANT).unwrap();
    let mut account = Account::new("work", "Work", Provider::Anthropic);
    account.model = Some("claude-opus-5-5".into());
    let resolved = resolve(&account, None).unwrap();
    assert_eq!(resolved.protocol, Protocol::Anthropic);
    let mut thread = Thread::default();
    assert!(thread.push_user("Please tighten scene 3."));

    // The 529 asks for 4 s; scaled to nothing, the test doesn't wait.
    let t = Instant::now();
    let (outcome, waits, texts) = run(&replay, &resolved, &mut thread, TimeScale::INSTANT, |name| {
        assert_eq!(name, "read_scene");
        "INT. KITCHEN - NIGHT\nMARA stirs the pot.".into()
    });
    assert!(t.elapsed() < Duration::from_secs(2));
    assert_eq!(outcome, Outcome::Finished);
    assert_eq!(waits, [4]);
    assert_eq!(texts.concat(), "Scene 3 is tighter now.");
    assert_eq!(thread.messages.last().unwrap().text(), "Scene 3 is tighter now.");
    assert_eq!(replay.remaining(), 0);
}

#[test]
fn a_tool_result_the_cassette_doesnt_expect_is_a_loud_error() {
    let replay = Replay::open(&cassette("anthropic_tool_round.jsonl"), TimeScale::INSTANT).unwrap();
    let mut account = Account::new("work", "Work", Provider::Anthropic);
    account.model = Some("claude-opus-5-5".into());
    let resolved = resolve(&account, None).unwrap();
    let mut thread = Thread::default();
    assert!(thread.push_user("Please tighten scene 3."));
    let (outcome, _, _) = run(&replay, &resolved, &mut thread, TimeScale::INSTANT, |_| "EXT. BEACH".into());
    let Outcome::Error(e) = outcome else { panic!("{outcome:?}") };
    let m = e.to_string();
    assert!(m.contains("exchange 2 of 3") && m.contains("INT. KITCHEN") && m.contains("EXT. BEACH"), "{m}");
}

#[test]
fn openai_compatible_offline_retries_then_errors() {
    let replay = Replay::open(&cassette("openai_errors.jsonl"), TimeScale::INSTANT).unwrap();
    let mut account = Account::new(
        "local",
        "Local",
        Provider::Custom { protocol: Protocol::OpenaiChat, base_url: "http://localhost:11434/v1".into() },
    );
    account.model = Some("gpt-test".into());
    let resolved = resolve(&account, None).unwrap();
    let mut thread = Thread::default();
    assert!(thread.push_user("hello"));

    // Refused, then cut off mid-reply (what streamed is dropped), then the reply.
    let (outcome, waits, texts) = run(&replay, &resolved, &mut thread, TimeScale::INSTANT, |_| unreachable!());
    assert_eq!(outcome, Outcome::Finished);
    assert_eq!(waits.len(), 2);
    assert_eq!(texts.concat(), "HelHello there.");
    assert_eq!(thread.messages.last().unwrap().text(), "Hello there.");

    // A refused key: not retried.
    assert!(thread.push_user("again"));
    let (outcome, waits, _) = run(&replay, &resolved, &mut thread, TimeScale::INSTANT, |_| unreachable!());
    assert!(waits.is_empty());
    let Outcome::Error(e) = outcome else { panic!("{outcome:?}") };
    assert_eq!(e.kind(), Some(ErrorKind::Auth));
    assert!(e.to_string().contains("Incorrect API key"), "{e}");

    // A line the parser can't read.
    assert!(thread.push_user("once more"));
    let (outcome, _, _) = run(&replay, &resolved, &mut thread, TimeScale::INSTANT, |_| unreachable!());
    assert!(matches!(outcome, Outcome::Error(AgentError::Parse(_))), "{outcome:?}");
    assert_eq!(replay.remaining(), 0);
}
