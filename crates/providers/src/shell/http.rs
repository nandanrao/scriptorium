//! The live transport: blocking HTTPS with `ureq`. Run it on a worker
//! thread; it calls back once per line of the streamed body.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use scriptorium_agent::{AgentError, Request};

use super::{CancelToken, Ended, Flow, Transport};
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
///
/// Each request gets its own connection, so cancelling one shuts down only
/// that one: a TLS handshake, a wait for headers or a stalled read return
/// at once. (A TCP connect in progress is not interrupted; it gives up after
/// its 30 s timeout, and the result is ignored.)
#[derive(Debug)]
pub struct Http {
    config: ureq::config::Config,
    /// The recording; `None` inside once [`Transport::finish`] closed it.
    recorder: Option<Mutex<Option<File>>>,
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
    ///
    /// Redirects are never followed: a request carries the account's key
    /// and the script's text, and both go only to the address the account
    /// names. A redirect is an error naming where it pointed.
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_per_read(Some(Duration::from_secs(300)))
            .user_agent(concat!("scriptorium/", env!("CARGO_PKG_VERSION")))
            .build();
        Http { config, recorder: None }
    }

    /// Also append every exchange to the cassette at `path` (created if
    /// missing), as readable [`cassette`](crate::cassette) text: the
    /// request's path and model, the start of its last user message and of
    /// each new tool result, and the reply's status and lines or error body.
    /// Never headers, so never a key; but it does hold the writer's words,
    /// script text from tool results, and replies, so treat a recording like
    /// the script. A request cancelled mid-reply is recorded as cut off.
    ///
    /// Record one run at a time: a [`Replay`](super::Replay) answers in
    /// order, so requests sent concurrently would record in an order a
    /// replay can't reproduce.
    pub fn record_to(mut self, path: &Path) -> Result<Http, AgentError> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| Failure::File(format!("can't record to {}: {e}", path.display())).to_error())?;
        self.recorder = Some(Mutex::new(Some(file)));
        Ok(self)
    }

    fn exchange(
        &self,
        req: &Request,
        cancel: &CancelToken,
        line: &mut dyn FnMut(&str) -> Flow,
        rec: &mut Exchange,
    ) -> Result<Ended, Failure> {
        let agent = ureq::Agent::new_with_config(self.config.clone());
        let to_shut = agent.clone();
        let _guard = cancel.on_cancel(move || to_shut.clone().shutdown());
        let host = host(&req.url);
        let offline = |e: ureq::Error| Failure::Offline(format!("couldn't reach {host}: {e}"));
        let response = if req.body.is_null() {
            let mut r = agent.get(&req.url);
            for (k, v) in &req.headers {
                r = r.header(k.as_str(), v.as_str());
            }
            r.call().map_err(offline)?
        } else {
            let mut r = agent.post(&req.url);
            for (k, v) in &req.headers {
                r = r.header(k.as_str(), v.as_str());
            }
            r.send(req.body.to_string().as_bytes()).map_err(offline)?
        };
        let status = response.status().as_u16();
        rec.status = Some(status);
        if (300..400).contains(&status) {
            let to = response.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("another address");
            let message = format!(
                "{host} redirected the request to {to}. Scriptorium doesn't follow redirects, so your key and script \
                 go only where the account points: if the service moved, change the account's address."
            );
            // In the shape error bodies have, so the shared mapping quotes it.
            let body = serde_json::json!({ "message": message }).to_string();
            rec.body = Some(body.clone());
            return Err(Failure::Status { status, retry_after: None, body });
        }
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
            if cancel.is_cancelled() {
                return Ok(Ended::Cancelled);
            }
            buf.clear();
            let n = (&mut reader)
                .take(MAX_LINE)
                .read_until(b'\n', &mut buf)
                .map_err(|e| Failure::CutOff(format!("{host}: {e}")))?;
            if n == 0 {
                return Ok(Ended::Finished);
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
                return Ok(Ended::Finished);
            }
        }
    }

    fn record(&self, rec: &mut Exchange, result: &Result<Ended, Failure>) -> std::io::Result<()> {
        let Some(recorder) = &self.recorder else { return Ok(()) };
        match result {
            Err(Failure::Offline(m) | Failure::CutOff(m)) => rec.offline = Some(m.clone()),
            Err(Failure::BadLine(m)) => rec.bad_line = Some(m.clone()),
            Ok(Ended::Cancelled) => rec.offline = Some("stopped while recording".into()),
            _ => {}
        }
        if rec.status.is_some_and(|s| (200..300).contains(&s)) {
            rec.status = None;
        }
        let mut file = recorder.lock().unwrap_or_else(|e| e.into_inner());
        let Some(file) = file.as_mut() else { return Ok(()) };
        file.write_all(rec.to_text().as_bytes())?;
        file.flush()
    }
}

