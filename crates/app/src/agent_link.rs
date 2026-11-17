//! The app as the agents' server (plan "agent connect", C1): outside agents'
//! tool calls, sent by `scriptorium mcp` over the link
//! ([`scriptorium_fsio::link`]), run here on the live open script.
//!
//! Threads, as the chat driver has them (`agent_panel/driver.rs`):
//!
//! - **An accept thread** takes connections ([`Server::accept`]), and **one
//!   thread per connection** reads its requests. They never touch GPUI.
//! - Each call goes over a channel to **one GPUI task** owned by the
//!   `Workspace` ([`pump`]). For each call it asks the workspace for its
//!   current script view (never holding one: a write in place or another
//!   script replaces it) and runs the call inside **one** `ScriptView`
//!   update: `run_call(LiveScript, Caller::Agent, call)`. It never waits:
//!   the session lock is taken with zero wait, and busy goes back as
//!   [`Answer::Busy`].
//! - **Busy retries happen on the connection thread**: every
//!   [`exec::BUSY_RETRY`] for up to [`exec::BUSY_FOR`], then the call's
//!   result says [`exec::BUSY`] ([`WRITER_REWRITING`] when the writer is
//!   rewriting a line).
//! - While the writer rewrites a line nothing can be saved: a writing call
//!   is busy before `taken`, but suggestions into a review already attached
//!   (they write only its file).
//! - The lock lease lives only inside the one update. `taken` is written
//!   from that update (through the connection's [`Replier`]), just before
//!   `run_call`, and names the script the call runs on.
//! - A call that changed the session file's contents is saved
//!   ([`ScriptView::save_for_agent`]) before its answer leaves the update,
//!   so an agent is told about a change only once it is on disk; one that
//!   changed nothing there saves nothing.
//!
//! Routes ([`Route::Files`]: the connector runs the call on the files):
//! a call naming a script other than the open one (canonical paths), or
//! with nothing open. Every tool an outside agent has runs here, the host
//! operations included (C2, through the live store). `guide` and
//! `where_is_writer` never go to the files: the app answers them whatever
//! is open.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::{App, AsyncApp, Context, Entity, WeakEntity};
use scriptorium_agent::ToolCall;
use scriptorium_chat::exec::{self, Caller, ReviewError, ReviewStore, Script};
use scriptorium_fsio::link::proto::{Done, Hello, Reply, Request, Route};
use scriptorium_fsio::link::{BindError, Replier, Server};
use scriptorium_tools::context;

use crate::agent_panel::live::LiveScript;
use crate::disk;
use crate::script_view::{LiveError, ScriptView, WRITER_REWRITING};
use crate::workspace::Workspace;
use scriptorium_core::Document;
use scriptorium_review::Session;

/// Tools the app answers whatever script is open (or none): they never go
/// to the files while the link is up.
const APP_ONLY: [&str; 2] = ["guide", context::NAME];

/// The script open in the app, for the hello (informational only).
#[derive(Clone, Default)]
pub struct OpenScript(Arc<Mutex<Option<String>>>);

impl OpenScript {
    pub fn set(&self, script: Option<&Path>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = script.map(|p| p.display().to_string());
    }

