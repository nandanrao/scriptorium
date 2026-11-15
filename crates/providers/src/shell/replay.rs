//! A transport that answers from a cassette ([`crate::cassette`]).

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use scriptorium_agent::{AgentError, Request};

use super::{CancelToken, Ended, Flow, TimeScale, Transport};
use crate::cassette::{self, Exchange, Reply};
use crate::failure::Failure;

/// Answers each request with the cassette's next exchange: one in-order
/// queue for the whole process (the CLI and the app make one per run), so
/// requests sent from several threads at once take exchanges in whatever
/// order they arrive.
///
/// A request that doesn't fit the exchange's `expect`, or one past the end,
/// is an error naming the exchange (and the exchange is used up either way,
/// so later requests don't silently shift). [`Transport::finish`] reports
/// exchanges left unused; a `Replay` dropped without it says so on stderr.
#[derive(Debug)]
pub struct Replay {
    name: String,
    exchanges: Vec<Exchange>,
    next: Mutex<usize>,
    time: TimeScale,
    finished: AtomicBool,
}

impl Replay {
    /// Reads a cassette file. An unreadable or malformed one is
    /// [`AgentError::File`].
    pub fn open(path: &Path, time: TimeScale) -> Result<Replay, AgentError> {
        let name = path.display().to_string();
        let text =
            std::fs::read_to_string(path).map_err(|e| Failure::File(format!("cassette {name}: {e}")).to_error())?;
        Replay::from_text(&name, &text, time)
    }

    /// A cassette from its text; `name` is for errors.
    pub fn from_text(name: &str, text: &str, time: TimeScale) -> Result<Replay, AgentError> {
        let exchanges = cassette::parse(text).map_err(|e| Failure::File(format!("cassette {name}: {e}")).to_error())?;
        Ok(Replay { name: name.to_string(), exchanges, next: Mutex::new(0), time, finished: AtomicBool::new(false) })
    }

