//! The app hosts apps (plan "apps", A7): the apps server
//! ([`scriptorium_apps::serve`]) on `127.0.0.1`, its [`Host`], and the
//! change events its pages listen to.
//!
//! - **Who serves:** the copy of Scriptorium that owns the agent link
//!   (`main.rs` starts the server unless the link answered `Owned`).
//! - **[`Host::open`]** is asked on every request and once a second while a
//!   page listens, from the server's threads: it reads the shared
//!   [`OpenScript`] the workspace keeps up to date, and never waits on the
//!   UI thread.
//! - **[`Host::call`]** checks the project against that snapshot, then puts
//!   the call on the agent link's queue ([`Job::app`]), where it runs on the
//!   UI thread like an agent's call, with `Caller::App` (the project is
//!   checked again there). At most [`APP_JOBS_MAX`] app calls wait at once;
//!   more are busy at once, and busy is never retried here (the page's
//!   library retries), so a busy script never holds the server's threads.
//! - **Events:** a workspace task every [`TICK_EVERY`], while a page
//!   listens, takes a [`Snapshot`] of the open view ([`Ticker`]) and
//!   broadcasts what [`events::diff`] finds, paced by [`events::Pacer`].
//!   The snapshot stays light on a long script: the reviews' statuses are
//!   worked out again only once the text has been still for a tick, and the
//!   writer's scene only when the line or the text changed.
//! - **Port taken** (or no token): no apps this run; [`Apps::Unavailable`]
//!   keeps the words (one notice; the Apps menu and `open_app` say them).
//! - **Stopped** on a clean quit (`main.rs`), and when dropped.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use futures::channel::mpsc::UnboundedSender;
use std::path::PathBuf;

use gpui::{App, Entity, EntityId};
use scriptorium_apps::events::{self, Event, ItemState, Pacer, ReviewState, Snapshot, Writer};
use scriptorium_apps::serve::{Call, CallAnswer, Host, Open, Running, Server};
use scriptorium_core::{Document, ParaId};
use scriptorium_fsio::link::proto;
use scriptorium_review::{Session, Status};

use crate::agent_link::{Answer, Job, OpenNow, OpenScript};
use crate::script_view::{ScriptView, file_name};

/// The most app calls waiting at once; one more is busy at once.
pub(crate) const APP_JOBS_MAX: usize = 8;
/// How often the events tick looks at the open view while a page listens.
pub(crate) const TICK_EVERY: Duration = Duration::from_millis(250);

/// Whether apps are served this run.
pub(crate) enum Apps {
    /// Not started (another copy serves, or not yet).
    Off,
    /// Serving, with the token pages are opened with.
    Serving { running: Arc<Running>, token: String },
    /// Not this run, in the writer's words ("Apps can't open: another
    /// program is using port 28417. …").
    Unavailable(String),
}

impl Apps {
    /// The server, while it serves.
    pub(crate) fn running(&self) -> Option<&Arc<Running>> {
        match self {
            Apps::Serving { running, .. } => Some(running),
            _ => None,
        }
    }

    /// Why apps don't run this time, when they can't.
    #[cfg_attr(not(test), allow(dead_code))] // The Apps menu and `open_app` (A8, A9).
    pub(crate) fn unavailable(&self) -> Option<&str> {
        match self {
            Apps::Unavailable(why) => Some(why),
            _ => None,
        }
    }

    /// The token apps' pages are opened with, while serving.
    #[cfg_attr(not(test), allow(dead_code))] // `open_app` (A8).
    pub(crate) fn token(&self) -> Option<&str> {
        match self {
            Apps::Serving { token, .. } => Some(token),
            _ => None,
        }
    }
}

/// Start the apps server on [`scriptorium_apps::serve::port`], answering
/// from `open` and running calls through `queue` (the agent link's).
/// Logged either way.
pub(crate) fn start(open: OpenScript, queue: UnboundedSender<Job>) -> Apps {
    let Some(token) = crate::settings::apps_token() else {
        return Apps::Unavailable(
            "Apps can't open: Scriptorium couldn't make their key. Restart it to try again.".into(),
        );
    };
    start_on(scriptorium_apps::serve::port(), token, open, queue)
}