    fn get(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// The served link, handed to the workspace ([`Workspace::serve_agents`]).
pub struct Link {
    pub(crate) jobs: UnboundedReceiver<Job>,
    pub(crate) open: OpenScript,
    server: Arc<Server>,
}

impl Link {
    /// Remove the socket (on a clean quit): the accept thread keeps the
    /// server alive, so it is never dropped.
    pub fn closer(&self) -> impl Fn() + 'static {
        let server = self.server.clone();
        move || server.close()
    }
}

/// Serve the link at its usual place ([`Server::bind`]). [`BindError::Owned`]:
/// another copy of the app serves it.
pub fn start() -> Result<Link, BindError> {
    serve(Server::bind()?)
}

/// Serve the link at `socket` (tests).
#[cfg(test)]
pub(crate) fn start_at(socket: &Path) -> Result<Link, BindError> {
    serve(Server::bind_at(socket)?)
}

fn serve(server: Server) -> Result<Link, BindError> {
    let server = Arc::new(server);
    let (tx, jobs) = unbounded();
    let open = OpenScript::default();
    let (s, o) = (server.clone(), open.clone());
    std::thread::Builder::new()
        .name("agent-link".into())
        .spawn(move || accept_loop(&s, &tx, &o))
        .map_err(|e| BindError::Io(format!("agent link thread: {e}")))?;
    crate::log::info(&format!("serving AI agents at {}", server.path().display()));
    Ok(Link { jobs, open, server })
}

fn accept_loop(server: &Server, tx: &UnboundedSender<Job>, open: &OpenScript) {
    loop {
        match server.accept() {
            Ok(conn) => {
                let (tx, open) = (tx.clone(), open.clone());
                let spawned = std::thread::Builder::new()
                    .name("agent-link-conn".into())
                    .spawn(move || connection(conn, &tx, &open));
                if let Err(e) = spawned {
                    crate::log::warn(&format!("agent link: no thread for a connection: {e}"));
                }
            }
            Err(e) => {
                crate::log::warn(&format!("agent link: accept: {e}"));
                // Don't spin on a lasting error.
                std::thread::sleep(exec::BUSY_RETRY);
            }
        }
        if tx.is_closed() {
            return; // The app's task is gone.
        }
    }
}

/// One call for the UI task, and where its answer goes.
pub(crate) struct Job {
    request: Request,
    replier: Replier,
    answer: mpsc::Sender<Answer>,
}

/// The UI task's answer to one [`Job`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Answer {
    /// Not taken: the connector runs it on the files.
    Files,
    /// Not taken: the script's files are busy (another program holds the
    /// lock, or the writer is rewriting a line, or the script is being
    /// written in place). Ask again; the text is the call's result if it is
    /// still busy when time is up.
    Busy(&'static str),
    /// Taken and run. `busy`: it found the files busy after all (nothing
    /// ran); ask again.
    Ran { done: Done, busy: bool },
}

/// One connector's connection: requests one at a time, each sent to the UI
/// task (again while busy), its reply written back.
fn connection(mut conn: scriptorium_fsio::link::Conn, tx: &UnboundedSender<Job>, open: &OpenScript) {
    let ours = Hello { script: open.get(), ..Hello::this_build() };
    match conn.handshake(&ours) {
        Ok(Some(_)) => {}
        Ok(None) => return, // Another version: the connector works on the files.
        Err(e) => {
            crate::log::warn(&format!("agent link: hello: {e}"));
            return;
        }
    }
    loop {
        let request = match conn.next_request() {
            Ok(Some(r)) => r,
            Ok(None) => return,
            Err(e) => {
                crate::log::warn(&format!("agent link: {e}"));
                return;
            }
        };
        let Some(reply) = run_request(&request, &conn.replier(), tx) else { return };
        if conn.reply(request.id, &reply).is_err() {
            return;
        }
    }
}

/// Send `request` to the UI task until it isn't busy (or [`exec::BUSY_FOR`]
/// is up). `None`: the app is going away; hang up.
fn run_request(request: &Request, replier: &Replier, tx: &UnboundedSender<Job>) -> Option<Reply> {
    let started = Instant::now();
    // A busy answer after `taken` (nothing ran): if the app goes before the
    // next try, say busy rather than hang up, which would read as "maybe
    // applied".
    let mut busy_after_taken: Option<Done> = None;
    loop {
        let (answer, rx) = mpsc::channel();
        let job = Job { request: request.clone(), replier: replier.clone(), answer };
        let answer = match tx.unbounded_send(job).ok().and_then(|()| rx.recv().ok()) {
            Some(a) => a,
            None => return busy_after_taken.map(Reply::Done),
        };
        let busy_done = match answer {
            Answer::Files => return Some(Reply::Route(Route::Files)),
            Answer::Ran { done, busy: false } => return Some(Reply::Done(done)),
            Answer::Ran { done, busy: true } => {
                busy_after_taken = Some(done.clone());
                done
            }
            Answer::Busy(text) => Done { text: text.into(), is_error: true },
        };
        if started.elapsed() >= exec::BUSY_FOR {
            return Some(Reply::Done(busy_done));
        }
        std::thread::sleep(exec::BUSY_RETRY);
    }
}

/// The UI task: runs each call as it comes, on the workspace's script view
/// as it is then. Ends when the workspace or the link goes.
pub(crate) async fn pump(mut jobs: UnboundedReceiver<Job>, workspace: WeakEntity<Workspace>, cx: &mut AsyncApp) {
    while let Some(job) = jobs.next().await {
        if !handle(job, &workspace, cx) {
            return;
        }
    }
}

/// Run one job on the workspace's script view as it is now, and answer it.
/// False: the workspace is gone (the job goes to the files).
pub(crate) fn handle(job: Job, workspace: &WeakEntity<Workspace>, cx: &mut AsyncApp) -> bool {
    let Ok(view) = workspace.read_with(cx, |w, _| w.script_view()) else {
        let _ = job.answer.send(Answer::Files);
        return false;
    };
    let answer = match view {
        Some(view) => run_on_view(&job, &view, cx),
        None => run_without_script(&job),
    };
    let _ = job.answer.send(answer);
    true
}

fn tool_call(request: &Request) -> ToolCall {
    ToolCall {
        id: format!("link-{}", request.id),
        name: request.call.name.clone(),
        input: request.call.arguments.clone(),
        input_error: None,
        raw_input: None,
        extra: None,
    }
}

fn done(d: &exec::CallDone) -> Done {
    Done { text: d.result.content.clone(), is_error: d.result.is_error }
}

/// Write `taken` for the job, naming `script`. False: the connector hung up
/// (don't run the call).
fn taken(job: &Job, script: Option<&Path>) -> bool {
    let script = script.map(|p| p.display().to_string());
    job.replier.reply(job.request.id, &Reply::Taken { script }).is_ok()
}

/// Nothing open: only the tools that need no script run here.
fn run_without_script(job: &Job) -> Answer {
    if !APP_ONLY.contains(&job.request.call.name.as_str()) {
        return Answer::Files;
    }
    if !taken(job, None) {
        return Answer::Files;
    }
    let call = tool_call(&job.request);
    if call.name == context::NAME {
        return Answer::Ran { done: where_is_writer(&call, || NOTHING_OPEN.to_string()), busy: false };
    }
    let ran = exec::run_call(&mut NoScript, Caller::Agent, &call);
    Answer::Ran { done: done(&ran), busy: false }
}

/// `where_is_writer` with no script open in the app.
pub(crate) const NOTHING_OPEN: &str = "Scriptorium is open with no script in it; nothing is selected. Ask the writer to open the script, or for its \
     path.";

/// Answer `where_is_writer` (checked against the offer as any call is):
/// `place` says where the writer is. The app answers it itself: on the
/// files there is no writer to place.
fn where_is_writer(call: &ToolCall, place: impl FnOnce() -> String) -> Done {
    let checked = scriptorium_tools::offered(scriptorium_tools::Offer::Agent, &call.name)
        .and_then(|spec| scriptorium_tools::check_args(&spec, &call.input));
    match checked {
        Ok(()) => Done { text: place(), is_error: false },
        Err(e) => Done { text: e.to_string(), is_error: true },
    }
}

/// No script open, for the tools that need none.
struct NoScript;

impl Script for NoScript {
    fn open(&mut self, _: bool) -> Result<Box<dyn ReviewStore + '_>, ReviewError> {
        Err(ReviewError::Failed("No script is open in Scriptorium.".into()))
    }
}

/// Whether the agent's `script` argument names `open` (canonical): a
/// relative path or one that doesn't resolve never does.
fn names(script: &str, open: &Path) -> bool {
    let named = PathBuf::from(exec::expand_home(script));
    if !named.is_absolute() {
        return false;
    }
    match (named.canonicalize(), open.canonicalize()) {
        (Ok(a), Ok(b)) if a == b => true,
        // The same file by another path (a hard link, a case-insensitive
        // folder spelled otherwise).
        _ => same_file(&named, open),
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => (a.dev(), a.ino()) == (b.dev(), b.ino()),
        _ => false,
    }
}

/// Whether `run_call` opens the script for writing for tool `name` (and so
/// takes the session lock and reloads): the pump checks those *before*
/// `taken`, so a busy script is answered busy before the call is taken.
/// `run_call` opens for writing exactly the tools [`scriptorium_tools::writes`]
/// names (the host operations included); the test
/// `opens_for_writing_is_what_run_call_does` holds it to that.
pub(crate) fn opens_for_writing(name: &str) -> bool {
    scriptorium_tools::writes(name)
}

