//! The apps server (feature `serve`): HTTP on `127.0.0.1`, run by the
//! Scriptorium app while it is open.
//!
//! [`Server::start`] binds the port (no fallback: a port in use is
//! [`Taken`]) and runs an accept thread that hands each request to a thread
//! of its own. Every request goes through [`route::decide`] (pure); this
//! module only carries out the [`Decision`]: files and data through
//! [`crate::shell`], calls through the [`Host`], page reports into
//! [`Seen`], and the event stream.
//!
//! - **Bodies** are read up to the decision's [`Decision::body_max`] and no
//!   further, whatever `Content-Length` said (or with none, chunked).
//! - **Events** (`GET /api/events`): at most [`LISTENERS_MAX`] listeners,
//!   each a thread with a small queue. [`Running::broadcast`] never waits:
//!   a listener whose queue is full is dropped (the page's library
//!   reconnects). Each frame is flushed as it is written. A closed tab is
//!   noticed only when a write to it fails, which is usually the second
//!   write after it closed (a broadcast or the 15 s ping): up to about
//!   30 s on a quiet stream. Until then it counts as a listener.
//! - **Headers:** the 16 KB limit (431) is a policy check made after
//!   tiny_http has read the whole head into memory; it is not a memory
//!   guard (tiny_http has no header limit).
//! - **Reloads:** while a page listens, a thread stamps the open folder's
//!   apps once a second ([`shell::stamp`]) and broadcasts `reload` after a
//!   second of quiet ([`Quiet`]). With no listeners it stamps nothing, and
//!   starts over (a fresh [`Quiet`]) when one comes back.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tiny_http::{Header, Request, Response};

use crate::events::{Event, PING, Quiet};
use crate::list::{Seen, SeenApp};
use crate::route::{self, Decision, Head, Page, State};
use crate::shell::{self, FileError};
use crate::{LIBRARY, LISTENERS_MAX, PORT, VERSION};

/// Replaces [`PORT`] in tests and scenarios, so they never meet a real copy.
pub const TEST_PORT_ENV: &str = "SCRIPTORIUM_TEST_APPS_PORT";
/// How often an idle event stream is pinged.
pub const PING_EVERY: Duration = Duration::from_secs(15);
/// How often the apps' files are stamped while a page listens.
pub const STAMP_EVERY: Duration = Duration::from_secs(1);
/// Frames waiting for one listener; one more and it is dropped as slow.
pub const QUEUE: usize = 16;
/// The longest [`Running::stop`] waits for the port to be free.
const STOP_WAIT: Duration = Duration::from_secs(2);
/// How long the check for something already on the port waits.
const PROBE_TIMEOUT: Duration = Duration::from_millis(200);
/// The longest page error kept, in characters.
pub const ERROR_MAX: usize = 2000;
/// The most apps [`Running::seen`] keeps reports for.
pub const SEEN_MAX: usize = 200;
/// The port to serve on: [`PORT`], or [`TEST_PORT_ENV`] when it is set to a
/// port number.
pub fn port() -> u16 {
    std::env::var(TEST_PORT_ENV).ok().and_then(|v| v.trim().parse().ok()).filter(|p| *p != 0).unwrap_or(PORT)
}

/// The open script, as the server needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Open {
    /// The folder's project key ([`crate::paths::project_key`]).
    pub project: String,
    /// The script's file name (`PURPLE LIST.fdx`).
    pub script: String,
    /// The script's folder: apps are in `<dir>/apps/`.
    pub dir: PathBuf,
}

/// A tool call from an app's page: `POST …/call` `{"name", "arguments"}`.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub name: String,
    /// A JSON object (`{}` when the page sent none).
    pub arguments: Value,
}

/// How the host answered a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallAnswer {
    /// The tool ran: 200 `{"text", "is_error"}`.
    Done { text: String, is_error: bool },
    /// The open folder isn't the page's (any more): 409 `{"error": "not-open"}`.
    NotOpen,
    /// Too much waiting, or the script is busy: 503 `{"error": "busy"}`.
    Busy,
}

