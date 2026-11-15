//! The live transport: blocking HTTPS with `ureq`. Run it on a worker
//! thread; it calls back once per line of the streamed body.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use scriptorium_agent::{AgentError, Request};

use super::{Flow, Transport};
use crate::cassette::Exchange;
use crate::failure::Failure;

/// The longest single line a reply may stream (a guard against a broken or
/// hostile server; real lines are far shorter).
const MAX_LINE: u64 = 32 * 1024 * 1024;
/// The most of an error body that is read.
const MAX_ERROR_BODY: u64 = 1024 * 1024;

/// Streams requests over HTTP(S). Honours `HTTPS_PROXY`/`ALL_PROXY`/
/// `NO_PROXY`. Optionally records each exchange to a cassette
/// ([`Http::record_to`]).
#[derive(Debug)]
pub struct Http {
    agent: ureq::Agent,
    recorder: Option<Mutex<File>>,
}

impl Default for Http {
    fn default() -> Self {
        Http::new()
    }
}

impl Http {
    /// Timeouts: 30 s to connect, and 5 minutes for any one read (a stalled
    /// stream, or a local server still reading a long prompt, gives up
    /// then). No limit on a whole reply: long replies stream for minutes.
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_per_read(Some(Duration::from_secs(300)))
            .user_agent(concat!("scriptorium/", env!("CARGO_PKG_VERSION")))
            .build();
        Http { agent: ureq::Agent::new_with_config(config), recorder: None }
    }

    /// Also append every exchange to the cassette at `path` (created if
    /// missing): the request's path, model, the start of its last user text
    /// and new tool results, and the reply's status, lines or body. Never
    /// headers, so never a key. For `scriptorium chat --record`.
    pub fn record_to(mut self, path: &Path) -> Result<Http, AgentError> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| Failure::File(format!("can't record to {}: {e}", path.display())).to_error())?;
        self.recorder = Some(Mutex::new(file));
        Ok(self)
    }

    fn exchange(&self, req: &Request, line: &mut dyn FnMut(&str) -> Flow, rec: &mut Exchange) -> Result<(), Failure> {
        let host = host(&req.url);
        let offline = |e: ureq::Error| Failure::Offline(format!("couldn't reach {host}: {e}"));
        let response = if req.body.is_null() {
            let mut r = self.agent.get(&req.url);
            for (k, v) in &req.headers {
                r = r.header(k.as_str(), v.as_str());
            }
            r.call().map_err(offline)?
        } else {
            let mut r = self.agent.post(&req.url);
            for (k, v) in &req.headers {
                r = r.header(k.as_str(), v.as_str());
            }
            r.send(req.body.to_string().as_bytes()).map_err(offline)?
        };
        let status = response.status().as_u16();
        rec.status = Some(status);
        if !(200..300).contains(&status) {
            let retry_after = response.headers().get("retry-after").and_then(|v| v.to_str().ok()).map(String::from);
            let mut bytes = vec![];
            // Whatever arrived of the error body is enough to describe it.
            let _ = response.into_body().into_reader().take(MAX_ERROR_BODY).read_to_end(&mut bytes);
            let body = String::from_utf8_lossy(&bytes).into_owned();
            rec.retry_after.clone_from(&retry_after);
            rec.body = Some(body.clone());
            return Err(Failure::Status { status, retry_after, body });
        }
        let mut reader = BufReader::new(response.into_body().into_reader());
        let mut buf = vec![];
        loop {
            buf.clear();
            let n = (&mut reader)
                .take(MAX_LINE)
                .read_until(b'\n', &mut buf)
                .map_err(|e| Failure::CutOff(format!("{host}: {e}")))?;
            if n == 0 {
                return Ok(());
            }
            if buf.last() != Some(&b'\n') && n as u64 == MAX_LINE {
                return Err(Failure::BadLine(format!("a line of the reply is longer than {MAX_LINE} bytes")));
            }
            let text = std::str::from_utf8(&buf)
                .map_err(|e| Failure::BadLine(format!("a line of the reply isn't UTF-8 text ({e})")))?;
            let text = text.strip_suffix('\n').unwrap_or(text);
            let text = text.strip_suffix('\r').unwrap_or(text);
            rec.lines.push(text.to_string());
            if line(text) == Flow::Stop {
                // Dropping the reader closes the connection.
                return Ok(());
            }
        }
    }
}