/// The route for `job` with `open` the open script: the files, or here.
fn routed_here(job: &Job, open: &Path) -> bool {
    let call = &job.request.call;
    if APP_ONLY.contains(&call.name.as_str()) {
        return true;
    }
    match call.arguments.get("script") {
        None | Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::String(s)) => names(s, open),
        // Not a path: the connector's own check answers it.
        Some(_) => false,
    }
}

/// Run `job` inside one update of `view`.
fn run_on_view(job: &Job, view: &Entity<ScriptView>, cx: &mut AsyncApp) -> Answer {
    view.update(cx, |v, cx| {
        let open = v.script_path().to_path_buf();
        if !routed_here(job, &open) {
            return Answer::Files;
        }
        if v.is_retired() {
            return Answer::Busy(exec::BUSY);
        }
        let call = tool_call(&job.request);
        if call.name == context::NAME {
            // Read-only, and needs no lock: what the view shows now.
            if !taken(job, Some(&open)) {
                return Answer::Files;
            }
            return Answer::Ran { done: where_is_writer(&call, || context::text(&v.where_is_writer(cx))), busy: false };
        }
        let writes = opens_for_writing(&call.name);
        // While the writer rewrites a line nothing can be saved, so a call
        // that changes the session is busy (an agent is never told of a
        // change that isn't on disk); suggestions into an attached review
        // change only its file, and go ahead.
        let rewriting = v.is_rewriting();
        if writes && rewriting && !runs_while_rewriting(&call, v) {
            return Answer::Busy(WRITER_REWRITING);
        }
        let busy = if rewriting { WRITER_REWRITING } else { exec::BUSY };
        // Held through the call and its save, inside this update only.
        let mut lease = None;
        let mut before = None;
        if writes {
            let Some(l) = v.try_lock_session() else { return Answer::Busy(exec::BUSY) };
            lease = Some(l);
            // What other programs wrote first, so the news below is the
            // agent's alone.
            // (A session file that can't be used: the call says so when it
            // runs.)
            if let Err(LiveError::Busy) = v.reload_or_busy(cx) {
                return Answer::Busy(busy);
            }
            before = Some((v.session().clone(), v.live_doc(cx).clone()));
        }
        if !taken(job, Some(&open)) {
            return Answer::Files;
        }
        let mut live = LiveScript::new(v, cx);
        let ran = exec::run_call(&mut live, Caller::Agent, &call);
        let notes = live.take_notes();
        let mut done = done(&ran);
        if ran.busy && v.is_rewriting() {
            done.text = WRITER_REWRITING.into();
        }
        if !ran.busy
            && let Some((session, doc)) = &before
        {
            // Saved only if the call changed what the session file holds,
            // so a call that did nothing never writes one.
            if session_changed(session, doc, v, cx) && !v.save_for_agent(cx) {
                crate::log::warn("agent link: the session wasn't saved before the reply; it is saved shortly");
            }
            let summary = disk::summarize(session, doc, v.session(), v.live_doc(cx));
            let news = agent_news(&call.name, &summary, v.session()).into_iter().chain(notes);
            let news: Vec<String> = news.collect();
            if !news.is_empty() {
                v.say_agent_news(news.join("  "), cx);
            }
        }
        drop(lease);
        Answer::Ran { done, busy: ran.busy }
    })
}

/// While the writer rewrites a line, whether writing call `call` still runs:
/// only suggestions into a review already attached, which write that
/// review's file and leave the session as it is. With no `review_file` the
/// only attached review is meant (or the tool refuses, changing nothing).
fn runs_while_rewriting(call: &ToolCall, v: &ScriptView) -> bool {
    if !scriptorium_tools::may_append(&call.name) {
        return false;
    }
    match call.input.get("review_file") {
        None | Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::String(f)) => exec::attached_review_file(v.script_dir(), v.session(), f).is_some(),
        // Not a path: the tool refuses it.
        Some(_) => true,
    }
}

/// Whether the call changed what the session file holds since `session`
/// and `doc` (the steps, the reviews attached). Suggestions added to an
/// attached review are in its file, not here.
fn session_changed(session: &Session, doc: &Document, v: &ScriptView, cx: &App) -> bool {
    let attached = |s: &Session| s.reviews.iter().map(|r| (r.path.clone(), r.id.clone())).collect::<Vec<_>>();
    v.live_doc(cx).log().len() != doc.log().len() || attached(v.session()) != attached(session)
}

/// What an outside agent's call changed, for the status bar ("An AI agent
/// added 6 suggestions to dialogue-pass."), from the same summary a reload
/// makes. `None`: nothing to say.
pub(crate) fn agent_news(tool: &str, s: &disk::Summary, now: &scriptorium_review::Session) -> Option<String> {
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let numbers = |list: &[(String, usize)]| {
        let one = list.iter().all(|(n, _)| *n == list[0].0);
        let nums: Vec<String> =
            list.iter().map(|(name, k)| if one { format!("#{k}") } else { format!("{name} #{k}") }).collect();
        let of = if one { format!(" in {}", list[0].0) } else { String::new() };
        format!("{}{of}", nums.join(", "))
    };
    let mut parts = vec![];
    for name in &s.attached {
        let n = now.reviews.iter().find(|r| r.name == *name).map_or(0, |r| r.items.len());
        // An attached review brings what it holds; a suggesting tool's new
        // review holds what the agent just added.
        parts.push(match n {
            0 => format!("attached {name}"),
            n if tool == "attach_reviews" => format!("attached {name} ({n} suggestion{})", plural(n)),
            n => format!("added {n} suggestion{} to {name}", plural(n)),
        });
    }
    for (name, n) in &s.new_suggestions {
        parts.push(format!("added {n} suggestion{} to {name}", plural(*n)));
    }
    if !s.decided.is_empty() {
        parts.push(format!("decided {}", numbers(&s.decided)));
    }
    if !s.undecided.is_empty() {
        parts.push(format!("undid the decision on {}", numbers(&s.undecided)));
    }
    if s.edited > 0 {
        parts.push(format!("edited {} line{}", s.edited, plural(s.edited)));
    }
    if s.removed > 0 {
        parts.push(format!("removed {} line{}", s.removed, plural(s.removed)));
    }
    parts.extend(s.detached.iter().map(|n| format!("detached {n}")));
    (!parts.is_empty()).then(|| format!("An AI agent {}.", parts.join(", ")))
}