/// [`start`] on `port` with `token`.
fn start_on(port: u16, token: String, open: OpenScript, queue: UnboundedSender<Job>) -> Apps {
    match Server::start(port, token.clone(), Arc::new(AppHost::new(open, queue))) {
        Ok(running) => {
            crate::log::info(&format!("serving apps at http://127.0.0.1:{port}/"));
            Apps::Serving { running: Arc::new(running), token }
        }
        Err(taken) => {
            crate::log::warn(&format!("not serving apps: {} ({})", taken, taken.error));
            Apps::Unavailable(taken.to_string())
        }
    }
}

/// The server's [`Host`]: the open script from the shared snapshot, and
/// calls through the agent link's queue.
pub(crate) struct AppHost {
    open: OpenScript,
    queue: UnboundedSender<Job>,
    /// App calls sent and not yet answered.
    waiting: AtomicUsize,
    next_id: AtomicU64,
}

impl AppHost {
    pub(crate) fn new(open: OpenScript, queue: UnboundedSender<Job>) -> Self {
        AppHost { open, queue, waiting: AtomicUsize::new(0), next_id: AtomicU64::new(1) }
    }

    /// App calls waiting now (tests).
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }
}

/// Takes one place among the waiting app calls; gives it back when dropped.
struct Place<'a>(&'a AtomicUsize);

impl Drop for Place<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The server's view of the open script, from the workspace's snapshot.
pub(crate) fn open_of(now: &OpenNow) -> Open {
    Open { project: now.key.clone(), script: file_name(&now.path), dir: now.dir.clone() }
}

impl Host for AppHost {
    fn open(&self) -> Option<Open> {
        self.open.now().as_ref().map(open_of)
    }

    fn call(&self, app: &str, project: &str, call: Call) -> CallAnswer {
        if self.open.now().is_none_or(|o| o.key != project) {
            return CallAnswer::NotOpen;
        }
        if self.waiting.fetch_add(1, Ordering::SeqCst) >= APP_JOBS_MAX {
            self.waiting.fetch_sub(1, Ordering::SeqCst);
            return CallAnswer::Busy;
        }
        let _place = Place(&self.waiting);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let call = proto::Call { name: call.name, arguments: call.arguments };
        let (job, rx) = Job::app(app, project, id, call);
        if self.queue.unbounded_send(job).is_err() {
            return CallAnswer::Busy; // The window is going away.
        }
        match rx.recv() {
            Ok(answer) => call_answer(answer),
            Err(_) => CallAnswer::Busy,
        }
    }
}

/// The UI task's answer as the server gives it: busy of any kind is busy
/// (nothing ran; the page's library tries again).
fn call_answer(answer: Answer) -> CallAnswer {
    match answer {
        Answer::Ran { done, busy: false } => CallAnswer::Done { text: done.text, is_error: done.is_error },
        Answer::Ran { busy: true, .. } | Answer::Busy(_) => CallAnswer::Busy,
        Answer::Files | Answer::NotOpen => CallAnswer::NotOpen,
    }
}

// ---- events -----------------------------------------------------------------

/// The events tick's state: the last snapshot, the pacer, and what is kept
/// between ticks so a long script isn't read through on each.
#[derive(Default)]
pub(crate) struct Ticker {
    prev: Option<Snapshot>,
    pacer: Pacer,
    /// The log length at the last tick: the text is still when it is the same.
    last_log: Option<usize>,
    /// The reviews' states, and what they were worked out for.
    reviews: Option<(ReviewsKey, Vec<ReviewState>)>,
    /// The writer's place, and the (log length, line) it was found for.
    writer: Option<((usize, ParaId), Writer)>,
    /// The script and view the caches above are for: another (or none)
    /// clears them, so a script just opened never shows the last one's.
    cached_for: Option<(PathBuf, EntityId)>,
}

/// What the reviews' states depend on: the text (decisions are in its log)
/// and each review with its number of suggestions.
type ReviewsKey = (usize, Vec<(String, usize)>);

impl Ticker {
    /// One tick at `now` (ms from any fixed start): the events to send.
    /// `listening`: whether any page listens; with none it does nothing
    /// and starts over when one comes back (its `hello` says where things
    /// stand).
    pub(crate) fn step(
        &mut self,
        now: u64,
        listening: bool,
        view: Option<&Entity<ScriptView>>,
        open: Option<OpenNow>,
        cx: &App,
    ) -> Vec<Event> {
        if !listening {
            *self = Ticker::default();
            return vec![];
        }
        let snap = self.snapshot(view, open, cx);
        let found = match &self.prev {
            Some(prev) => events::diff(prev, &snap),
            None => vec![],
        };
        self.prev = Some(snap);
        self.pacer.step(now, found)
    }

