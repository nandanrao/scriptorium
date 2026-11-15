//! One streamed reply, from request to [`Turn`]: what both the CLI's loop
//! and the panel's driver do with [`Action::Send`](scriptorium_agent::turn::Action::Send).

use scriptorium_agent::turn::Input;
use scriptorium_agent::{AgentError, Event, Protocol, ReplyParser, Request, Turn};
use serde_json::Value;

use super::{CancelToken, Ended, Flow, Transport};

/// A reply that didn't fail.
#[derive(Debug, Clone, PartialEq)]
pub enum Received {
    /// The whole reply.
    Reply(Turn),
    /// Cancelled. With what streamed before, if anything did
    /// ([`ReplyParser::abort`]: text only, marked stopped).
    Stopped(Option<Turn>),
}

impl Received {
    /// What to tell [`Run::next`](scriptorium_agent::turn::Run::next).
    pub fn into_input(self) -> Input {
        match self {
            Received::Reply(turn) | Received::Stopped(Some(turn)) => Input::Reply(turn),
            Received::Stopped(None) => Input::Stop(vec![]),
        }
    }
}

/// Sends `req` and parses the reply for `protocol`, calling `on_event` for
/// each event as it streams (text, thinking, tool calls, usage). Stops
/// reading as soon as the reply's final event arrives (`message_stop`,
/// `[DONE]`), so a proxy that holds the body open can't stall the turn.
///
/// An error (HTTP, connection, unreadable reply) is for
/// [`Input::Failed`]; whatever streamed before it is not part of the thread,
/// so clear what was shown of it.
pub fn receive(
    transport: &dyn Transport,
    req: &Request,
    protocol: Protocol,
    cancel: &CancelToken,
    on_event: &mut dyn FnMut(&Event),
) -> Result<Received, AgentError> {
    let mut parser = ReplyParser::new(protocol);
    let mut any = false;
    let mut failed = None;
    let ended = transport.send(req, cancel, &mut |line| {
        any = true;
        match parser.line(line) {
            Ok(events) => {
                events.iter().for_each(&mut *on_event);
                if ends_reply(protocol, line) { Flow::Stop } else { Flow::Continue }
            }
            Err(e) => {
                failed = Some(e);
                Flow::Stop
            }
        }
    })?;
    if ended == Ended::Cancelled {
        return Ok(Received::Stopped(any.then(|| parser.abort())));
    }
    if let Some(e) = failed {
        return Err(e);
    }
    parser.finish().map(Received::Reply)
}

/// Whether `line` carries the reply's final event. (The parser takes the
/// frame it starts when it finishes, so reading can stop at this line.)
fn ends_reply(protocol: Protocol, line: &str) -> bool {
    let Some(data) = line.strip_prefix("data:").map(str::trim) else { return false };
    match protocol {
        Protocol::OpenaiChat => data == "[DONE]",
        Protocol::Anthropic => {
            data.contains("message_stop")
                && serde_json::from_str::<Value>(data)
                    .is_ok_and(|v| v.get("type").and_then(Value::as_str) == Some("message_stop"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::{Replay, TimeScale};
    use serde_json::json;

    fn req() -> Request {
        Request { url: "http://h/v1/chat/completions".into(), headers: vec![], body: json!({"model": "m"}) }
    }

    #[test]
    fn final_events() {
        assert!(ends_reply(Protocol::OpenaiChat, "data: [DONE]"));
        assert!(ends_reply(Protocol::OpenaiChat, "data:[DONE]"));
        assert!(!ends_reply(Protocol::OpenaiChat, "data: {\"x\":\"[DONE]\"}"));
        assert!(ends_reply(Protocol::Anthropic, r#"data: {"type":"message_stop"}"#));
        assert!(!ends_reply(Protocol::Anthropic, "event: message_stop"));
        assert!(!ends_reply(
            Protocol::Anthropic,
            r#"data: {"type":"content_block_delta","delta":{"text":"message_stop"}}"#
        ));
    }

    #[test]
    fn reading_stops_at_the_final_event() {
        // Lines after [DONE] would fail the parse if they were read.
        let text = r#"{"lines": [
            {"choices": [{"delta": {"content": "Hi"}, "finish_reason": "stop"}]}, "",
            "data: [DONE]", "",
            "data: {broken"
        ]}"#;
        let replay = Replay::from_text("c", text, TimeScale::INSTANT).unwrap();
        let mut texts = vec![];
        let got = receive(&replay, &req(), Protocol::OpenaiChat, &CancelToken::new(), &mut |e| {
            if let Event::Text { text, .. } = e {
                texts.push(text.clone());
            }
        })
        .unwrap();
        let Received::Reply(turn) = got else { panic!("{got:?}") };
        assert_eq!(texts, ["Hi"]);
        assert_eq!(turn.message.text(), "Hi");
    }

    #[test]
    fn cancelled_before_or_during() {
        let text = r#"{"lines": [{"choices": [{"delta": {"content": "Hi"}}]}, "", "data: x"]}"#;
        let replay = Replay::from_text("c", &format!("{text}\n{text}"), TimeScale::INSTANT).unwrap();
        let cancel = CancelToken::new();
        let c = cancel.clone();
        let got = receive(&replay, &req(), Protocol::OpenaiChat, &cancel, &mut |_| c.cancel()).unwrap();
        let Received::Stopped(Some(turn)) = got.clone() else { panic!("{got:?}") };
        assert_eq!(turn.stop, scriptorium_agent::StopReason::Cancelled);
        assert_eq!(got.into_input(), Input::Reply(turn));
        let got = receive(&replay, &req(), Protocol::OpenaiChat, &cancel, &mut |_| {}).unwrap();
        assert_eq!(got, Received::Stopped(None));
        assert_eq!(got.into_input(), Input::Stop(vec![]));
    }
}