impl Transport for Http {
    fn send(&self, req: &Request, line: &mut dyn FnMut(&str) -> Flow) -> Result<(), AgentError> {
        let mut rec = Exchange::recording(req);
        let result = self.exchange(req, line, &mut rec);
        if let Some(recorder) = &self.recorder {
            if let Err(Failure::Offline(m) | Failure::CutOff(m) | Failure::BadLine(m)) = &result {
                rec.offline = Some(m.clone());
                rec.status = rec.status.filter(|_| !rec.lines.is_empty());
            }
            if rec.status == Some(200) {
                rec.status = None;
            }
            let mut file = recorder.lock().unwrap_or_else(|e| e.into_inner());
            let written = writeln!(file, "{}", rec.to_line()).and_then(|()| file.flush());
            // A failed recording is reported unless the request itself failed.
            if let (Err(e), Ok(())) = (written, &result) {
                return Err(Failure::File(format!("can't write the recording: {e}")).to_error());
            }
        }
        result.map_err(|f| f.to_error())
    }
}

/// The host of a URL, for messages.
fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::{Replay, TimeScale};
    use scriptorium_agent::ErrorKind;
    use serde_json::json;
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    /// A one-shot HTTP server on loopback that answers `response` (raw
    /// bytes) and returns the request it got.
    fn serve(response: Vec<u8>) -> (String, JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut head = String::new();
            let mut len = 0;
            loop {
                let mut l = String::new();
                reader.read_line(&mut l).unwrap();
                if let Some(v) = l.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                head.push_str(&l);
                if l == "\r\n" || l.is_empty() {
                    break;
                }
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            head.push_str(&String::from_utf8_lossy(&body));
            let _ = s.write_all(&response);
            let _ = s.flush();
            head
        });
        (url, handle)
    }

    fn ok(body: &[u8]) -> Vec<u8> {
        let mut r =
            format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n", body.len())
                .into_bytes();
        r.extend_from_slice(body);
        r
    }

    fn post(url: &str) -> Request {
        Request {
            url: format!("{url}/v1/messages"),
            headers: vec![("x-api-key".into(), "sk-secret".into()), ("content-type".into(), "application/json".into())],
            body: json!({"model": "claude-opus-5-5", "messages": [{"role": "user", "content": "tighten it"}]}),
        }
    }

    fn lines(http: &Http, req: &Request) -> (Vec<String>, Result<(), AgentError>) {
        let mut got = vec![];
        let r = http.send(req, &mut |l| {
            got.push(l.to_string());
            Flow::Continue
        });
        (got, r)
    }

    #[test]
    fn streams_lines() {
        let (url, server) = serve(ok(b"event: ping\r\ndata: {\"a\":\"\xc3\xa9\"}\n\ndata: [DONE]"));
        let (got, r) = lines(&Http::new(), &post(&url));
        r.unwrap();
        assert_eq!(got, ["event: ping", "data: {\"a\":\"é\"}", "", "data: [DONE]"]);
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /v1/messages "), "{request}");
        assert!(request.contains("x-api-key: sk-secret"));
        assert!(request.ends_with(r#""model":"claude-opus-5-5"}"#) || request.contains("tighten it"));
    }

    #[test]
    fn a_null_body_is_a_get() {
        let (url, server) = serve(ok(b"{\"data\":[]}"));
        let req = Request { url: format!("{url}/v1/models"), headers: vec![], body: serde_json::Value::Null };
        let (got, r) = lines(&Http::new(), &req);
        r.unwrap();
        assert_eq!(got, ["{\"data\":[]}"]);
        assert!(server.join().unwrap().starts_with("GET /v1/models "));
    }

    #[test]
    fn error_statuses_go_through_the_shared_mapping() {
        let body = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        let (url, server) = serve(
            format!("HTTP/1.1 529 Overloaded\r\nretry-after: 3\r\ncontent-length: {}\r\n\r\n{body}", body.len())
                .into_bytes(),
        );
        let (got, r) = lines(&Http::new(), &post(&url));
        server.join().unwrap();
        assert!(got.is_empty());
        let expected = Failure::Status { status: 529, retry_after: Some("3".into()), body: body.into() }.to_error();
        assert_eq!(r.unwrap_err(), expected);
    }

    #[test]
    fn refused_connections_are_offline() {
        let url = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", l.local_addr().unwrap())
        };
        let e = lines(&Http::new(), &post(&url)).1.unwrap_err();
        assert_eq!(e.kind(), Some(ErrorKind::Offline));
        assert!(e.to_string().contains("127.0.0.1"), "{e}");
    }

    #[test]
    fn a_reply_cut_short_is_offline_after_its_lines() {
        let (url, server) = serve(b"HTTP/1.1 200 OK\r\ncontent-length: 1000\r\n\r\ndata: 1\n".to_vec());
        let (got, r) = lines(&Http::new(), &post(&url));
        server.join().unwrap();
        assert_eq!(got, ["data: 1"]);
        let e = r.unwrap_err();
        assert_eq!(e.kind(), Some(ErrorKind::Offline));
        assert!(e.to_string().contains("cut off"), "{e}");
    }

    #[test]
    fn a_line_that_isnt_text_is_a_parse_error() {
        let (url, server) = serve(ok(b"data: \xff\xfe\n"));
        let r = lines(&Http::new(), &post(&url)).1;
        server.join().unwrap();
        assert!(matches!(r, Err(AgentError::Parse(_))), "{r:?}");
    }

    #[test]
    fn stop_stops_reading() {
        let (url, server) = serve(ok(b"a\nb\nc\n"));
        let mut got = vec![];
        Http::new()
            .send(&post(&url), &mut |l| {
                got.push(l.to_string());
                Flow::Stop
            })
            .unwrap();
        server.join().unwrap();
        assert_eq!(got, ["a"]);
    }

    #[test]
    fn recordings_replay_the_same_and_hold_no_key() {
        let dir = std::env::temp_dir().join(format!("scriptorium-providers-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cassette = dir.join("rec.jsonl");
        let _ = std::fs::remove_file(&cassette);
        let http = Http::new().record_to(&cassette).unwrap();

        let (url, server) = serve(ok(b"event: ping\ndata: {}\n\n"));
        let req1 = post(&url);
        let live1 = lines(&http, &req1);
        server.join().unwrap();
        let body = r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#;
        let (url, server) = serve(
            format!("HTTP/1.1 429 Too Many\r\nretry-after: 9\r\ncontent-length: {}\r\n\r\n{body}", body.len())
                .into_bytes(),
        );
        let req2 = post(&url);
        let live2 = lines(&http, &req2);
        server.join().unwrap();
        let (url, server) = serve(b"HTTP/1.1 200 OK\r\ncontent-length: 99\r\n\r\ndata: 1\n".to_vec());
        let req3 = post(&url);
        let live3 = lines(&http, &req3);
        server.join().unwrap();

        let text = std::fs::read_to_string(&cassette).unwrap();
        assert!(!text.contains("sk-secret"), "{text}");
        assert_eq!(text.lines().count(), 3, "{text}");
        let replay = Replay::open(&cassette, TimeScale::INSTANT).unwrap();
        assert_eq!(lines_replay(&replay, &req1), live1);
        assert_eq!(lines_replay(&replay, &req2), live2);
        let (got3, r3) = lines_replay(&replay, &req3);
        assert_eq!(got3, live3.0);
        assert_eq!(r3.unwrap_err().kind(), Some(ErrorKind::Offline));
        assert_eq!(replay.remaining(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn lines_replay(r: &Replay, req: &Request) -> (Vec<String>, Result<(), AgentError>) {
        let mut got = vec![];
        let res = r.send(req, &mut |l| {
            got.push(l.to_string());
            Flow::Continue
        });
        (got, res)
    }

    #[test]
    fn hosts() {
        assert_eq!(host("https://api.anthropic.com/v1/messages"), "api.anthropic.com");
        assert_eq!(host("http://localhost:11434"), "localhost:11434");
    }
}