impl Transport for Http {
    fn send(
        &self,
        req: &Request,
        cancel: &CancelToken,
        line: &mut dyn FnMut(&str) -> Flow,
    ) -> Result<Ended, AgentError> {
        if cancel.is_cancelled() {
            return Ok(Ended::Cancelled);
        }
        let mut rec = Exchange::recording(req);
        let mut result = self.exchange(req, cancel, line, &mut rec);
        // Whatever the shut-down connection reported, it was a cancel.
        if result.is_err() && cancel.is_cancelled() {
            result = Ok(Ended::Cancelled);
        }
        let written = self.record(&mut rec, &result);
        // A failed recording is reported unless the request itself failed.
        if let (Err(e), Ok(_)) = (written, &result) {
            return Err(Failure::File(format!("can't write the recording: {e}")).to_error());
        }
        result.map_err(|f| f.to_error())
    }

    /// Closes the recording, if any: waits for an exchange being written
    /// to be written whole, syncs the file, and records nothing after. A
    /// program quitting at once (a second Ctrl+C) calls this first, so the
    /// cassette never ends in half an exchange.
    fn finish(&self) -> Result<(), AgentError> {
        let Some(recorder) = &self.recorder else { return Ok(()) };
        let file = recorder.lock().unwrap_or_else(|e| e.into_inner()).take();
        match file.map(|f| f.sync_all()) {
            Some(Err(e)) => Err(Failure::File(format!("can't write the recording: {e}")).to_error()),
            _ => Ok(()),
        }
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
    use std::net::{TcpListener, TcpStream};
    use std::thread::JoinHandle;
    use std::time::Instant;

    /// Reads one HTTP request from `s`: its head and body.
    fn read_request(s: &TcpStream) -> String {
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
        head
    }

    /// A one-shot HTTP server on loopback that answers `response` (raw
    /// bytes) and returns the request it got.
    fn serve(response: Vec<u8>) -> (String, JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let head = read_request(&s);
            let _ = s.write_all(&response);
            let _ = s.flush();
            head
        });
        (url, handle)
    }

    /// A server that answers `response` and then holds the connection open
    /// without a word until the client goes away.
    fn serve_and_stall(response: Vec<u8>) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            read_request(&s);
            let _ = s.write_all(&response);
            let _ = s.flush();
            let mut sink = [0; 64];
            // Returns when the client closes its end.
            while matches!(s.read(&mut sink), Ok(n) if n > 0) {}
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

    fn lines(t: &dyn Transport, req: &Request) -> (Vec<String>, Result<Ended, AgentError>) {
        let mut got = vec![];
        let r = t.send(req, &CancelToken::new(), &mut |l| {
            got.push(l.to_string());
            Flow::Continue
        });
        (got, r)
    }

    #[test]
    fn streams_lines() {
        let (url, server) = serve(ok(b"event: ping\r\ndata: {\"a\":\"\xc3\xa9\"}\n\ndata: [DONE]"));
        let (got, r) = lines(&Http::new(), &post(&url));
        assert_eq!(r.unwrap(), Ended::Finished);
        assert_eq!(got, ["event: ping", "data: {\"a\":\"é\"}", "", "data: [DONE]"]);
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /v1/messages "), "{request}");
        assert!(request.contains("x-api-key: sk-secret"));
        assert!(request.contains("tighten it"));
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
    fn redirects_are_not_followed() {
        // Were it followed, the key and body would go to the second server.
        let (to, target) = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", l.local_addr().unwrap());
            l.set_nonblocking(true).unwrap();
            (url, l)
        };
        let (url, server) = serve(
            format!("HTTP/1.1 307 Temporary Redirect\r\nlocation: {to}/steal\r\ncontent-length: 0\r\n\r\n")
                .into_bytes(),
        );
        let (got, r) = lines(&Http::new(), &post(&url));
        server.join().unwrap();
        assert!(got.is_empty());
        let e = r.unwrap_err().to_string();
        assert!(e.contains("redirect") && e.contains(&format!("{to}/steal")), "{e}");
        assert!(target.accept().is_err(), "the redirect was followed");
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
    fn flow_stop_stops_reading_a_body_held_open() {
        // The server never ends the body; Stop after the first line returns.
        let (url, server) =
            serve_and_stall(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n6\r\na\nb\nc\n\r\n".to_vec());
        let mut got = vec![];
        let ended = Http::new()
            .send(&post(&url), &CancelToken::new(), &mut |l| {
                got.push(l.to_string());
                Flow::Stop
            })
            .unwrap();
        assert_eq!(ended, Ended::Finished);
        assert_eq!(got, ["a"]);
        server.join().unwrap();
    }

    /// Cancels `cancel` after 100 ms, on another thread.
    fn cancel_soon(cancel: &CancelToken) {
        let c = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            c.cancel();
        });
    }

    #[test]
    fn cancel_interrupts_a_wait_for_headers() {
        let (url, server) = serve_and_stall(vec![]);
        let cancel = CancelToken::new();
        cancel_soon(&cancel);
        let t = Instant::now();
        let ended = Http::new().send(&post(&url), &cancel, &mut |_| panic!("no line")).unwrap();
        assert_eq!(ended, Ended::Cancelled);
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        server.join().unwrap();
    }

    #[test]
    fn cancel_interrupts_a_stalled_stream() {
        let (url, server) =
            serve_and_stall(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n8\r\ndata: 1\n\r\n".to_vec());
        let cancel = CancelToken::new();
        let mut got = vec![];
        let t = Instant::now();
        cancel_soon(&cancel);
        let ended = Http::new()
            .send(&post(&url), &cancel, &mut |l| {
                got.push(l.to_string());
                Flow::Continue
            })
            .unwrap();
        assert_eq!(ended, Ended::Cancelled);
        assert_eq!(got, ["data: 1"]);
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        server.join().unwrap();
        // Already cancelled: nothing is sent.
        assert_eq!(Http::new().send(&post(&url), &cancel, &mut |_| Flow::Continue).unwrap(), Ended::Cancelled);
    }

    #[test]
    fn recordings_replay_the_same_and_hold_no_key() {
        let dir = std::env::temp_dir().join(format!("scriptorium-providers-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cassette = dir.join("rec.jsonl");
        let _ = std::fs::remove_file(&cassette);
        let http = Http::new().record_to(&cassette).unwrap();

        let mut live = vec![];
        let mut reqs = vec![];
        let body = r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#;
        for response in [
            ok(b"event: ping\ndata: {}\n\n"),
            format!("HTTP/1.1 429 Too Many\r\nretry-after: 9\r\ncontent-length: {}\r\n\r\n{body}", body.len())
                .into_bytes(),
            b"HTTP/1.1 200 OK\r\ncontent-length: 99\r\n\r\ndata: 1\n".to_vec(),
            ok(b"data: 1\ndata: \xff\n"),
        ] {
            let (url, server) = serve(response);
            let req = post(&url);
            live.push(lines(&http, &req));
            server.join().unwrap();
            reqs.push(req);
        }

        let text = std::fs::read_to_string(&cassette).unwrap();
        assert!(!text.contains("sk-secret"), "{text}");
        assert!(text.contains("\"lines\": [\n  \"event: ping\",\n  {},"), "{text}");
        let replay = Replay::open(&cassette, TimeScale::INSTANT).unwrap();
        assert_eq!(replay.remaining(), 4);
        let replayed: Vec<_> = reqs.iter().map(|r| lines(&replay, r)).collect();
        assert_eq!(replayed[0], live[0]);
        assert_eq!(replayed[1], live[1]);
        for (r, l) in replayed.iter().zip(&live).skip(2) {
            assert_eq!(r.0, l.0);
            assert_eq!(r.1.as_ref().unwrap_err().kind(), l.1.as_ref().unwrap_err().kind());
        }
        assert!(matches!(replayed[3].1, Err(AgentError::Parse(_))), "a bad line replays as Parse");
        replay.finish().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn hosts() {
        assert_eq!(host("https://api.anthropic.com/v1/messages"), "api.anthropic.com");
        assert_eq!(host("http://localhost:11434"), "localhost:11434");
    }
}