/// Start serving outside agents for `workspace`'s window: the pump task.
pub(crate) fn spawn_pump(jobs: UnboundedReceiver<Job>, cx: &mut Context<Workspace>) -> gpui::Task<()> {
    cx.spawn(async move |ws, cx| pump(jobs, ws, cx).await)
}

#[cfg(test)]
mod tests {
    //! Over real sockets, with the connector played by test threads. GPUI's
    //! test executor can't be woken from another thread, so the tests run
    //! the UI task's step ([`handle`]) themselves while they wait.

    use std::thread::JoinHandle;
    use std::time::Duration;

    use gpui::{TestAppContext, WindowHandle};
    use scriptorium_chat::exec::FileScript;
    use scriptorium_fsio::link::proto::Call;
    use scriptorium_fsio::link::{Answer as Got, Client, Gone};
    use serde_json::{Value, json};

    use super::*;

    /// Four lines (ids 0-3), then ten more actions (ids 4-13).
    fn script() -> String {
        let more: String =
            (0..10).map(|i| format!("    <Paragraph Type=\"Action\"><Text>Line {i}.</Text></Paragraph>\n")).collect();
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Scene Heading"><Text>EXT. FARM HOUSE - NIGHT</Text></Paragraph>
    <Paragraph Type="Action"><Text>Laura smiles.</Text></Paragraph>
    <Paragraph Type="Character"><Text>CESAR</Text></Paragraph>
    <Paragraph Type="Dialogue"><Text>I am go to the town tomorrow.</Text></Paragraph>
{more}  </Content>
</FinalDraft>
"#
        )
    }

    /// A folder with `film.fdx`, and a short one for the socket (socket
    /// paths are short).
    struct Folder {
        dir: PathBuf,
        sock_dir: PathBuf,
    }

    impl Folder {
        fn new(name: &str) -> Self {
            let id = format!("{name}-{}", std::process::id());
            let dir = std::env::temp_dir().join(format!("scriptorium-link-{id}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("film.fdx"), script()).unwrap();
            let sock_dir = PathBuf::from(format!("/tmp/sl-{id}"));
            let _ = std::fs::remove_dir_all(&sock_dir);
            Folder { dir: dir.canonicalize().unwrap(), sock_dir }
        }

        fn script(&self) -> PathBuf {
            self.dir.join("film.fdx")
        }

        fn socket(&self) -> PathBuf {
            self.sock_dir.join("agent.sock")
        }

        fn read(&self, name: &str) -> String {
            std::fs::read_to_string(self.dir.join(name)).unwrap_or_default()
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
            let _ = std::fs::remove_dir_all(&self.sock_dir);
        }
    }

    /// A workspace window (on `script`, or empty) serving the link at its
    /// folder's socket, with the calls run by hand.
    struct Served {
        window: WindowHandle<Workspace>,
        jobs: UnboundedReceiver<Job>,
        socket: PathBuf,
    }

    fn app(cx: &mut TestAppContext, f: &Folder, script: Option<PathBuf>) -> Served {
        cx.update(|cx| {
            crate::agent_panel::init(cx);
            cx.bind_keys(crate::bindings());
        });
        let link = start_at(&f.socket()).unwrap();
        let window = cx.add_window(|window, cx| Workspace::new(script, vec![], window, cx));
        let jobs = window.update(cx, |w, _, cx| w.serve_agents_by_hand(link, cx)).unwrap();
        Served { window, jobs, socket: f.socket() }
    }

    impl Served {
        fn view(&self, cx: &mut TestAppContext) -> Entity<ScriptView> {
            self.window.read_with(cx, |w, _| w.script_view().unwrap()).unwrap()
        }

        /// Run the calls that came in, as the UI task does.
        fn step(&mut self, cx: &mut TestAppContext) {
            let ws = self.window.entity(cx).unwrap().downgrade();
            while let Ok(job) = self.jobs.try_recv() {
                let mut acx = cx.to_async();
                assert!(handle(job, &ws, &mut acx));
            }
            cx.run_until_parked();
        }

        /// Serve until `h`, a thread talking to the app, ends.
        fn wait<T>(&mut self, cx: &mut TestAppContext, h: JoinHandle<T>) -> T {
            let until = Instant::now() + Duration::from_secs(30);
            while !h.is_finished() {
                assert!(Instant::now() < until, "the call never ended");
                self.step(cx);
                std::thread::sleep(Duration::from_millis(2));
            }
            self.step(cx);
            h.join().unwrap()
        }

        /// Serve for `d`.
        fn serve_for(&mut self, cx: &mut TestAppContext, d: Duration) {
            let until = Instant::now() + d;
            while Instant::now() < until {
                self.step(cx);
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        fn one(&mut self, cx: &mut TestAppContext, c: Call) -> Result<Got, Gone> {
            let h = agent(&self.socket, vec![c]);
            self.wait(cx, h).pop().unwrap()
        }
    }

    fn connect(socket: &Path) -> Client {
        Client::connect(socket, &Hello::this_build(), Duration::from_secs(5)).unwrap()
    }

    fn call(name: &str, arguments: Value) -> Call {
        Call { name: name.into(), arguments }
    }

    /// `calls` from a connector on its own thread, one after another.
    fn agent(socket: &Path, calls: Vec<Call>) -> JoinHandle<Vec<Result<Got, Gone>>> {
        let socket = socket.to_path_buf();
        std::thread::spawn(move || {
            let mut client = connect(&socket);
            calls.iter().map(|c| client.call(c)).collect()
        })
    }

    /// The call's text, run in the app on the open script.
    fn ran(got: Result<Got, Gone>, f: &Folder) -> String {
        match got {
            Ok(Got::Done { done, script }) => {
                assert!(!done.is_error, "{}", done.text);
                assert_eq!(script.as_deref(), Some(f.script().to_str().unwrap()));
                done.text
            }
            other => panic!("not run in the app: {other:?}"),
        }
    }

    fn suggest(para: u64, option: &str, review: &str) -> Call {
        call("suggest_change", json!({"paragraph_id": para, "options": [option], "review_file": review}))
    }

    /// How many suggestions the view holds in review file `file`.
    fn items(cx: &mut TestAppContext, v: &Entity<ScriptView>, file: &str) -> usize {
        v.read_with(cx, |v, _| {
            let s = v.session();
            (0..s.reviews.len()).find(|&r| v.review_file(r).ends_with(file)).map_or(0, |r| s.reviews[r].items.len())
        })
    }

    #[gpui::test]
    fn read_and_suggest_calls_reach_the_live_view(cx: &mut TestAppContext) {
        let f = Folder::new("live");
        let mut app = app(cx, &f, Some(f.script()));
        let v = app.view(cx);
        // The hello names the open script.
        let h = std::thread::spawn({
            let s = f.socket();
            move || connect(&s).server_hello().script.clone()
        });
        assert_eq!(app.wait(cx, h).as_deref(), Some(f.script().to_str().unwrap()));

        let text = ran(app.one(cx, call("read_script", json!({}))), &f);
        assert!(text.contains("I am go to the town tomorrow."), "{text}");
        // `script` named, through another spelling of the same path.
        let named = format!("{}/./film.fdx", f.dir.display());
        let text = ran(app.one(cx, call("read_script", json!({"script": named}))), &f);
        assert!(text.contains("Laura smiles."), "{text}");

        ran(app.one(cx, suggest(3, "I am going to town tomorrow.", "notes.review.txt")), &f);
        assert_eq!(items(cx, &v, "notes.review.txt"), 1);
        assert!(f.read("notes.review.txt").contains("I am going to town tomorrow."));
        // Saved before the reply: the session file lists the new review.
        assert!(f.read("film.scriptorium.json").contains("notes.review.txt"));
        let said = v.read_with(cx, |v, _| v.notice_text().to_string());
        assert!(said.starts_with("An AI agent added 1 suggestion to "), "{said}");
        // The view doesn't take its own write for another program's.
        cx.executor().advance_clock(Duration::from_secs(3));
        cx.run_until_parked();
        assert_eq!(items(cx, &v, "notes.review.txt"), 1);
        assert_eq!(v.read_with(cx, |v, _| v.notice_text().to_string()), said);
    }

    #[gpui::test]
    fn two_connections_calling_at_once_both_apply(cx: &mut TestAppContext) {
        let f = Folder::new("two");
        let mut app = app(cx, &f, Some(f.script()));
        let v = app.view(cx);
        let a = agent(&f.socket(), (4..8).map(|p| suggest(p, &format!("A {p}."), "a.review.txt")).collect());
        let b = agent(&f.socket(), (4..8).map(|p| suggest(p, &format!("B {p}."), "b.review.txt")).collect());
        // Both connected before either is served: their calls interleave.
        std::thread::sleep(Duration::from_millis(50));
        let (a, b) = (app.wait(cx, a), app.wait(cx, b));
        for got in a.into_iter().chain(b) {
            ran(got, &f);
        }
        assert_eq!(items(cx, &v, "a.review.txt"), 4);
        assert_eq!(items(cx, &v, "b.review.txt"), 4);
        // In each connector's order.
        let a = f.read("a.review.txt");
        let at: Vec<usize> = (4..8).map(|p| a.find(&format!("A {p}.")).unwrap()).collect();
        assert!(at.is_sorted(), "{a}");
    }

    #[gpui::test]
    fn a_busy_script_is_tried_again_then_applies(cx: &mut TestAppContext) {
        let f = Folder::new("busy");
        let mut app = app(cx, &f, Some(f.script()));
        let v = app.view(cx);
        // Another program holds the session lock for a while.
        let at = scriptorium_fsio::SessionLock::path_for(&f.script());
        let lock = scriptorium_fsio::SessionLock::acquire(&at, Duration::ZERO).unwrap();
        let h = agent(&f.socket(), vec![suggest(1, "Laura grins.", "notes.review.txt")]);
        app.serve_for(cx, exec::BUSY_RETRY * 3);
        assert!(!h.is_finished(), "busy is tried again, not answered");
        assert_eq!(items(cx, &v, "notes.review.txt"), 0);
        drop(lock);
        ran(app.wait(cx, h).pop().unwrap(), &f);
        assert_eq!(items(cx, &v, "notes.review.txt"), 1);
    }

    /// While the writer rewrites a line and another program changed the
    /// files, the app can't load them: busy until the rewrite ends.
    #[gpui::test]
    fn a_call_while_the_writer_rewrites_is_busy_then_applies(cx: &mut TestAppContext) {
        let f = Folder::new("rewrite");
        let mut app = app(cx, &f, Some(f.script()));
        let v = app.view(cx);
        ran(app.one(cx, suggest(1, "Laura grins.", "notes.review.txt")), &f);
        let id = v.read_with(cx, |v, _| v.session().reviews[0].id.clone());
        app.window.update(cx, |_, window, cx| v.update(cx, |v, cx| v.rewrite_for_test(&id, 1, window, cx))).unwrap();
        assert!(v.read_with(cx, |v, _| v.is_rewriting()));
        // Another program adds to the review meanwhile.
        let mut fs = FileScript::new(&f.script()).unwrap();
        let c = ToolCall {
            id: "f".into(),
            name: "suggest_change".into(),
            input: json!({"paragraph_id": 5, "options": ["Files."], "review_file": "notes.review.txt"}),
            input_error: None,
            raw_input: None,
            extra: None,
        };
        assert!(!exec::run_call(&mut fs, Caller::Agent, &c).result.is_error);
        let h = agent(&f.socket(), vec![suggest(6, "App.", "notes.review.txt")]);
        app.serve_for(cx, exec::BUSY_RETRY * 3);
        assert!(!h.is_finished(), "busy while the rewrite goes on");
        // The writer finishes (the app saves, loading the other's first).
        v.update(cx, |v, cx| assert_eq!(v.save_now(cx), None));
        ran(app.wait(cx, h).pop().unwrap(), &f);
        assert_eq!(items(cx, &v, "notes.review.txt"), 3);
        assert!(f.read("notes.review.txt").contains("Files.") && f.read("notes.review.txt").contains("App."));
    }

    /// Start the writer rewriting suggestion 1 of the first review.
    fn start_rewrite(app: &Served, cx: &mut TestAppContext, v: &Entity<ScriptView>) {
        let id = v.read_with(cx, |v, _| v.session().reviews[0].id.clone());
        app.window.update(cx, |_, window, cx| v.update(cx, |v, cx| v.rewrite_for_test(&id, 1, window, cx))).unwrap();
        assert!(v.read_with(cx, |v, _| v.is_rewriting()));
    }

    /// A translation goes on while the writer rewrites a line: suggestions
    /// into an attached review write only its file (the line being
    /// rewritten is refused), and the session file is left as it is.
    #[gpui::test]
    fn suggestions_into_an_attached_review_go_ahead_while_the_writer_rewrites(cx: &mut TestAppContext) {
        let f = Folder::new("translate");
        let mut app = app(cx, &f, Some(f.script()));
        let v = app.view(cx);
        ran(app.one(cx, suggest(1, "Laura grins.", "notes.review.txt")), &f);
        ran(app.one(cx, suggest(0, "EXT. GRANJA - NOCHE", "es.review.txt")), &f);
        let session = f.read("film.scriptorium.json");
        start_rewrite(&app, cx, &v);
        for batch in [2..8, 8..14] {
            let mut list: Vec<Value> =
                batch.map(|p| json!({"paragraph_id": p, "options": [format!("Línea {p}.")]})).collect();
            list.push(json!({"paragraph_id": 1, "options": ["Laura sonríe."]}));
            let args = json!({"suggestions": list, "review_file": "es.review.txt"});
            let out = ran(app.one(cx, call("suggest_changes", args)), &f);
            assert!(out.contains(exec::EDITING_LINE), "the line being rewritten is refused: {out}");
        }
        // Without review_file, two attached: refused at once, not busy.
        match app.one(cx, call("suggest_change", json!({"paragraph_id": 2, "options": ["CÉSAR"]}))) {
            Ok(Got::Done { done, .. }) => assert_eq!(done.text, exec::NO_REVIEW),
            other => panic!("{other:?}"),
        }
        assert_eq!(items(cx, &v, "es.review.txt"), 13);
        assert!(f.read("es.review.txt").contains("Línea 13."));
        assert_eq!(f.read("film.scriptorium.json"), session, "the session file is untouched");
        assert!(v.read_with(cx, |v, _| v.is_rewriting()));
    }

    /// While the writer rewrites a line, a call that would change the
    /// session waits for the rewrite, and says so when time is up.
    #[gpui::test]
    fn calls_that_change_the_session_wait_for_the_rewrite(cx: &mut TestAppContext) {
        let f = Folder::new("wait");
        let mut app = app(cx, &f, Some(f.script()));
        let v = app.view(cx);
        ran(app.one(cx, suggest(1, "Laura grins.", "notes.review.txt")), &f);
        start_rewrite(&app, cx, &v);
        let session = f.read("film.scriptorium.json");
        let calls = [
            suggest(5, "New.", "other.review.txt"),
            call("decide_suggestion", json!({"n": 1, "decision": "keep"})),
            call("edit_paragraph", json!({"paragraph_id": 0, "text": "INT. BARN - DAY"})),
            call("edit_paragraph", json!({"paragraph_id": 1, "text": "Hi."})),
            call("undo", json!({})),
            call("write_script", json!({})),
            call("attach_reviews", json!({"files": ["notes.review.txt"]})),
            call("detach_reviews", json!({"files": ["notes.review.txt"]})),
        ];
        // Each on its own connection, so they wait side by side.
        let hs: Vec<_> = calls.iter().map(|c| agent(&f.socket(), vec![c.clone()])).collect();
        for (c, h) in calls.iter().zip(hs) {
            match app.wait(cx, h).pop().unwrap() {
                Ok(Got::Done { done, .. }) => {
                    assert!(done.is_error, "{}: {}", c.name, done.text);
                    assert_eq!(done.text, WRITER_REWRITING, "{}", c.name);
                }
                other => panic!("{}: {other:?}", c.name),
            }
        }
        assert!(!f.dir.join("other.review.txt").exists());
        assert!(!f.dir.join("film (scriptorium).fdx").exists());
        assert_eq!(f.read("film.scriptorium.json"), session);
        assert_eq!(v.read_with(cx, |v, _| v.session().reviews.len()), 1);
        // Once the rewrite is finished, they run.
        v.update(cx, |v, cx| assert_eq!(v.save_now(cx), None));
        ran(app.one(cx, suggest(5, "New.", "other.review.txt")), &f);
        assert!(f.read("film.scriptorium.json").contains("other.review.txt"));
    }

    #[gpui::test]
    fn other_scripts_and_nothing_open_go_to_the_files(cx: &mut TestAppContext) {
        let f = Folder::new("route");
        let other = f.dir.join("other.fdx");
        std::fs::write(&other, script()).unwrap();
        let mut served = app(cx, &f, Some(f.script()));
        let files = |got: Result<Got, Gone>| assert_eq!(got, Ok(Got::Files));
        files(served.one(cx, call("read_script", json!({"script": other.to_str().unwrap()}))));
        files(served.one(cx, call("read_script", json!({"script": "film.fdx"}))));
        files(served.one(cx, call("read_script", json!({"script": "/no/such/film.fdx"}))));
        // The open script through a link, and by a hard link, is the open one.
        std::os::unix::fs::symlink(f.script(), f.dir.join("link.fdx")).unwrap();
        std::fs::hard_link(f.script(), f.dir.join("hard.fdx")).unwrap();
        for name in ["link.fdx", "hard.fdx"] {
            let at = f.dir.join(name);
            ran(served.one(cx, call("read_script", json!({"script": at.to_str().unwrap()}))), &f);
        }
        // `guide` is answered here.
        let got = served.one(cx, call("guide", json!({"topic": "review"})));
        assert!(matches!(got, Ok(Got::Done { ref done, .. }) if !done.is_error), "{got:?}");
        assert!(!f.dir.join("film.scriptorium.json").exists(), "nothing written");
        // The host operations run here too (C2), never on the files.
        for name in scriptorium_tools::AGENT_ONLY {
            let got = served.one(cx, call(name, json!({})));
            assert!(matches!(got, Ok(Got::Done { .. })), "{name}: {got:?}");
        }
        // They changed nothing in the session, so none wrote it.
        assert!(!f.dir.join("film.scriptorium.json").exists(), "a call that changed nothing saved");
        drop(served);

        // With nothing open.
        let g = Folder::new("route-empty");
        let mut served = app(cx, &g, None);
        files(served.one(cx, call("read_script", json!({}))));
        files(served.one(cx, suggest(1, "Laura grins.", "notes.review.txt")));
        match served.one(cx, call("guide", json!({"topic": "translate"}))) {
            Ok(Got::Done { done, script: None }) => assert!(!done.is_error && !done.text.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    /// `where_is_writer` is answered from the open view, whatever script a
    /// call names, and with nothing open; never sent to the files.
    #[gpui::test]
    fn where_is_writer_is_answered_from_the_view(cx: &mut TestAppContext) {
        use scriptorium_core::{ParaId, Pos, Selection};
        let f = Folder::new("where");
        let mut served = app(cx, &f, Some(f.script()));
        let v = served.view(cx);
        // The writer selects from "Laura smiles." into the dialogue.
        v.update(cx, |v, cx| {
            v.set_selection_for_test(Selection { anchor: Pos::new(ParaId(1), 6), head: Pos::new(ParaId(3), 4) }, cx)
        });
        let text = ran(served.one(cx, call("where_is_writer", json!({}))), &f);
        for want in [
            format!("Open in Scriptorium: {}", f.script().display()),
            "Showing: the script.".into(),
            "Selected: ¶1 to ¶3:\n  smiles.\n  CESAR\n  I am".into(),
            "Scene: 1, EXT. FARM HOUSE - NIGHT".into(),
            "Written: no changes".into(),
        ] {
            assert!(text.contains(&want), "{want}\n{text}");
        }
        // The agent panel's conversation is never part of it.
        assert!(!text.contains("agent panel") && !text.contains("conversation"), "{text}");
        // Arguments it doesn't take are refused, as for any tool.
        match served.one(cx, call("where_is_writer", json!({"script": "/elsewhere/other.fdx"}))) {
            Ok(Got::Done { done, .. }) => assert!(done.is_error && done.text.contains("`script`"), "{}", done.text),
            other => panic!("{other:?}"),
        }
        assert!(!f.dir.join("film.scriptorium.json").exists(), "nothing written");
        drop(served);

        let g = Folder::new("where-empty");
        let mut served = app(cx, &g, None);
        match served.one(cx, call("where_is_writer", json!({}))) {
            Ok(Got::Done { done, script: None }) => {
                assert_eq!((done.text.as_str(), done.is_error), (NOTHING_OPEN, false))
            }
            other => panic!("{other:?}"),
        }
    }

    /// "Saved before the reply":when an agent hears a decision, an edit or
    /// an undo was made, it is in the session file already (the app's
    /// autosave never ran: the test's clock doesn't move).
    #[gpui::test]
    fn a_decision_is_in_the_session_file_when_the_reply_is_sent(cx: &mut TestAppContext) {
        let f = Folder::new("saved");
        let mut served = app(cx, &f, Some(f.script()));
        ran(served.one(cx, suggest(1, "Laura grins.", "notes.review.txt")), &f);
        let socket = served.socket.clone();
        let session = f.dir.join("film.scriptorium.json");
        let h = std::thread::spawn(move || {
            let mut client = connect(&socket);
            let mut seen = vec![];
            for c in [
                call("decide_suggestion", json!({"n": 1, "decision": "accept"})),
                call("edit_paragraph", json!({"paragraph_id": 3, "text": "I go to town."})),
                call("undo", json!({})),
            ] {
                let got = client.call(&c);
                // Read the moment the reply is in.
                seen.push((got, std::fs::read_to_string(&session).unwrap_or_default()));
            }
            seen
        });
        let seen = served.wait(cx, h);
        let steps = |json: &str| json.matches("\"Edit\":").count();
        let (got, json) = &seen[0];
        assert!(ran(got.clone(), &f).contains("Laura grins."));
        assert!(json.contains("\"to\":\"accepted:0@1\""), "decision not saved: {json}");
        assert_eq!(steps(json), 1, "{json}");
        let (got, json) = &seen[1];
        ran(got.clone(), &f);
        assert_eq!(steps(json), 2, "edit not saved: {json}");
        // The undo is saved too: the session file reads as the decision
        // alone (opened afresh below).
        let (got, json) = &seen[2];
        assert_eq!(ran(got.clone(), &f), "¶3: I am go to the town tomorrow.");
        assert_ne!(*json, seen[1].1, "undo not saved");
        let opened = crate::script_view::Opened::load(&f.script(), &[]).unwrap();
        let w = cx.add_window(|window, cx| ScriptView::new(opened, window, cx));
        w.update(cx, |v, _, cx| {
            let doc = v.live_doc(cx);
            let text = |p| doc.paragraph(scriptorium_core::ParaId(p)).map(|p| p.text()).unwrap_or_default();
            assert_eq!(text(1), "Laura grins.");
            assert_eq!(text(3), "I am go to the town tomorrow.");
        })
        .unwrap();
    }

    /// During Write .fdx in place the view is retired until the window
    /// replaces it: busy, then the call reaches the new view.
    #[gpui::test]
    fn a_call_during_a_write_in_place_is_busy(cx: &mut TestAppContext) {
        let f = Folder::new("retired");
        let mut app = app(cx, &f, Some(f.script()));
        let old = app.view(cx);
        old.update(cx, |v, _| v.retire_for_test());
        let h = agent(&f.socket(), vec![call("read_script", json!({}))]);
        app.serve_for(cx, exec::BUSY_RETRY * 3);
        assert!(!h.is_finished(), "busy while the view is retired");
        app.window.update(cx, |w, window, cx| w.reopen_for_test(f.script(), window, cx)).unwrap();
        ran(app.wait(cx, h).pop().unwrap(), &f);
    }

    /// The pump decides "opens for writing" (lock, reload, busy before
    /// `taken`) as `run_call` does, for every tool an outside agent has.
    #[test]
    fn opens_for_writing_is_what_run_call_does() {
        struct Probe(Option<bool>);
        impl Script for Probe {
            fn open(&mut self, writes: bool) -> Result<Box<dyn ReviewStore + '_>, ReviewError> {
                self.0 = Some(writes);
                Err(ReviewError::Failed("probe".into()))
            }
            fn detach(&mut self, _: &[String]) -> Result<String, ReviewError> {
                self.0 = Some(true);
                Err(ReviewError::Failed("probe".into()))
            }
        }
        let tools = scriptorium_tools::catalogue(scriptorium_tools::Offer::Agent);
        let mut probed = 0;
        for spec in tools {
            let input = args_for(&spec.schema);
            let c = ToolCall {
                id: "p".into(),
                name: spec.name.to_string(),
                input,
                input_error: None,
                raw_input: None,
                extra: None,
            };
            let mut probe = Probe(None);
            let done = exec::run_call(&mut probe, Caller::Agent, &c);
            match probe.0 {
                Some(w) => {
                    assert_eq!(w, opens_for_writing(&c.name), "{}", c.name);
                    probed += 1;
                }
                None => assert!(
                    APP_ONLY.contains(&c.name.as_str()),
                    "{} never opened the script: {}",
                    c.name,
                    done.result.content
                ),
            }
        }
        assert!(probed >= 15, "{probed}");
    }

    /// Arguments that pass `check_args` for a tool's schema: each
    /// required property, made up from its schema.
    fn args_for(schema: &Value) -> Value {
        let props = schema["properties"].as_object().cloned().unwrap_or_default();
        let required: Vec<String> = schema["required"]
            .as_array()
            .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let mut out = serde_json::Map::new();
        for name in required {
            out.insert(name.clone(), value_for(&props.get(&name).cloned().unwrap_or(json!({}))));
        }
        Value::Object(out)
    }

    fn value_for(schema: &Value) -> Value {
        if let Some(first) = schema["enum"].as_array().and_then(|e| e.first()) {
            return first.clone();
        }
        let ty = match &schema["type"] {
            Value::Array(t) => t.iter().find(|t| *t != "null").cloned().unwrap_or(Value::Null),
            t => t.clone(),
        };
        match ty.as_str() {
            Some("integer" | "number") => json!(1),
            Some("boolean") => json!(false),
            Some("array") => json!([value_for(&schema["items"])]),
            Some("object") => args_for(schema),
            _ => json!("x"),
        }
    }

    #[gpui::test]
    fn a_second_app_does_not_serve(cx: &mut TestAppContext) {
        let f = Folder::new("second");
        let mut app = app(cx, &f, Some(f.script()));
        assert!(matches!(start_at(&f.socket()), Err(BindError::Owned)));
        // The first still serves.
        ran(app.one(cx, call("read_script", json!({}))), &f);
    }

    #[gpui::test]
    fn after_the_view_is_replaced_calls_reach_the_new_one(cx: &mut TestAppContext) {
        let f = Folder::new("reopen");
        let mut app = app(cx, &f, Some(f.script()));
        let old = app.view(cx);
        app.window.update(cx, |w, window, cx| w.reopen_for_test(f.script(), window, cx)).unwrap();
        let new = app.view(cx);
        assert_ne!(old.entity_id(), new.entity_id());
        ran(app.one(cx, suggest(1, "Laura grins.", "notes.review.txt")), &f);
        assert_eq!(items(cx, &new, "notes.review.txt"), 1);
        assert_eq!(items(cx, &old, "notes.review.txt"), 0);
    }

    /// An agent on the files (one the app doesn't serve: another copy's
    /// connector, or a call started before the app opened) and one through
    /// the app write the same script at once: both land, and the app loads
    /// the other's.
    #[gpui::test]
    fn the_files_route_and_the_app_at_once(cx: &mut TestAppContext) {
        let f = Folder::new("both");
        let mut app = app(cx, &f, Some(f.script()));
        let v = app.view(cx);
        let script = f.script();
        let on_files = std::thread::spawn(move || {
            let mut fs = FileScript::new(&script).unwrap().with_lock_wait(scriptorium_fsio::LOCK_WAIT);
            (4..9)
                .map(|p| {
                    let c = ToolCall {
                        id: format!("f{p}"),
                        name: "suggest_change".into(),
                        input: json!({"paragraph_id": p, "options": [format!("Files {p}.")], "review_file": "files.review.txt"}),
                        input_error: None,
                        raw_input: None,
                        extra: None,
                    };
                    exec::run_call(&mut fs, Caller::Agent, &c)
                })
                .collect::<Vec<_>>()
        });
        let through_app =
            agent(&f.socket(), (9..14).map(|p| suggest(p, &format!("App {p}."), "app.review.txt")).collect());
        for got in app.wait(cx, through_app) {
            ran(got, &f);
        }
        let on_files = app.wait(cx, on_files);
        assert!(on_files.iter().all(|d| !d.result.is_error), "{on_files:?}");
        // The app's watch loads what the files route wrote.
        cx.executor().advance_clock(Duration::from_secs(3));
        cx.run_until_parked();
        assert_eq!(items(cx, &v, "app.review.txt"), 5);
        assert_eq!(items(cx, &v, "files.review.txt"), 5);
        v.update(cx, |v, cx| assert_eq!(v.save_now(cx), None));
        let saved = f.read("film.scriptorium.json");
        assert!(saved.contains("app.review.txt") && saved.contains("files.review.txt"), "{saved}");
        assert_eq!(f.read("app.review.txt").matches("App ").count(), 5);
        assert_eq!(f.read("files.review.txt").matches("Files ").count(), 5);
    }

    #[test]
    fn news_reads_as_the_agents() {
        let now = scriptorium_review::Session::default();
        let s = disk::Summary {
            new_suggestions: vec![("dialogue-pass".into(), 6)],
            decided: vec![("dialogue-pass".into(), 2), ("dialogue-pass".into(), 3)],
            ..Default::default()
        };
        assert_eq!(
            agent_news("suggest_changes", &s, &now).as_deref(),
            Some("An AI agent added 6 suggestions to dialogue-pass, decided #2, #3 in dialogue-pass.")
        );
        assert_eq!(agent_news("undo", &disk::Summary::default(), &now), None);
        let s = disk::Summary { edited: 1, detached: vec!["notes".into()], ..Default::default() };
        assert_eq!(
            agent_news("suggest_changes", &s, &now).as_deref(),
            Some("An AI agent edited 1 line, detached notes.")
        );
    }
}