    fn snapshot(&mut self, view: Option<&Entity<ScriptView>>, open: Option<OpenNow>, cx: &App) -> Snapshot {
        let (Some(entity), Some(open)) = (view, open) else {
            self.forget();
            return Snapshot::default();
        };
        let now_for = (open.path.clone(), entity.entity_id());
        if self.cached_for.as_ref() != Some(&now_for) {
            self.forget();
            self.cached_for = Some(now_for);
        }
        let v = entity.read(cx);
        let doc = v.live_doc(cx);
        let log_len = doc.log().len();
        let still = self.last_log == Some(log_len);
        self.last_log = Some(log_len);
        let session = v.session();
        let key: ReviewsKey = (log_len, session.reviews.iter().map(|r| (r.id.clone(), r.items.len())).collect());
        let stale = self.reviews.as_ref().is_none_or(|(k, _)| *k != key);
        // While the text changes (typing), the last states stand: worked
        // out again once it is still (decisions are in the log, so they
        // show a tick later at most).
        if stale && (still || self.reviews.is_none()) {
            self.reviews = Some((key, review_states(session, doc)));
        }
        let at = v.writer_para(cx);
        if self.writer.as_ref().is_none_or(|(k, _)| *k != (log_len, at)) {
            let scene = scene_heading(doc, at);
            self.writer = Some(((log_len, at), Writer { scene, paragraph: at.0 }));
        }
        Snapshot {
            project: Some(open.key),
            script: Some(file_name(&open.path)),
            log_len,
            reviews: self.reviews.as_ref().map(|(_, r)| r.clone()).unwrap_or_default(),
            writer: self.writer.as_ref().map(|(_, w)| w.clone()),
        }
    }
}

impl Ticker {
    /// Clear what was kept about the view.
    fn forget(&mut self) {
        self.last_log = None;
        self.reviews = None;
        self.writer = None;
        self.cached_for = None;
    }
}

/// Each attached review's suggestions: number (as the tools number them)
/// and status in words.
fn review_states(session: &Session, doc: &Document) -> Vec<ReviewState> {
    session
        .reviews
        .iter()
        .enumerate()
        .map(|(r, review)| {
            let numbers = scriptorium_tools::item_numbers(doc, &review.items);
            let items = (0..review.items.len())
                .map(|i| ItemState { index: i, number: numbers[i], status: status_word(session.status(doc, r, i)) })
                .collect();
            ReviewState { name: review.name.clone(), items }
        })
        .collect()
}

fn status_word(s: Status) -> String {
    match s {
        Status::Pending => "pending".into(),
        Status::Accepted(k) => format!("accepted {}", k + 1),
        Status::Rejected => "kept".into(),
        Status::Rewritten => "rewritten".into(),
        Status::Removed => "removed".into(),
    }
}