/// What the server asks of the running app. Called from the server's
/// threads; `open` is called on every request and once a second while a
/// page listens, so it must be cheap.
pub trait Host: Send + Sync {
    /// The open script, if any.
    fn open(&self) -> Option<Open>;
    /// Runs `call` for app `app` on the script of folder `project`
    /// (checked against the open one when the request came in; the host
    /// checks again when it runs it).
    fn call(&self, app: &str, project: &str, call: Call) -> CallAnswer;
}

/// The port couldn't be bound: apps don't run this time.
#[derive(Debug)]
pub struct Taken {
    pub port: u16,
    pub error: io::Error,
}

impl fmt::Display for Taken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.error.kind() == io::ErrorKind::AddrInUse {
            write!(f, "Apps can't open: another program is using port {}. Quit it and restart Scriptorium.", self.port)
        } else {
            write!(f, "Apps can't open: port {} can't be used ({}).", self.port, self.error)
        }
    }
}

impl std::error::Error for Taken {}

/// Starts the apps server.
pub struct Server;

impl Server {
    /// Binds `127.0.0.1:<port>` and serves until [`Running::stop`] (or the
    /// [`Running`] is dropped). `token` is the per-install token the cookie
    /// must carry; it must not be empty.
    pub fn start(port: u16, token: String, host: Arc<dyn Host>) -> Result<Running, Taken> {
        // Something already answering there is "taken" even where binding
        // would succeed beside it (macOS lets 127.0.0.1 bind next to a
        // program on 0.0.0.0 with the same port).
        if answers(port) {
            return Err(Taken { port, error: io::ErrorKind::AddrInUse.into() });
        }
        let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|error| Taken { port, error })?;
        let server = tiny_http::Server::from_listener(listener, None)
            .map_err(|e| Taken { port, error: io::Error::other(e.to_string()) })?;
        let server = Arc::new(server);
        let shared = Arc::new(Shared {
            port,
            token,
            host,
            listeners: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(0),
            seen: Mutex::new(Seen::new()),
            stopping: AtomicBool::new(false),
        });
        let accept = {
            let (server, shared) = (server.clone(), shared.clone());
            thread::Builder::new()
                .name("apps-accept".into())
                .spawn(move || accept_loop(&server, &shared))
                .map_err(|error| Taken { port, error })?
        };
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let stamps = {
            let shared = shared.clone();
            thread::Builder::new().name("apps-stamps".into()).spawn(move || stamp_loop(&shared, &stop_rx)).ok()
        };
        Ok(Running { shared, parts: Mutex::new(Some(Parts { server, accept, stamps, stop_tx })) })
    }
}

/// The server while it runs. Dropping it stops it.
pub struct Running {
    shared: Arc<Shared>,
    parts: Mutex<Option<Parts>>,
}

struct Parts {
    server: Arc<tiny_http::Server>,
    accept: JoinHandle<()>,
    stamps: Option<JoinHandle<()>>,
    /// Dropped to wake the stamps thread and end it.
    stop_tx: Sender<()>,
}

impl Running {
    /// The port it listens on.
    pub fn port(&self) -> u16 {
        self.shared.port
    }

    /// Sends `event` to every listening page. Never waits: a listener
    /// whose queue is full is dropped.
    pub fn broadcast(&self, event: &Event) {
        self.shared.broadcast(&event.frame());
    }

    /// How many pages listen to events now, counting closed tabs not yet
    /// noticed (see the module notes: up to about 30 s).
    pub fn listeners(&self) -> usize {
        lock(&self.shared.listeners).len()
    }

    /// What pages reported this run, per app (for `list_apps`).
    pub fn seen(&self) -> Seen {
        lock(&self.shared.seen).clone()
    }

