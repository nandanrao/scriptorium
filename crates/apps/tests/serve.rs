//! The apps server over real sockets, with a fake host, on a free port.

#![cfg(feature = "serve")]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use scriptorium_apps::events::Event;
use scriptorium_apps::serve::{Call, CallAnswer, Host, Open, Running, Server};
use scriptorium_apps::{BODY_MAX, LISTENERS_MAX};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const KEY: &str = "00112233445566aa";
const OTHER: &str = "ffeeddccbbaa9988";
const CSP_START: &str = "default-src 'self'";

struct Fake {
    open: Mutex<Option<Open>>,
    answer: Mutex<CallAnswer>,
    calls: AtomicUsize,
    last: Mutex<Option<(String, String, Call)>>,
}

impl Host for Fake {
    fn open(&self) -> Option<Open> {
        self.open.lock().unwrap().clone()
    }
    fn call(&self, app: &str, project: &str, call: Call) -> CallAnswer {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last.lock().unwrap() = Some((app.into(), project.into(), call));
        self.answer.lock().unwrap().clone()
    }
}

struct Setup {
    port: u16,
    dir: PathBuf,
    host: Arc<Fake>,
    running: Running,
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("scriptorium-apps-serve-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn app(dir: &Path, name: &str) -> PathBuf {
    let a = dir.join("apps").join(name);
    fs::create_dir_all(&a).unwrap();
    fs::write(a.join("app.json"), r#"{"scriptorium": 1, "title": "Scene list", "description": "Every scene."}"#)
        .unwrap();
    fs::write(a.join("index.html"), "<p>hi</p>").unwrap();
    a
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn setup(name: &str) -> Setup {
    let dir = temp(name);
    app(&dir, "scene-list");
    let host = Arc::new(Fake {
        open: Mutex::new(Some(Open { project: KEY.into(), script: "film.fdx".into(), dir: dir.clone() })),
        answer: Mutex::new(CallAnswer::Done { text: "INT. KITCHEN".into(), is_error: false }),
        calls: AtomicUsize::new(0),
        last: Mutex::new(None),
    });
    let port = free_port();
    let running = Server::start(port, TOKEN.into(), host.clone()).expect("start");
    Setup { port, dir, host, running }
}

#[derive(Debug)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap()
    }
    /// Every answer carries the fixed headers, the CSP first among them.
    fn has_fixed_headers(&self) -> bool {
        self.header("content-security-policy").is_some_and(|v| v.starts_with(CSP_START))
            && self.header("cache-control") == Some("no-store")
            && self.header("x-content-type-options") == Some("nosniff")
            && self.header("referrer-policy") == Some("no-referrer")
    }
}

/// Sends one request (`Connection: close` added) and reads the whole answer.
fn raw(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: Option<&[u8]>) -> Reply {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\n");
    for (n, v) in headers {
        req.push_str(&format!("{n}: {v}\r\n"));
    }
    if let Some(b) = body {
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    if let Some(b) = body {
        // The server may answer and close before reading it all.
        let _ = s.write_all(b);
    }
    read_reply(s)
}

fn read_reply(mut s: TcpStream) -> Reply {
    let mut all = Vec::new();
    let _ = s.read_to_end(&mut all);
    let split = all.windows(4).position(|w| w == b"\r\n\r\n").expect("a head");
    let head = String::from_utf8_lossy(&all[..split]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let headers =
        lines.filter_map(|l| l.split_once(':')).map(|(n, v)| (n.trim().to_string(), v.trim().to_string())).collect();
    Reply { status, headers, body: all[split + 4..].to_vec() }
}

fn host_line(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

fn cookie() -> String {
    format!("scriptorium={TOKEN}")
}

fn origin(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// A request as a page sends it: our `Host`, the cookie, and `Origin` on
/// writes.
fn page(port: u16, method: &str, path: &str, body: Option<&[u8]>) -> Reply {
    let (h, c, o) = (host_line(port), cookie(), origin(port));
    let mut headers = vec![("Host", h.as_str()), ("Cookie", c.as_str())];
    if method != "GET" {
        headers.push(("Origin", o.as_str()));
    }
    raw(port, method, path, &headers, body)
}

fn get_no_cookie(port: u16, path: &str) -> Reply {
    let h = host_line(port);
    raw(port, "GET", path, &[("Host", &h)], None)
}

#[test]
fn exchange_sets_the_cookie_and_moves_on() {
    let s = setup("exchange");
    let to = format!("/p/{KEY}/scene-list/");
    let r = get_no_cookie(s.port, &format!("/open?t={TOKEN}&to={to}"));
    assert_eq!(r.status, 200);
    let set = r.header("set-cookie").unwrap();
    assert!(set.starts_with(&format!("scriptorium={TOKEN};")), "{set}");
    assert!(set.contains("HttpOnly") && set.contains("SameSite=Lax"), "{set}");
    assert!(r.text().contains(&format!("url={to}")), "{}", r.text());
    assert!(!r.text().contains(TOKEN));
    assert!(r.has_fixed_headers());

    let wrong = get_no_cookie(s.port, &format!("/open?t=nope&to={to}"));
    assert_eq!(wrong.status, 401);
    assert!(wrong.header("set-cookie").is_none());

    let with = page(s.port, "GET", &to, None);
    assert_eq!(with.status, 200);
    assert_eq!(with.text(), "<p>hi</p>");
    assert!(with.header("content-type").unwrap().starts_with("text/html"));
    assert!(with.has_fixed_headers());

    let without = get_no_cookie(s.port, &to);
    assert_eq!(without.status, 401);
    assert!(without.text().contains("Apps menu"), "{}", without.text());
    assert!(without.has_fixed_headers(), "the 401 page carries the CSP too");

    let index = page(s.port, "GET", "/", None);
    assert_eq!(index.status, 200);
    assert!(index.text().contains("Scene list") && index.text().contains(&to), "{}", index.text());
    assert_eq!(get_no_cookie(s.port, "/").status, 401);

    let redirect = page(s.port, "GET", &format!("/p/{KEY}/scene-list"), None);
    assert_eq!(redirect.status, 302);
    assert_eq!(redirect.header("location"), Some(to.as_str()));
}

#[test]
fn refusals_end_to_end() {
    let s = setup("refusals");
    let (c, o) = (cookie(), origin(s.port));
    let call = format!("/api/p/{KEY}/a/scene-list/call");
    let body = br#"{"name": "script_info"}"#;

    let r = raw(s.port, "GET", "/", &[("Host", &format!("evil.example:{}", s.port)), ("Cookie", &c)], None);
    assert_eq!(r.status, 421);
    assert!(r.has_fixed_headers());

    let h = host_line(s.port);
    let r = raw(s.port, "POST", &call, &[("Host", &h), ("Cookie", &c)], Some(body));
    assert_eq!(r.status, 403);
    assert!(r.has_fixed_headers());
    let r = raw(s.port, "POST", &call, &[("Host", &h), ("Cookie", &c), ("Origin", "http://evil.example")], Some(body));
    assert_eq!(r.status, 403);

    let big = "x".repeat(17 * 1024);
    let r = raw(s.port, "GET", "/", &[("Host", &h), ("Cookie", &c), ("X-Big", &big)], None);
    assert_eq!(r.status, 431);
    assert!(r.has_fixed_headers());

    // A body one byte over, announced.
    let over = vec![b' '; BODY_MAX as usize + 1];
    let r = raw(s.port, "POST", &call, &[("Host", &h), ("Cookie", &c), ("Origin", &o)], Some(&over));
    assert_eq!(r.status, 413);
    assert!(r.has_fixed_headers());

    // The same, chunked: no Content-Length, cut off while reading.
    let mut stream = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let head = format!(
        "POST {call} HTTP/1.1\r\nHost: {h}\r\nCookie: {c}\r\nOrigin: {o}\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).unwrap();
    let _ = stream.write_all(format!("{:x}\r\n", over.len()).as_bytes());
    let _ = stream.write_all(&over);
    let _ = stream.write_all(b"\r\n0\r\n\r\n");
    let r = read_reply(stream);
    assert_eq!(r.status, 413);
    assert_eq!(s.host.calls.load(Ordering::SeqCst), 0, "host.call never ran");

    // The cookie checked on the API too.
    let r = raw(s.port, "POST", &call, &[("Host", &h), ("Origin", &o)], Some(body));
    assert_eq!(r.status, 401);
    assert_eq!(s.host.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_huge_announced_body_is_413_and_the_server_lives_on() {
    let s = setup("huge");
    let (h, c, o) = (host_line(s.port), cookie(), origin(s.port));
    for path in [format!("/api/p/{KEY}/a/scene-list/call"), format!("/api/p/{KEY}/a/scene-list/data/x.json")] {
        let method = if path.ends_with("call") { "POST" } else { "PUT" };
        let mut stream = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {h}\r\nCookie: {c}\r\nOrigin: {o}\r\nContent-Length: 1000000000000000\r\n\r\nsome"
        );
        stream.write_all(head.as_bytes()).unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        while !got.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut buf).unwrap();
            assert!(n > 0, "an answer before the close");
            got.extend_from_slice(&buf[..n]);
        }
        assert!(got.starts_with(b"HTTP/1.1 413"), "{}", String::from_utf8_lossy(&got));
        drop(stream);
    }
    assert_eq!(s.host.calls.load(Ordering::SeqCst), 0);
    assert_eq!(page(s.port, "GET", "/api/status", None).status, 200, "still serving");
}

#[test]
fn paths_and_links_refused() {
    let s = setup("paths");
    let base = format!("/p/{KEY}/scene-list");
    assert_eq!(page(s.port, "GET", &format!("{base}/../app.json"), None).status, 404);
    assert_eq!(page(s.port, "GET", &format!("{base}/%2e%2e/app.json"), None).status, 404);
    assert_eq!(page(s.port, "GET", &format!("{base}/%2e%2e/%2e%2e/film.fdx"), None).status, 404);
    assert_eq!(page(s.port, "GET", &format!("{base}/app.json"), None).status, 200);
    assert_eq!(page(s.port, "GET", &format!("{base}/missing.js"), None).status, 404);
    let api = format!("/api/p/{KEY}/a/scene-list/data");
    assert_eq!(page(s.port, "GET", &format!("{api}/%2e%2e/app.json"), None).status, 404);

    #[cfg(unix)]
    {
        let outside = temp("paths-outside");
        fs::write(outside.join("secret.json"), "{}").unwrap();
        std::os::unix::fs::symlink(&outside, s.dir.join("apps/scene-list/data")).unwrap();
        let r = page(s.port, "GET", &format!("{api}/secret.json"), None);
        assert_eq!(r.status, 404);
        assert_eq!(page(s.port, "GET", &format!("{api}/"), None).status, 404);
        let r = page(s.port, "PUT", &format!("{api}/new.json"), Some(b"{}"));
        assert_eq!(r.status, 404);
        assert!(!outside.join("new.json").exists(), "nothing written through the link");
    }
}

#[test]
fn data_round_trip_and_not_static() {
    let s = setup("data");
    let api = format!("/api/p/{KEY}/a/scene-list/data");
    assert_eq!(page(s.port, "GET", &format!("{api}/"), None).json(), serde_json::json!([]));
    let r = page(s.port, "PUT", &format!("{api}/notes.json"), Some(br#"{"a": 1}"#));
    assert_eq!(r.status, 204);
    assert!(r.has_fixed_headers());
    assert_eq!(page(s.port, "PUT", &format!("{api}/img/a.png"), Some(b"png")).status, 204);
    let r = page(s.port, "GET", &format!("{api}/notes.json"), None);
    assert_eq!(r.status, 200);
    assert_eq!(r.body, br#"{"a": 1}"#);
    assert_eq!(r.header("content-type"), Some("application/json"));
    assert_eq!(
        page(s.port, "GET", &format!("{api}/"), None).json(),
        serde_json::json!([{"path": "img/a.png", "size": 3}, {"path": "notes.json", "size": 8}])
    );
    // The data folder is never a page.
    assert_eq!(page(s.port, "GET", &format!("/p/{KEY}/scene-list/data/notes.json"), None).status, 404);
    assert_eq!(page(s.port, "DELETE", &format!("{api}/notes.json"), None).status, 204);
    assert_eq!(page(s.port, "GET", &format!("{api}/notes.json"), None).status, 404);
    assert_eq!(page(s.port, "DELETE", &format!("{api}/notes.json"), None).status, 404);
}

#[test]
fn another_folder_is_not_open() {
    let s = setup("not-open");
    let r = page(s.port, "GET", &format!("/p/{OTHER}/scene-list/"), None);
    assert_eq!(r.status, 409);
    assert!(r.text().contains("/scriptorium.js"));
    assert!(r.has_fixed_headers());
    let r = page(s.port, "POST", &format!("/api/p/{OTHER}/a/scene-list/call"), Some(br#"{"name": "script_info"}"#));
    assert_eq!(r.status, 409);
    assert_eq!(r.json(), serde_json::json!({"error": "not-open"}));
    assert_eq!(page(s.port, "GET", &format!("/api/p/{OTHER}/a/scene-list/data/x.json"), None).status, 409);
    assert_eq!(s.host.calls.load(Ordering::SeqCst), 0);

    *s.host.open.lock().unwrap() = None;
    let status = page(s.port, "GET", "/api/status", None).json();
    assert_eq!(status["project"], serde_json::Value::Null);
    assert_eq!(page(s.port, "GET", &format!("/p/{KEY}/scene-list/"), None).status, 409);
}

#[test]
fn calls_status_and_library() {
    let s = setup("calls");
    let call = format!("/api/p/{KEY}/a/scene-list/call");
    let r = page(s.port, "POST", &call, Some(br#"{"name": "read_scene", "arguments": {"scene": 1}, "project": "x"}"#));
    assert_eq!(r.status, 200);
    assert_eq!(r.json(), serde_json::json!({"text": "INT. KITCHEN", "is_error": false}));
    let (app, project, c) = s.host.last.lock().unwrap().clone().unwrap();
    assert_eq!((app.as_str(), project.as_str(), c.name.as_str()), ("scene-list", KEY, "read_scene"));
    assert_eq!(c.arguments, serde_json::json!({"scene": 1}));

    let r = page(s.port, "POST", &call, Some(br#"{"name": "script_info"}"#));
    assert_eq!(r.status, 200);
    assert_eq!(s.host.last.lock().unwrap().clone().unwrap().2.arguments, serde_json::json!({}));

    *s.host.answer.lock().unwrap() = CallAnswer::Busy;
    let r = page(s.port, "POST", &call, Some(br#"{"name": "script_info"}"#));
    assert_eq!((r.status, r.json()), (503, serde_json::json!({"error": "busy"})));
    *s.host.answer.lock().unwrap() = CallAnswer::NotOpen;
    assert_eq!(page(s.port, "POST", &call, Some(br#"{"name": "script_info"}"#)).status, 409);

    let calls = s.host.calls.load(Ordering::SeqCst);
    assert_eq!(page(s.port, "POST", &call, Some(b"not json")).status, 400);
    assert_eq!(page(s.port, "POST", &call, Some(br#"{"arguments": {}}"#)).status, 400);
    assert_eq!(page(s.port, "POST", &call, Some(br#"{"name": "x", "arguments": [1]}"#)).status, 400);
    assert_eq!(s.host.calls.load(Ordering::SeqCst), calls);

    let status = page(s.port, "GET", "/api/status", None);
    assert_eq!(
        status.json(),
        serde_json::json!({"app": "scriptorium", "version": scriptorium_apps::VERSION, "project": KEY, "script": "film.fdx"})
    );
    let lib = page(s.port, "GET", "/scriptorium.js", None);
    assert_eq!(lib.status, 200);
    assert!(lib.header("content-type").unwrap().starts_with("text/javascript"));
    assert_eq!(lib.text(), scriptorium_apps::LIBRARY);
}

#[test]
fn log_then_seen() {
    let s = setup("log");
    let log = format!("/api/p/{KEY}/a/scene-list/log");
    assert!(s.running.seen().is_empty());
    assert_eq!(page(s.port, "POST", &log, Some(br#"{"loaded": true}"#)).status, 204);
    assert_eq!(page(s.port, "POST", &log, Some(br#"{"error": "x is not defined (app.js:3)"}"#)).status, 204);
    let seen = s.running.seen();
    let app = &seen["scene-list"];
    assert!(app.loaded);
    assert_eq!(app.error.as_deref(), Some("x is not defined (app.js:3)"));
    assert_eq!(page(s.port, "POST", &log, Some(b"[")).status, 400);
}

/// An event stream: the head, then frames read one at a time.
struct Stream {
    reader: BufReader<TcpStream>,
}

impl Stream {
    fn open(port: u16) -> Result<Stream, Reply> {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let req = format!("GET /api/events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: {}\r\n\r\n", cookie());
        s.write_all(req.as_bytes()).unwrap();
        let mut reader = BufReader::new(s);
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        if !status.contains(" 200 ") {
            let mut rest = Vec::new();
            let _ = reader.read_to_end(&mut rest);
            let mut all = status.into_bytes();
            all.extend(rest);
            let code = String::from_utf8_lossy(&all).split(' ').nth(1).unwrap().parse().unwrap();
            return Err(Reply { status: code, headers: vec![], body: all });
        }
        let mut stream = Stream { reader };
        let head = stream.block();
        assert!(head.iter().any(|l| l.starts_with("Content-Type: text/event-stream")), "{head:?}");
        assert!(head.iter().any(|l| l.starts_with("Content-Security-Policy:")), "{head:?}");
        Ok(stream)
    }

    /// Lines up to the next blank one.
    fn block(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line).unwrap_or(0) == 0 {
                panic!("stream ended");
            }
            let line = line.trim_end_matches(['\r', '\n']).to_string();
            if line.is_empty() {
                return lines;
            }
            lines.push(line);
        }
    }

    /// The next event (skipping pings) as (name, data), or `None` on a
    /// read timeout.
    fn next(&mut self, timeout: Duration) -> Option<(String, serde_json::Value)> {
        self.reader.get_ref().set_read_timeout(Some(timeout)).unwrap();
        loop {
            let mut lines = Vec::new();
            loop {
                let mut line = String::new();
                match self.reader.read_line(&mut line) {
                    Ok(0) => panic!("stream ended"),
                    Ok(_) => {}
                    Err(_) => return None,
                }
                let line = line.trim_end_matches(['\r', '\n']).to_string();
                if line.is_empty() {
                    break;
                }
                lines.push(line);
            }
            let name = lines.iter().find_map(|l| l.strip_prefix("event: "));
            let data = lines.iter().find_map(|l| l.strip_prefix("data: "));
            if let (Some(n), Some(d)) = (name, data) {
                return Some((n.to_string(), serde_json::from_str(d).unwrap()));
            }
        }
    }
}

fn wait_for(what: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if what() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn events_hello_and_broadcast() {
    let s = setup("events");
    let mut stream = Stream::open(s.port).unwrap();
    let (name, data) = stream.next(Duration::from_secs(2)).unwrap();
    assert_eq!(name, "hello");
    assert_eq!(data["project"], KEY);
    assert_eq!(data["script"], "film.fdx");
    assert!(wait_for(|| s.running.listeners() == 1));

    let sent = Instant::now();
    s.running.broadcast(&Event::Script);
    let (name, _) = stream.next(Duration::from_secs(1)).expect("within a second");
    assert_eq!(name, "script");
    assert!(sent.elapsed() < Duration::from_secs(1));

    drop(stream);
    // A closed tab is found on a later write.
    assert!(wait_for(|| {
        s.running.broadcast(&Event::Script);
        s.running.listeners() == 0
    }));
}

#[test]
fn the_33rd_listener_is_busy() {
    let s = setup("listeners");
    let streams: Vec<Stream> = (0..LISTENERS_MAX).map(|_| Stream::open(s.port).unwrap()).collect();
    assert!(wait_for(|| s.running.listeners() == LISTENERS_MAX));
    let r = Stream::open(s.port).err().expect("refused");
    assert_eq!(r.status, 503);
    drop(streams);
}

#[test]
fn stop_ends_streams_and_frees_the_port() {
    let s = setup("stop");
    let mut stream = Stream::open(s.port).unwrap();
    assert_eq!(stream.next(Duration::from_secs(2)).unwrap().0, "hello");
    s.running.stop();
    s.running.stop();
    let again = Server::start(s.port, TOKEN.into(), s.host.clone()).expect("the port is free again");
    again.stop();
}

#[test]
fn edits_give_one_reload_after_a_second_of_quiet() {
    let s = setup("reload");
    let mut stream = Stream::open(s.port).unwrap();
    assert_eq!(stream.next(Duration::from_secs(2)).unwrap().0, "hello");
    // Let the stamps thread take its first stamps (the baseline).
    std::thread::sleep(Duration::from_millis(1300));
    let index = s.dir.join("apps/scene-list/index.html");
    let mut last = Instant::now();
    for i in 0..3 {
        if i > 0 {
            std::thread::sleep(Duration::from_millis(900));
        }
        fs::write(&index, "<p>hi</p>".repeat(i + 2)).unwrap();
        last = Instant::now();
    }
    // Data changes never reload.
    fs::create_dir_all(s.dir.join("apps/scene-list/data")).unwrap();
    fs::write(s.dir.join("apps/scene-list/data/x.json"), "{}").unwrap();
    let (name, data) = stream.next(Duration::from_secs(4)).expect("a reload");
    let after = last.elapsed();
    assert_eq!(name, "reload");
    assert_eq!(data, serde_json::json!({"app": "scene-list"}));
    assert!(after >= Duration::from_millis(950), "a second after the last edit, got {after:?}");
    assert!(after < Duration::from_millis(3000), "{after:?}");
    assert!(stream.next(Duration::from_millis(2500)).is_none(), "only one reload");
}

#[test]
fn a_held_port_is_taken() {
    let held = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port();
    let host = Arc::new(Fake {
        open: Mutex::new(None),
        answer: Mutex::new(CallAnswer::Busy),
        calls: AtomicUsize::new(0),
        last: Mutex::new(None),
    });
    let taken = Server::start(port, TOKEN.into(), host).err().expect("taken");
    assert_eq!(taken.port, port);
    assert_eq!(
        taken.to_string(),
        format!("Apps can't open: another program is using port {port}. Quit it and restart Scriptorium.")
    );
}