/// The heading of the scene holding `para` (`""` before the first, or for a
/// line not in `doc`): found by looking back from the line, not by reading
/// the whole script.
fn scene_heading(doc: &Document, para: ParaId) -> String {
    let Some(ix) = doc.index_of(para) else { return String::new() };
    doc.paragraphs()[..=ix]
        .iter()
        .rev()
        .find(|p| scriptorium_core::outline::is_heading(p))
        .map(|p| p.text().trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    //! Apps' calls run as the agent link's tests run agents' (the UI task's
    //! step, [`agent_link::handle`], run by hand), through the [`AppHost`]
    //! the server would call, from test threads. No HTTP here: the server
    //! has its own tests.

    use std::path::{Path, PathBuf};
    use std::thread::JoinHandle;
    use std::time::Instant;

    use futures::channel::mpsc::UnboundedReceiver;
    use gpui::{Entity, TestAppContext, WindowHandle};
    use scriptorium_core::{Pos, Selection};
    use scriptorium_fsio::link::proto::Hello;
    use scriptorium_fsio::link::{Answer as Got, Client};
    use serde_json::{Value, json};

    use super::*;
    use crate::agent_link::{self, start_at};
    use crate::workspace::Workspace;

    fn script(heading: &str) -> String {
        let more: String =
            (0..10).map(|i| format!("    <Paragraph Type=\"Action\"><Text>Line {i}.</Text></Paragraph>\n")).collect();
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Scene Heading"><Text>{heading}</Text></Paragraph>
    <Paragraph Type="Action"><Text>Laura smiles.</Text></Paragraph>
    <Paragraph Type="Character"><Text>CESAR</Text></Paragraph>
    <Paragraph Type="Dialogue"><Text>I am go to the town tomorrow.</Text></Paragraph>
    <Paragraph Type="Scene Heading"><Text>INT. KITCHEN - DAY</Text></Paragraph>
{more}  </Content>
</FinalDraft>
"#
        )
    }

    struct Folder {
        dir: PathBuf,
        sock_dir: PathBuf,
    }

    impl Folder {
        fn new(name: &str) -> Self {
            let id = format!("{name}-{}", std::process::id());
            let dir = std::env::temp_dir().join(format!("scriptorium-apps-host-{id}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("film.fdx"), script("EXT. FARM HOUSE - NIGHT")).unwrap();
            let sock_dir = PathBuf::from(format!("/tmp/sa-{id}"));
            let _ = std::fs::remove_dir_all(&sock_dir);
            Folder { dir: dir.canonicalize().unwrap(), sock_dir }
        }

        fn script(&self) -> PathBuf {
            self.dir.join("film.fdx")
        }

        fn key(&self) -> String {
            scriptorium_apps::paths::project_key(&self.dir)
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

    /// A workspace on `film.fdx` serving agents (the link) and apps (the
    /// host over the same queue), with the calls run by hand.
    struct Served {
        window: WindowHandle<Workspace>,
        jobs: UnboundedReceiver<Job>,
        host: Arc<AppHost>,
        socket: PathBuf,
    }

    fn served(cx: &mut TestAppContext, f: &Folder) -> Served {
        cx.update(|cx| {
            crate::agent_panel::init(cx);
            cx.bind_keys(crate::bindings());
        });
        let socket = f.sock_dir.join("agent.sock");
        let link = start_at(&socket).unwrap();
        let script = Some(f.script());
        let window = cx.add_window(|window, cx| Workspace::new(script, vec![], window, cx));
        let (jobs, host) = window
            .update(cx, |w, _, cx| {
                let jobs = w.serve_agents_by_hand(link, cx);
                let host = Arc::new(AppHost::new(w.shared_open(), w.queue().unwrap()));
                (jobs, host)
            })
            .unwrap();
        Served { window, jobs, host, socket }
    }

    impl Served {
        fn view(&self, cx: &mut TestAppContext) -> Entity<ScriptView> {
            self.window.read_with(cx, |w, _| w.script_view().unwrap()).unwrap()
        }

        fn step(&mut self, cx: &mut TestAppContext) {
            let ws = self.window.entity(cx).unwrap().downgrade();
            while let Ok(job) = self.jobs.try_recv() {
                let mut acx = cx.to_async();
                assert!(agent_link::handle(job, &ws, &mut acx));
            }
            cx.run_until_parked();
        }

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

        /// App `app`'s call for folder `project`, from a server thread.
        fn app_call(&self, app: &str, project: &str, name: &str, args: Value) -> JoinHandle<CallAnswer> {
            let (host, app, project) = (self.host.clone(), app.to_string(), project.to_string());
            let call = Call { name: name.into(), arguments: args };
            std::thread::spawn(move || host.call(&app, &project, call))
        }

        fn app(&mut self, cx: &mut TestAppContext, f: &Folder, name: &str, args: Value) -> CallAnswer {
            let h = self.app_call("scene-list", &f.key(), name, args);
            self.wait(cx, h)
        }

        /// An agent's call over the link.
        fn agent(&mut self, cx: &mut TestAppContext, name: &str, args: Value) -> String {
            let socket = self.socket.clone();
            let call = proto::Call { name: name.into(), arguments: args };
            let h = std::thread::spawn(move || {
                let mut c = Client::connect(&socket, &Hello::this_build(), Duration::from_secs(5)).unwrap();
                c.call(&call)
            });
            match self.wait(cx, h) {
                Ok(Got::Done { done, .. }) => {
                    assert!(!done.is_error, "{}", done.text);
                    done.text
                }
                other => panic!("{other:?}"),
            }
        }

        /// One tick of the events, at `now` ms, with a page listening.
        fn tick(&self, cx: &mut TestAppContext, ticker: &mut Ticker, now: u64) -> Vec<Event> {
            self.window
                .read_with(cx, |w, cx| {
                    let view = w.script_view();
                    ticker.step(now, true, view.as_ref(), w.shared_open().now(), cx)
                })
                .unwrap()
        }
    }

    fn done(answer: CallAnswer) -> String {
        match answer {
            CallAnswer::Done { text, is_error: false } => text,
            other => panic!("{other:?}"),
        }
    }

    fn items(cx: &mut TestAppContext, v: &Entity<ScriptView>, file: &str) -> usize {
        v.read_with(cx, |v, _| {
            let s = v.session();
            (0..s.reviews.len()).find(|&r| v.review_file(r).ends_with(file)).map_or(0, |r| s.reviews[r].items.len())
        })
    }

    fn start_rewrite(s: &Served, cx: &mut TestAppContext, v: &Entity<ScriptView>) {
        let id = v.read_with(cx, |v, _| v.session().reviews[0].id.clone());
        s.window.update(cx, |_, window, cx| v.update(cx, |v, cx| v.rewrite_for_test(&id, 1, window, cx))).unwrap();
        assert!(v.read_with(cx, |v, _| v.is_rewriting()));
    }

    #[gpui::test]
    fn an_apps_calls_read_the_live_view_and_suggest_into_its_review(cx: &mut TestAppContext) {
        let f = Folder::new("live");
        let mut s = served(cx, &f);
        let v = s.view(cx);
        // The server's view of what is open.
        let open = s.host.open().unwrap();
        assert_eq!(
            (open.project.as_str(), open.script.as_str(), open.dir.as_path()),
            (f.key().as_str(), "film.fdx", f.dir.as_path())
        );

        let text = done(s.app(cx, &f, "read_scene", json!({"scene": 1})));
        assert!(text.contains("I am go to the town tomorrow."), "{text}");
        let info = done(s.app(cx, &f, "script_info", json!({})));
        assert!(info.starts_with(&exec_folder_line(&f.dir)), "{info}");
        // Not offered to an app: refused by the tool set, not run.
        for (name, args) in [
            ("open_app", json!({"name": "scene-list"})),
            ("decide_suggestion", json!({"n": 1, "decision": "keep"})),
            ("guide", json!({"topic": "app"})),
        ] {
            match s.app(cx, &f, name, args) {
                CallAnswer::Done { is_error: true, .. } => {}
                other => panic!("{name}: {other:?}"),
            }
        }
        let text = done(s.app(cx, &f, "where_is_writer", json!({})));
        assert!(text.contains("Scene: 1, EXT. FARM HOUSE - NIGHT"), "{text}");

        let args = json!({"suggestions": [{"paragraph_id": 3, "options": ["I am going to town tomorrow."]}]});
        let out = done(s.app(cx, &f, "suggest_changes", args));
        assert!(out.contains("\"added\""), "{out}");
        assert_eq!(items(cx, &v, "scene-list (app).review.txt"), 1);
        let review = f.read("scene-list (app).review.txt");
        assert!(review.starts_with("SCRIPT REVIEW") && review.contains("I am going to town tomorrow."), "{review}");
        assert!(f.read("film.scriptorium.json").contains("scene-list (app).review.txt"), "saved before the answer");
        let said = v.read_with(cx, |v, _| v.notice_text().to_string());
        let name = v.read_with(cx, |v, _| v.session().reviews[0].name.clone());
        assert_eq!(said, format!("The scene-list app added 1 suggestion to {name}."));
        // The second goes into the same review.
        done(s.app(cx, &f, "suggest_change", json!({"paragraph_id": 1, "options": ["Laura grins."]})));
        assert_eq!(items(cx, &v, "scene-list (app).review.txt"), 2);
        assert_eq!(v.read_with(cx, |v, _| v.session().reviews.len()), 1);
    }

    fn exec_folder_line(dir: &Path) -> String {
        scriptorium_chat::exec::folder_line(dir)
    }

    #[gpui::test]
    fn while_the_writer_rewrites_only_suggestions_into_an_attached_review_go_ahead(cx: &mut TestAppContext) {
        let f = Folder::new("rewrite");
        let mut s = served(cx, &f);
        let v = s.view(cx);
        s.agent(
            cx,
            "suggest_change",
            json!({"paragraph_id": 1, "options": ["Laura grins."], "review_file": "notes.review.txt"}),
        );
        start_rewrite(&s, cx, &v);
        // Into a review already attached: applies (writes only its file).
        let args = json!({"paragraph_id": 5, "options": ["Uno."], "review_file": "notes.review.txt"});
        done(s.app(cx, &f, "suggest_change", args));
        assert_eq!(items(cx, &v, "notes.review.txt"), 2);
        // Its own review would be created and attached: busy, at once.
        let started = Instant::now();
        let got = s.app(cx, &f, "suggest_change", json!({"paragraph_id": 6, "options": ["Dos."]}));
        assert_eq!(got, CallAnswer::Busy);
        assert!(started.elapsed() < scriptorium_chat::exec::BUSY_FOR / 2, "busy is answered at once");
        assert!(!f.dir.join("scene-list (app).review.txt").exists());
        // A new review named: busy too.
        let args = json!({"paragraph_id": 6, "options": ["Dos."], "review_file": "other.review.txt"});
        assert_eq!(s.app(cx, &f, "suggest_change", args), CallAnswer::Busy);
        // Reads go on.
        done(s.app(cx, &f, "list_reviews", json!({})));
        // Once the rewrite ends, the app's own review is made.
        v.update(cx, |v, cx| assert_eq!(v.save_now(cx), None));
        done(s.app(cx, &f, "suggest_change", json!({"paragraph_id": 6, "options": ["Dos."]})));
        assert_eq!(items(cx, &v, "scene-list (app).review.txt"), 1);
    }

    #[gpui::test]
    fn a_call_for_another_folder_is_not_open(cx: &mut TestAppContext) {
        let f = Folder::new("other");
        let mut s = served(cx, &f);
        // Refused by the host from its snapshot, before the queue.
        assert_eq!(
            s.host.call("scene-list", "0123456789abcdef", Call { name: "script_info".into(), arguments: json!({}) }),
            CallAnswer::NotOpen
        );
        // Queued for this folder, then another opened before it ran: the
        // UI thread checks again.
        let g = Folder::new("other-2");
        let h = s.app_call("scene-list", &f.key(), "script_info", json!({}));
        while s.host.waiting() == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
        s.window.update(cx, |w, window, cx| w.reopen_for_test(g.script(), window, cx)).unwrap();
        assert_eq!(s.wait(cx, h), CallAnswer::NotOpen);
        assert_eq!(s.host.open().unwrap().project, g.key());
        done(s.app_call_wait(cx, &g, "script_info"));
    }

    impl Served {
        fn app_call_wait(&mut self, cx: &mut TestAppContext, f: &Folder, name: &str) -> CallAnswer {
            let h = self.app_call("scene-list", &f.key(), name, json!({}));
            self.wait(cx, h)
        }
    }

    #[gpui::test]
    fn the_ninth_waiting_app_call_is_busy_at_once(cx: &mut TestAppContext) {
        let f = Folder::new("nine");
        let mut s = served(cx, &f);
        let waiting: Vec<_> =
            (0..APP_JOBS_MAX).map(|_| s.app_call("scene-list", &f.key(), "script_info", json!({}))).collect();
        let until = Instant::now() + Duration::from_secs(10);
        while s.host.waiting() < APP_JOBS_MAX {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(1));
        }
        let ninth = s.host.call("scene-list", &f.key(), Call { name: "script_info".into(), arguments: json!({}) });
        assert_eq!(ninth, CallAnswer::Busy);
        for h in waiting {
            done(s.wait(cx, h));
        }
        assert_eq!(s.host.waiting(), 0);
        done(s.app(cx, &f, "script_info", json!({})));
    }

    #[gpui::test]
    fn an_agents_and_an_apps_calls_interleave_in_order(cx: &mut TestAppContext) {
        let f = Folder::new("both");
        let mut s = served(cx, &f);
        let v = s.view(cx);
        // An agent on the link, and an app, each with four suggestions
        // sent before any is run.
        let socket = s.socket.clone();
        let agent = std::thread::spawn(move || {
            let mut c = Client::connect(&socket, &Hello::this_build(), Duration::from_secs(5)).unwrap();
            (4..8)
                .map(|p| {
                    let args = json!({"paragraph_id": p, "options": [format!("Agent {p}.")], "review_file": "agent.review.txt"});
                    c.call(&proto::Call { name: "suggest_change".into(), arguments: args })
                })
                .collect::<Vec<_>>()
        });
        let (host, key) = (s.host.clone(), f.key());
        let app = std::thread::spawn(move || {
            (8..12)
                .map(|p| {
                    let args = json!({"paragraph_id": p, "options": [format!("App {p}.")]});
                    host.call("scene-list", &key, Call { name: "suggest_change".into(), arguments: args })
                })
                .collect::<Vec<_>>()
        });
        std::thread::sleep(Duration::from_millis(50));
        for got in s.wait(cx, agent) {
            assert!(matches!(got, Ok(Got::Done { ref done, .. }) if !done.is_error), "{got:?}");
        }
        for got in s.wait(cx, app) {
            done(got);
        }
        assert_eq!(items(cx, &v, "agent.review.txt"), 4);
        assert_eq!(items(cx, &v, "scene-list (app).review.txt"), 4);
        let app = f.read("scene-list (app).review.txt");
        let at: Vec<usize> = (8..12).map(|p| app.find(&format!("App {p}.")).unwrap()).collect();
        assert!(at.is_sorted(), "{app}");
    }

    #[gpui::test]
    fn the_tick_finds_decisions_moves_edits_and_another_script(cx: &mut TestAppContext) {
        let f = Folder::new("tick");
        let mut s = served(cx, &f);
        let v = s.view(cx);
        let mut t = Ticker::default();
        let now = std::cell::Cell::new(0);
        let tick = |s: &Served, cx: &mut TestAppContext, t: &mut Ticker, ms: u64| {
            now.set(now.get() + ms);
            s.tick(cx, t, now.get())
        };
        assert_eq!(tick(&s, cx, &mut t, 0), vec![], "the first tick only looks");
        s.agent(
            cx,
            "suggest_change",
            json!({"paragraph_id": 1, "options": ["Laura grins."], "review_file": "notes.review.txt"}),
        );
        // A new suggestion: `script`, once the text has been still 300 ms.
        let mut got = vec![];
        for _ in 0..4 {
            got.extend(tick(&s, cx, &mut t, 250));
        }
        assert_eq!(got, vec![Event::Script]);

        // An agent's decision: `decided` (and `script`, a moment later).
        s.agent(cx, "decide_suggestion", json!({"n": 1, "decision": "accept"}));
        let mut got = vec![];
        for _ in 0..8 {
            got.extend(tick(&s, cx, &mut t, 250));
        }
        let review = v.read_with(cx, |v, _| v.session().reviews[0].name.clone());
        assert!(got.contains(&Event::Decided { review, numbers: vec![1] }), "{got:?}");
        assert_eq!(got.iter().filter(|e| **e == Event::Script).count(), 1, "{got:?}");

        // The writer moves into the second scene: `writer`.
        v.update(cx, |v, cx| v.set_selection_for_test(Selection::cursor(Pos::new(ParaId(6), 0)), cx));
        let got = tick(&s, cx, &mut t, 250);
        assert_eq!(got, vec![Event::Writer(Writer { scene: "INT. KITCHEN - DAY".into(), paragraph: 6 })]);
        assert_eq!(tick(&s, cx, &mut t, 250), vec![], "nothing new");

        // Ten edits in a second: one `script`, after they stop.
        let mut got = vec![];
        for i in 0..10 {
            s.agent(cx, "edit_paragraph", json!({"paragraph_id": 7, "text": format!("Edit {i}.")}));
            got.extend(tick(&s, cx, &mut t, 100));
        }
        for _ in 0..8 {
            got.extend(tick(&s, cx, &mut t, 250));
        }
        assert_eq!(got.iter().filter(|e| **e == Event::Script).count(), 1, "{got:?}");

        // Another script opens: `open`.
        let g = Folder::new("tick-2");
        s.window.update(cx, |w, window, cx| w.reopen_for_test(g.script(), window, cx)).unwrap();
        let got = tick(&s, cx, &mut t, 250);
        assert_eq!(got, vec![Event::Open { project: Some(g.key()), script: Some("film.fdx".into()) }]);

        // No page listening: nothing, and it starts over.
        let none =
            s.window.read_with(cx, |w, cx| t.step(now.get() + 250, false, w.script_view().as_ref(), None, cx)).unwrap();
        assert_eq!(none, vec![]);
        assert_eq!(tick(&s, cx, &mut t, 500), vec![], "a fresh start only looks");
    }

    /// A script just opened never shows the last one's scene or reviews,
    /// even where its log length and line are the same.
    #[gpui::test]
    fn another_script_starts_from_nothing_kept(cx: &mut TestAppContext) {
        let f = Folder::new("fresh");
        let mut s = served(cx, &f);
        s.agent(
            cx,
            "suggest_change",
            json!({"paragraph_id": 1, "options": ["Grins."], "review_file": "notes.review.txt"}),
        );
        let mut t = Ticker::default();
        s.tick(cx, &mut t, 0);
        let first = t.prev.clone().unwrap();
        assert_eq!(first.writer.as_ref().map(|w| w.scene.as_str()), Some("EXT. FARM HOUSE - NIGHT"));
        assert_eq!(first.reviews.len(), 1);
        // Another folder's script, fresh too (same log length, same line).
        let g = Folder::new("fresh-2");
        std::fs::write(g.script(), script("EXT. BARN - DAY")).unwrap();
        s.window.update(cx, |w, window, cx| w.reopen_for_test(g.script(), window, cx)).unwrap();
        let got = s.tick(cx, &mut t, 250);
        assert!(matches!(got.as_slice(), [Event::Open { .. }]), "{got:?}");
        let now = t.prev.clone().unwrap();
        assert_eq!(now.log_len, first.log_len);
        assert_eq!(now.writer.as_ref().map(|w| (w.scene.as_str(), w.paragraph)), Some(("EXT. BARN - DAY", 0)));
        assert!(now.reviews.is_empty(), "{:?}", now.reviews);
    }

    #[test]
    fn a_port_in_use_means_no_apps_this_run_in_plain_words() {
        let held = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        let apps = start_on(port, "t".repeat(64), OpenScript::default(), tx.clone());
        let want = format!("Apps can't open: another program is using port {port}. Quit it and restart Scriptorium.");
        assert_eq!(apps.unavailable(), Some(want.as_str()));
        assert!(apps.running().is_none() && apps.token().is_none());
        // Free again: served, and stopping frees the port.
        drop(held);
        let apps = start_on(port, "t".repeat(64), OpenScript::default(), tx);
        let running = apps.running().unwrap().clone();
        assert_eq!((running.port(), apps.token().map(str::len)), (port, Some(64)));
        running.stop();
        assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
    }

    #[test]
    fn busy_of_any_kind_is_busy_for_a_page() {
        use proto::Done;
        assert_eq!(call_answer(Answer::Busy("x")), CallAnswer::Busy);
        let d = Done { text: "t".into(), is_error: true };
        assert_eq!(call_answer(Answer::Ran { done: d.clone(), busy: true }), CallAnswer::Busy);
        assert_eq!(
            call_answer(Answer::Ran { done: d, busy: false }),
            CallAnswer::Done { text: "t".into(), is_error: true }
        );
        assert_eq!(call_answer(Answer::NotOpen), CallAnswer::NotOpen);
    }

    #[test]
    fn the_scene_is_found_looking_back() {
        use scriptorium_core::{ElementKind, Paragraph, Script};
        let p = |i: u64, k: ElementKind, t: &str| Paragraph::plain(ParaId(i), k, t);
        let paragraphs = vec![
            p(0, ElementKind::Action, "Before."),
            p(1, ElementKind::SceneHeading, "EXT. FARM - NIGHT "),
            p(2, ElementKind::Action, "Laura smiles."),
            p(3, ElementKind::Action, "Rain."),
            // Typed as Action, read as a heading (as `read_scene` has it).
            p(4, ElementKind::Action, "INT. KITCHEN - DAY"),
            p(5, ElementKind::Action, "Tea."),
        ];
        let doc = Document::new(Script { paragraphs, ..Script::default() });
        assert_eq!(scene_heading(&doc, ParaId(0)), "");
        assert_eq!(scene_heading(&doc, ParaId(3)), "EXT. FARM - NIGHT");
        assert_eq!(scene_heading(&doc, ParaId(5)), "INT. KITCHEN - DAY");
        assert_eq!(scene_heading(&doc, ParaId(99)), "");
    }
}