    /// Stops serving: the port is free when this returns. Open event
    /// streams end; requests already being answered finish. Calling it
    /// again does nothing.
    pub fn stop(&self) {
        let Some(parts) = lock(&self.parts).take() else {
            return;
        };
        self.shared.stopping.store(true, Ordering::SeqCst);
        drop(parts.stop_tx);
        parts.server.unblock();
        let _ = parts.accept.join();
        if let Some(stamps) = parts.stamps {
            let _ = stamps.join();
        }
        // The last handle: tiny_http's own accept thread ends and the
        // listening socket closes.
        drop(parts.server);
        lock(&self.shared.listeners).clear();
        // tiny_http closes the socket on its own thread, a moment later;
        // wait for it (briefly) so a restart finds the port free.
        let start = Instant::now();
        while answers(self.shared.port) && start.elapsed() < STOP_WAIT {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Shared {
    port: u16,
    token: String,
    host: Arc<dyn Host>,
    listeners: Mutex<Vec<Listener>>,
    next_id: AtomicU64,
    seen: Mutex<Seen>,
    stopping: AtomicBool,
}

struct Listener {
    id: u64,
    tx: SyncSender<String>,
}

impl Shared {
    fn broadcast(&self, frame: &str) {
        lock(&self.listeners).retain(|l| match l.tx.try_send(frame.to_string()) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        });
    }

    fn remove_listener(&self, id: u64) {
        lock(&self.listeners).retain(|l| l.id != id);
    }
}

/// Whether something accepts connections on `127.0.0.1:<port>`.
fn answers(port: u16) -> bool {
    TcpStream::connect_timeout(&(Ipv4Addr::LOCALHOST, port).into(), PROBE_TIMEOUT).is_ok()
}

/// A poisoned lock still holds good data here (plain lists and maps).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn accept_loop(server: &tiny_http::Server, shared: &Arc<Shared>) {
    while !shared.stopping.load(Ordering::SeqCst) {
        match server.recv() {
            Ok(request) => {
                let shared = shared.clone();
                // A failed spawn drops the request: tiny_http answers 500.
                let _ = thread::Builder::new().name("apps-request".into()).spawn(move || handle(&shared, request));
            }
            Err(_) if shared.stopping.load(Ordering::SeqCst) => break,
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn stamp_loop(shared: &Shared, stop: &mpsc::Receiver<()>) {
    let start = Instant::now();
    let mut quiet = Quiet::new();
    let mut dir: Option<PathBuf> = None;
    loop {
        match stop.recv_timeout(STAMP_EVERY) {
            Err(RecvTimeoutError::Timeout) => {}
            _ => return,
        }
        if lock(&shared.listeners).is_empty() {
            // Stamps from before a pause would make every app edited
            // during it reload at once.
            quiet = Quiet::new();
            dir = None;
            continue;
        }
        let now_dir = shared.host.open().map(|o| o.dir);
        if now_dir != dir {
            quiet = Quiet::new();
            dir = now_dir;
        }
        let Some(d) = &dir else {
            continue;
        };
        let now = start.elapsed().as_millis() as u64;
        for event in quiet.step(now, shell::stamp(d)) {
            shared.broadcast(&event.frame());
        }
    }
}

/// The request's head for [`route::decide`]. `header_bytes` is counted
/// from what tiny_http already read, so the 431 it leads to limits what is
/// accepted, not what is read.
fn head_of(request: &Request) -> Head {
    let url = request.url();
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let method = request.method().as_str().to_string();
    let headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .map(|h| (h.field.as_str().as_str().to_string(), h.value.as_str().to_string()))
        .collect();
    // `<method> <url> HTTP/1.1\r\n`, each `name: value\r\n`, the blank line.
    let header_bytes =
        method.len() + url.len() + 12 + headers.iter().map(|(n, v)| n.len() + v.len() + 4).sum::<usize>() + 2;
    Head { method, path: path.to_string(), query: query.to_string(), headers, header_bytes }
}

/// An answer about to be sent.
struct Answer {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    extra: Vec<(&'static str, String)>,
}

impl Answer {
    fn new(status: u16, content_type: &'static str, body: impl Into<Vec<u8>>) -> Answer {
        Answer { status, content_type, body: body.into(), extra: Vec::new() }
    }

    fn json(status: u16, value: &Value) -> Answer {
        Answer::new(status, "application/json", value.to_string())
    }

    fn page(status: u16, page: Page) -> Answer {
        let (content_type, body) = page.body();
        Answer::new(status, content_type, body)
    }

    fn empty(status: u16) -> Answer {
        Answer::new(status, "text/plain; charset=utf-8", Vec::new())
    }

    fn with(mut self, name: &'static str, value: String) -> Answer {
        self.extra.push((name, value));
        self
    }

    fn send(self, request: Request) {
        let mut response = Response::from_data(self.body).with_status_code(self.status);
        let mut add = |name: &str, value: &str| {
            if let Ok(h) = Header::from_bytes(name.as_bytes(), value.as_bytes()) {
                response.add_header(h);
            }
        };
        add("Content-Type", self.content_type);
        for (name, value) in route::headers() {
            add(name, value);
        }
        for (name, value) in &self.extra {
            add(name, value);
        }
        let _ = request.respond(response);
    }
}

fn handle(shared: &Shared, mut request: Request) {
    let head = head_of(&request);
    let open = shared.host.open();
    let state =
        State { port: shared.port, token: shared.token.clone(), project: open.as_ref().map(|o| o.project.clone()) };
    let decision = route::decide(&head, &state);
    if decision == Decision::Events {
        return events(shared, request, open);
    }
    let body = match decision.body_max() {
        None => Vec::new(),
        // The length tiny_http will read by (`decide` saw the header only
        // when it was sent once). An unread body is drained with a fixed
        // buffer (vendor/tiny_http/SCRIPTORIUM.md).
        Some(max) if request.body_length().is_some_and(|n| n as u64 > max) => {
            return Answer::page(413, Page::TooLarge).with("Connection", "close".to_string()).send(request);
        }
        Some(max) => match read_body(&mut request, max) {
            Ok(body) => body,
            Err(answer) => return answer.send(request),
        },
    };
    answer(shared, decision, open, body).send(request);
}

/// The body, at most `max` bytes: 413 past it, 400 when it can't be read.
fn read_body(request: &mut Request, max: u64) -> Result<Vec<u8>, Answer> {
    let mut body = Vec::new();
    match request.as_reader().take(max + 1).read_to_end(&mut body) {
        Ok(_) if body.len() as u64 > max => {
            Err(Answer::page(413, Page::TooLarge).with("Connection", "close".to_string()))
        }
        Ok(_) => Ok(body),
        Err(_) => Err(Answer::page(400, Page::BadRequest).with("Connection", "close".to_string())),
    }
}

fn answer(shared: &Shared, decision: Decision, open: Option<Open>, body: Vec<u8>) -> Answer {
    let not_open = || Answer::page(409, Page::NotOpen);
    let bad = || Answer::page(400, Page::BadRequest);
    match decision {
        Decision::Refuse(status, page) => Answer::page(status, page),
        Decision::Exchange { to } => Answer::new(200, "text/html; charset=utf-8", route::exchange_page(&to))
            .with("Set-Cookie", route::cookie_line(&shared.token)),
        Decision::Redirect { to } => Answer::empty(302).with("Location", to),
        Decision::Index => {
            let apps = open.as_ref().map(|o| shell::scan(&o.dir)).unwrap_or_default();
            let page = route::index_page(open.as_ref().map(|o| o.project.as_str()), &apps);
            Answer::new(200, "text/html; charset=utf-8", page)
        }
        Decision::Library => Answer::new(200, route::mime("js"), LIBRARY),
        Decision::NotOpenPage => Answer::new(409, "text/html; charset=utf-8", route::NOT_OPEN_PAGE),
        Decision::Status => Answer::json(
            200,
            &json!({
                "app": "scriptorium",
                "version": VERSION,
                "project": open.as_ref().map(|o| &o.project),
                "script": open.as_ref().map(|o| &o.script),
            }),
        ),
        Decision::File { app, path } => {
            let Some(open) = open else { return not_open() };
            let ext = path.last().and_then(|p| p.rsplit_once('.')).map_or("", |(_, e)| e);
            match shell::read_static(&open.dir, &app, &path) {
                Ok(bytes) => Answer::new(200, route::mime(ext), bytes),
                Err(e) => file_error(e),
            }
        }
        Decision::Call { app, project } => {
            let Ok(Value::Object(mut body)) = serde_json::from_slice::<Value>(&body) else { return bad() };
            let Some(Value::String(name)) = body.remove("name") else { return bad() };
            let arguments = match body.remove("arguments") {
                None | Some(Value::Null) => json!({}),
                Some(a @ Value::Object(_)) => a,
                Some(_) => return bad(),
            };
            match shared.host.call(&app, &project, Call { name, arguments }) {
                CallAnswer::Done { text, is_error } => Answer::json(200, &json!({"text": text, "is_error": is_error})),
                CallAnswer::NotOpen => not_open(),
                CallAnswer::Busy => Answer::json(503, &json!({"error": "busy"})),
            }
        }
        Decision::Log { app } => {
            let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(&body) else { return bad() };
            let mut seen = lock(&shared.seen);
            if !seen.contains_key(&app) && seen.len() >= SEEN_MAX {
                return Answer::empty(204);
            }
            let entry: &mut SeenApp = seen.entry(app).or_default();
            if body.get("loaded") == Some(&Value::Bool(true)) {
                entry.loaded = true;
            }
            if let Some(Value::String(error)) = body.get("error") {
                entry.error = Some(error.chars().take(ERROR_MAX).collect());
            }
            Answer::empty(204)
        }
        Decision::DataList { app } => {
            let Some(open) = open else { return not_open() };
            match shell::data_list(&open.dir, &app) {
                Ok(files) => Answer::json(
                    200,
                    &Value::Array(files.into_iter().map(|f| json!({"path": f.path, "size": f.size})).collect()),
                ),
                Err(e) => file_error(e),
            }
        }
        Decision::DataRead { app, path } => {
            let Some(open) = open else { return not_open() };
            let ext = path.last().and_then(|p| p.rsplit_once('.')).map_or("", |(_, e)| e);
            match shell::data_read(&open.dir, &app, &path) {
                Ok(bytes) => Answer::new(200, route::mime(ext), bytes),
                Err(e) => file_error(e),
            }
        }
        Decision::DataWrite { app, path } => {
            let Some(open) = open else { return not_open() };
            match shell::data_write(&open.dir, &app, &path, &body) {
                Ok(()) => Answer::empty(204),
                Err(e) => file_error(e),
            }
        }
        Decision::DataRemove { app, path } => {
            let Some(open) = open else { return not_open() };
            match shell::data_remove(&open.dir, &app, &path) {
                Ok(()) => Answer::empty(204),
                Err(e) => file_error(e),
            }
        }
        // Handled before the body is read.
        Decision::Events => Answer::page(400, Page::BadRequest),
    }
}

/// A missing file and a refused one look the same (404); too large 413;
/// the disk failing 500.
fn file_error(e: FileError) -> Answer {
    match e {
        FileError::NotFound | FileError::Refused(_) => Answer::page(404, Page::NotFound),
        FileError::TooLarge => Answer::page(413, Page::TooLarge),
        FileError::Io(_) => Answer::new(500, "text/plain; charset=utf-8", "The disk refused."),
    }
}

/// `GET /api/events`: `hello`, then every broadcast frame and a ping
/// every 15 s, until a write fails, the listener is dropped as slow, or
/// the server stops.
fn events(shared: &Shared, request: Request, open: Option<Open>) {
    let (tx, rx) = mpsc::sync_channel::<String>(QUEUE);
    let id = shared.next_id.fetch_add(1, Ordering::SeqCst);
    {
        let mut listeners = lock(&shared.listeners);
        if listeners.len() >= LISTENERS_MAX || shared.stopping.load(Ordering::SeqCst) {
            drop(listeners);
            return Answer::json(503, &json!({"error": "busy"})).send(request);
        }
        listeners.push(Listener { id, tx });
    }
    let mut head =
        String::from("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nConnection: close\r\n");
    for (name, value) in route::headers() {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let hello = Event::Hello { project: open.as_ref().map(|o| o.project.clone()), script: open.map(|o| o.script) };
    let mut writer = request.into_writer();
    let mut send = |text: &str| writer.write_all(text.as_bytes()).and_then(|()| writer.flush());
    if send(&head).and_then(|()| send(&hello.frame())).is_ok() {
        loop {
            let sent = match rx.recv_timeout(PING_EVERY) {
                Ok(frame) => send(&frame),
                Err(RecvTimeoutError::Timeout) => send(PING),
                Err(RecvTimeoutError::Disconnected) => break,
            };
            if sent.is_err() {
                break;
            }
        }
    }
    shared.remove_listener(id);
}