    /// Exchanges not yet used.
    pub fn remaining(&self) -> usize {
        self.exchanges.len().saturating_sub(*self.next.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl Transport for Replay {
    fn send(
        &self,
        req: &Request,
        cancel: &CancelToken,
        line: &mut dyn FnMut(&str) -> Flow,
    ) -> Result<Ended, AgentError> {
        if cancel.is_cancelled() {
            return Ok(Ended::Cancelled);
        }
        let n = {
            let mut next = self.next.lock().unwrap_or_else(|e| e.into_inner());
            let n = *next;
            *next += 1;
            n
        };
        let total = self.exchanges.len();
        let Some(x) = self.exchanges.get(n) else {
            return Err(Failure::Replay(format!(
                "{}: request {} but the cassette has only {total} exchange(s)",
                self.name,
                n + 1
            ))
            .to_error());
        };
        x.expect
            .check(req)
            .map_err(|m| Failure::Replay(format!("{}: exchange {} of {total}: {m}", self.name, n + 1)).to_error())?;
        let delay = Duration::from_millis(x.delay_ms);
        match x.reply() {
            Reply::Fail(f) => {
                if !self.time.sleep(delay, cancel) {
                    return Ok(Ended::Cancelled);
                }
                Err(f.to_error())
            }
            Reply::Lines { lines, then } => {
                let mut stopped = false;
                for l in lines {
                    if !self.time.sleep(delay, cancel) {
                        return Ok(Ended::Cancelled);
                    }
                    if line(l) == Flow::Stop {
                        stopped = true;
                        break;
                    }
                    if cancel.is_cancelled() {
                        return Ok(Ended::Cancelled);
                    }
                }
                if cancel.is_cancelled() {
                    return Ok(Ended::Cancelled);
                }
                match then {
                    Some(f) if !stopped => Err(f.to_error()),
                    _ => Ok(Ended::Finished),
                }
            }
        }
    }

    fn finish(&self) -> Result<(), AgentError> {
        self.finished.store(true, Ordering::SeqCst);
        match self.remaining() {
            0 => Ok(()),
            left => Err(Failure::Replay(format!(
                "{}: {left} of {} exchange(s) never requested (the first is exchange {})",
                self.name,
                self.exchanges.len(),
                self.exchanges.len() - left + 1
            ))
            .to_error()),
        }
    }
}

impl Drop for Replay {
    fn drop(&mut self) {
        let left = self.remaining();
        if left > 0 && !self.finished.load(Ordering::SeqCst) && !std::thread::panicking() {
            eprintln!("replay: {}: {left} exchange(s) never requested", self.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scriptorium_agent::ErrorKind;
    use serde_json::json;

    fn req(model: &str) -> Request {
        Request {
            url: "https://api.anthropic.com/v1/messages".into(),
            headers: vec![],
            body: json!({"model": model, "messages": [{"role": "user", "content": "hi"}]}),
        }
    }

    fn collect(r: &Replay, model: &str) -> (Vec<String>, Result<Ended, AgentError>) {
        let mut got = vec![];
        let res = r.send(&req(model), &CancelToken::new(), &mut |l| {
            got.push(l.to_string());
            Flow::Continue
        });
        (got, res)
    }

    #[test]
    fn exchanges_in_order() {
        let text = r#"
{"expect": {"model": "m"}, "lines": ["data: 1", "", "data: 2", ""]}
{"status": 529, "retry_after": "2", "body": "{\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}"}
{"offline": "connection reset"}
{"lines": ["data: 1"], "offline": "reset"}
{"lines": ["data: 1"], "bad_line": "not text"}
"#;
        let r = Replay::from_text("t.jsonl", text, TimeScale::INSTANT).unwrap();
        assert_eq!(r.remaining(), 5);
        let (lines, res) = collect(&r, "m");
        assert_eq!(lines, ["data: 1", "", "data: 2", ""]);
        assert_eq!(res.unwrap(), Ended::Finished);
        let (_, res) = collect(&r, "m");
        assert!(matches!(res, Err(AgentError::Api { kind: ErrorKind::Overloaded, retry_after: Some(2), .. })));
        let (_, res) = collect(&r, "m");
        assert_eq!(res.unwrap_err().kind(), Some(ErrorKind::Offline));
        let (lines, res) = collect(&r, "m");
        assert_eq!(lines, ["data: 1"]);
        assert_eq!(res.unwrap_err().kind(), Some(ErrorKind::Offline));
        let (_, res) = collect(&r, "m");
        assert!(matches!(res, Err(AgentError::Parse(_))));
        assert_eq!(r.remaining(), 0);
        let (_, res) = collect(&r, "m");
        let e = res.unwrap_err().to_string();
        assert!(e.contains("t.jsonl") && e.contains("request 6") && e.contains("only 5"), "{e}");
        r.finish().unwrap();
    }

    #[test]
    fn a_mismatch_names_the_exchange_and_uses_it_up() {
        let text = "{\"expect\": {\"model\": \"a\"}}\n{\"expect\": {\"model\": \"b\"}}\n";
        let r = Replay::from_text("c", text, TimeScale::INSTANT).unwrap();
        let e = collect(&r, "zzz").1.unwrap_err();
        assert!(matches!(e, AgentError::Api { retryable: false, .. }));
        assert!(e.to_string().contains("exchange 1 of 2"), "{e}");
        collect(&r, "b").1.unwrap();
    }

    #[test]
    fn unused_exchanges_fail_finish() {
        let r = Replay::from_text("c.jsonl", "{}\n{}\n{}\n", TimeScale::INSTANT).unwrap();
        collect(&r, "m").1.unwrap();
        let e = r.finish().unwrap_err().to_string();
        assert!(e.contains("c.jsonl") && e.contains("2 of 3") && e.contains("exchange 2"), "{e}");
    }

    #[test]
    fn stop_ends_the_stream_before_a_trailing_failure() {
        let r = Replay::from_text("c", r#"{"lines": ["a", "b", "c"], "offline": "x"}"#, TimeScale::INSTANT).unwrap();
        let mut got = vec![];
        let ended = r
            .send(&req("m"), &CancelToken::new(), &mut |l| {
                got.push(l.to_string());
                if l == "b" { Flow::Stop } else { Flow::Continue }
            })
            .unwrap();
        assert_eq!(got, ["a", "b"]);
        assert_eq!(ended, Ended::Finished);
    }

    #[test]
    fn cancel_interrupts_a_delay() {
        let r = Replay::from_text("c", r#"{"delay_ms": 600000, "lines": ["a"]}"#, TimeScale::REAL).unwrap();
        let cancel = CancelToken::new();
        let c = cancel.clone();
        let t = std::time::Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            c.cancel();
        });
        let ended = r.send(&req("m"), &cancel, &mut |_| panic!("no line")).unwrap();
        assert_eq!(ended, Ended::Cancelled);
        assert!(t.elapsed() < Duration::from_secs(2));
        // Already cancelled: no exchange is used.
        let r = Replay::from_text("c", "{}", TimeScale::INSTANT).unwrap();
        assert_eq!(r.send(&req("m"), &cancel, &mut |_| Flow::Continue).unwrap(), Ended::Cancelled);
        assert_eq!(r.remaining(), 1);
        r.finished.store(true, Ordering::SeqCst);
    }

    #[test]
    fn delays_are_scaled() {
        let r = Replay::from_text("c", r#"{"delay_ms": 100000, "lines": ["a", "b"]}"#, TimeScale::INSTANT).unwrap();
        let t = std::time::Instant::now();
        collect(&r, "m").1.unwrap();
        assert!(t.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn bad_cassettes_are_file_errors() {
        let e = Replay::from_text("bad.jsonl", "{not json", TimeScale::INSTANT).unwrap_err();
        assert!(matches!(&e, AgentError::File(m) if m.contains("bad.jsonl") && m.contains("line 1")), "{e:?}");
        let e = Replay::open(Path::new("/nonexistent/cassette.jsonl"), TimeScale::INSTANT).unwrap_err();
        assert!(matches!(e, AgentError::File(_)));
    }
}
