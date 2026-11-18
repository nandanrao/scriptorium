//! The window: a toolbar (with the File dropdown on Linux), the open script
//! (a `ScriptView`) or the empty state, notices, and dropping files.
//!
//! There's one window, holding at most one script. File › Open saves the
//! current script's session and replaces it; File › Add review attaches review
//! files to it. Menu and toolbar actions are handled here at the root, so they
//! work whatever has focus (macOS menu items fire with nothing focused).

use std::path::PathBuf;

use gpui::{
    App, ClipboardItem, Context, Div, Entity, ExternalPaths, FocusHandle, Focusable, FontWeight, KeyDownEvent,
    MouseButton, PathPromptOptions, SharedString, Stateful, Subscription, Task, Window, actions, deferred, div,
    prelude::*, px,
};

use futures::channel::mpsc::UnboundedSender;

use crate::agent_link::{self, Job, Link, OpenScript};
use crate::agent_panel::accounts::{AccountsSheet, Dismissed};
use crate::agent_panel::{OpenAccounts, ToggleAgentPanel};
use crate::apps_host::{self, Apps};
use crate::connect::{self, ConnectAgent, ConnectSheet, InstallCli};
use crate::menus::{self, Item};
use crate::new_version::{MarkInSync, MarkNotInSync, NewVersion, NewVersionSheet, SheetEvent, Swap};
use crate::script_view::{
    ApplyToFdx, NextView, Opened, PrevView, RedoAny, Reopen, SaveAndQuit, ScriptView, ShowReview1, ShowReview2,
    ShowReview3, ShowScript, UndoAny, file_name, has_extension,
};
use crate::theme::*;

actions!(scriptorium, [Open, AddReview, About]);

/// Apps › <an app>: open the app `name` (its folder under the open
/// script's `apps/`) in the writer's browser.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = scriptorium, no_json)]
pub struct OpenApp {
    pub name: String,
}

/// Test hook: when set, file pickers don't open; they return these paths
/// (separated by `:`) instead. For driving the app headless, where the
/// desktop portal can't show a dialog.
const PICK_ENV: &str = "SCRIPTORIUM_TEST_PICK";

pub struct Workspace {
    view: Option<Entity<ScriptView>>,
    focus: FocusHandle,
    /// The Linux File dropdown, and what had focus before it opened.
    menu: Option<Option<FocusHandle>>,
    menu_focus: FocusHandle,
    /// Shown in the status bar when no script is open (otherwise the script
    /// view's status bar shows notices).
    notice: SharedString,
    /// The AI accounts sheet while it's open, and what had focus before.
    accounts: Option<(Entity<AccountsSheet>, Option<FocusHandle>, Subscription)>,
    /// File › New version…'s sheet while it's open, and what had focus before.
    new_version: Option<(Entity<NewVersionSheet>, Option<FocusHandle>, Subscription)>,
    /// Install command-line tool… is running; a second click is ignored.
    installing_cli: bool,
    /// Scriptorium › Connect an AI agent…'s sheet while it's open, and what
    /// had focus before.
    connect: Option<(Entity<ConnectSheet>, Option<FocusHandle>, Subscription)>,
    /// A fix the launch check offers (Claude Desktop's setting), shown in a
    /// bar under the toolbar until taken or dismissed; true while it runs.
    fix_offer: Option<(connect::Offer, bool)>,
    _observe: Option<Subscription>,
    /// The view's [`Reopen`]: Write .fdx wrote the version in place.
    _reopen: Option<Subscription>,
    /// Serving outside agents (`agent_link`): the script open, for the
    /// link's hello, and the task running their calls.
    link: Option<(OpenScript, Task<()>)>,
    /// The script open, for the apps server (read from its threads).
    open: OpenScript,
    /// The queue outside agents' and apps' calls run through, one at a
    /// time ([`agent_link::pump`]): the link's, or (with no link) one of
    /// the apps' own, with its task.
    queue: Option<UnboundedSender<Job>>,
    _own_pump: Option<Task<()>>,
    /// Serving apps (plan "apps"), and the events tick while serving.
    apps: Apps,
    _apps_tick: Option<Task<()>>,
    /// The Apps menu as last scanned ([`Self::refresh_apps_menu`]).
    apps_menu: menus::AppsMenu,
    /// Rescans the Apps menu when the window gains focus.
    _activation: Option<Subscription>,
    /// This window, to tell whether it is the active one (whose Apps menu
    /// the macOS menu bar shows).
    window: gpui::AnyWindowHandle,
}

/// The Apps menu last given to the macOS menu bar (one bar for every
/// window): a window that becomes active sets it again when its own
/// differs. Absent: `main.rs`'s first, [`menus::AppsMenu::default`].
struct MenuBar(menus::AppsMenu);

impl gpui::Global for MenuBar {}

impl Workspace {
    pub fn new(script: Option<PathBuf>, reviews: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            view: None,
            focus: cx.focus_handle(),
            menu: None,
            menu_focus: cx.focus_handle(),
            notice: "".into(),
            accounts: None,
            new_version: None,
            installing_cli: false,
            connect: None,
            fix_offer: None,
            _observe: None,
            _reopen: None,
            link: None,
            open: OpenScript::default(),
            queue: None,
            _own_pump: None,
            apps: Apps::Off,
            _apps_tick: None,
            apps_menu: menus::AppsMenu::default(),
            _activation: None,
            window: window.window_handle(),
        };
        // An agent may have built an app while the writer was elsewhere.
        this._activation = Some(cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.refresh_apps_menu(cx);
                this.sync_menu_bar(cx);
            }
        }));
        // Closing the window saves, like Quit.
        let ws = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            ws.update(cx, |w, cx| w.with_view(cx, |v, cx| _ = v.save_now(cx))).ok();
            true
        });
        match script {
            Some(p) => this.open_script(p, reviews, window, cx),
            None => window.focus(&this.focus, cx),
        }
        this.check_agents(cx);
        this
    }

    /// The script view open now (outside agents' calls look it up per call).
    pub(crate) fn script_view(&self) -> Option<Entity<ScriptView>> {
        self.view.clone()
    }

    /// Run outside agents' calls (from `link`) on the script open here, as
    /// it is at each call.
    pub fn serve_agents(&mut self, link: Link, cx: &mut Context<Self>) {
        let Link { jobs, queue, open, .. } = link;
        self.link = Some((open, agent_link::spawn_pump(jobs, cx)));
        self.queue = Some(queue);
        self.note_open(cx);
    }

    /// [`Self::serve_agents`] with no task: the test runs the calls
    /// ([`agent_link::handle`]), as GPUI's test executor can't be woken from
    /// the link's threads.
    #[cfg(test)]
    pub(crate) fn serve_agents_by_hand(
        &mut self,
        link: Link,
        cx: &mut Context<Self>,
    ) -> futures::channel::mpsc::UnboundedReceiver<agent_link::Job> {
        let Link { jobs, queue, open, .. } = link;
        self.link = Some((open, Task::ready(())));
        self.queue = Some(queue);
        self.note_open(cx);
        jobs
    }

    /// Tell the link (for its hello) and the apps server which script is
    /// open.
    fn note_open(&self, cx: &App) {
        let view = self.view.as_ref().map(|v| v.read(cx));
        let now = view.map(|v| (v.script_path(), v.script_dir()));
        self.open.set(now);
        if let Some((open, _)) = &self.link {
            open.set(now);
        }
    }

    /// The script open, as the apps server reads it.
    #[cfg(test)]
    pub(crate) fn shared_open(&self) -> OpenScript {
        self.open.clone()
    }

    /// The calls' queue, if one runs.
    #[cfg(test)]
    pub(crate) fn queue(&self) -> Option<UnboundedSender<Job>> {
        self.queue.clone()
    }

    /// Serve apps from this window ([`apps_host::start`]): calls through
    /// the agents' queue (one of their own when the link isn't served),
    /// events from a tick while a page listens. Can't: said once, and kept
    /// for the Apps menu and `open_app`.
    pub fn serve_apps(&mut self, cx: &mut Context<Self>) {
        let queue = match &self.queue {
            Some(q) => q.clone(),
            None => {
                let (tx, jobs) = futures::channel::mpsc::unbounded();
                self._own_pump = Some(agent_link::spawn_pump(jobs, cx));
                self.queue = Some(tx.clone());
                tx
            }
        };
        self.apps = apps_host::start(self.open.clone(), queue);
        match &self.apps {
            Apps::Serving { running, .. } => self._apps_tick = Some(Self::apps_tick(running.clone(), cx)),
            Apps::Unavailable(why) => self.set_notice(why.clone(), cx),
            Apps::Off => {}
        }
        self.refresh_apps_menu(cx);
    }

    /// The Apps menu now: the open folder's apps (one `read_dir` and their
    /// manifests), why apps can't run, or no script. Called when a script
    /// opens, the window gains focus and the Linux dropdown opens, so it
    /// needs no server thread; on macOS the menu bar is set again when it
    /// changed.
    fn refresh_apps_menu(&mut self, cx: &mut Context<Self>) {
        let now = match (&self.apps, self.open.now()) {
            (Apps::Unavailable(why), _) => menus::AppsMenu::Unavailable(why.clone()),
            (_, None) => menus::AppsMenu::NoScript,
            (Apps::Off, _) => menus::AppsMenu::Unavailable(crate::browser::APPS_OFF.into()),
            (_, Some(open)) => menus::AppsMenu::listed(&scriptorium_apps::shell::scan(&open.dir)),
        };
        if now != self.apps_menu {
            self.apps_menu = now;
            if cx.active_window() == Some(self.window) {
                self.sync_menu_bar(cx);
            }
            cx.notify();
        }
    }

    /// macOS: the menu bar shows this window's Apps menu, set again only
    /// when what was last set (by any window) differs.
    fn sync_menu_bar(&self, cx: &mut Context<Self>) {
        if !cfg!(target_os = "macos") {
            return;
        }
        let last = cx.try_global::<MenuBar>().map_or(menus::AppsMenu::default(), |m| m.0.clone());
        if last != self.apps_menu {
            cx.set_menus(menus::app_menus(&self.apps_menu));
            cx.set_global(MenuBar(self.apps_menu.clone()));
        }
    }

    /// The Apps menu as last scanned (tests).
    #[cfg(test)]
    pub(crate) fn apps_menu(&self) -> &menus::AppsMenu {
        &self.apps_menu
    }

    /// Apps › <an app>: checked again (the folder may have changed since
    /// the menu was built) and opened in the writer's browser, through the
    /// same path as the `open_app` tool ([`crate::browser::open_app`]).
    fn on_open_app(&mut self, a: &OpenApp, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        let said = match self.open.now() {
            None => "Open a script to use its apps.".to_string(),
            Some(open) => match crate::browser::open_app(&self.apps, &open.dir, &a.name) {
                Ok(title) => format!("Opened {title} in your browser."),
                Err(refused) => refused.text,
            },
        };
        self.set_notice(said, cx);
        self.refresh_apps_menu(cx);
    }

    /// Apps as served this run.
    pub(crate) fn apps(&self) -> &Apps {
        &self.apps
    }

    /// Apps as if served (tests of `open_app` and `list_apps`, A8).
    #[cfg(test)]
    pub(crate) fn set_apps(&mut self, apps: Apps) {
        self.apps = apps;
    }

    /// Every [`apps_host::TICK_EVERY`]: while a page listens, what changed
    /// in the open view goes to the pages.
    fn apps_tick(running: std::sync::Arc<scriptorium_apps::serve::Running>, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |ws, cx| {
            let start = std::time::Instant::now();
            let mut ticker = apps_host::Ticker::default();
            loop {
                cx.background_executor().timer(apps_host::TICK_EVERY).await;
                if running.listeners() == 0 {
                    // No page: nothing looked at, and a fresh start when
                    // one comes (its `hello` says where things stand).
                    ticker = apps_host::Ticker::default();
                    continue;
                }
                let now = start.elapsed().as_millis() as u64;
                let Ok(events) = ws.update(cx, |w, cx| ticker.step(now, true, w.view.as_ref(), w.open.now(), cx))
                else {
                    break;
                };
                for event in &events {
                    running.broadcast(event);
                }
            }
        })
    }

    /// Write .fdx in place, as the view's [`Reopen`] has it (tests).
    #[cfg(test)]
    pub(crate) fn reopen_for_test(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.reopen_written(Reopen { path, notice: String::new() }, window, cx);
    }

    pub(crate) fn set_notice(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let text = text.into();
        match &self.view {
            Some(v) => v.update(cx, |v, cx| v.set_notice(text, cx)),
            None => self.notice = text,
        }
        cx.notify();
    }

    // ---- opening -------------------------------------------------------------

    /// Ask for files (or take them from the test hook), then run `then`.
    fn pick(
        &mut self,
        multiple: bool,
        prompt: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, Vec<PathBuf>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        if let Some(paths) = std::env::var_os(PICK_ENV) {
            let paths: Vec<PathBuf> = std::env::split_paths(&paths).collect();
            then(self, paths, window, cx);
            return;
        }
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple,
            prompt: Some(prompt.to_string().into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let picked = rx.await;
            this.update_in(cx, |this, window, cx| match picked {
                Ok(Ok(Some(paths))) if !paths.is_empty() => then(this, paths, window, cx),
                Ok(Ok(_)) | Err(_) => {} // cancelled
                Ok(Err(e)) => {
                    crate::log::warn(&format!("file picker: {e:#}"));
                    this.set_notice(
                        "Couldn't open a file picker (no desktop portal?). Pass the path on the command line \
                         instead, or drop the file on the window.",
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    fn on_open(&mut self, _: &Open, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        self.pick(false, "Open", window, cx, |this, paths, window, cx| {
            this.open_script(paths[0].clone(), vec![], window, cx);
        });
    }

    fn on_add_review(&mut self, _: &AddReview, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        if self.view.is_none() {
            self.set_notice("Open a script first, then add reviews to it.", cx);
            return;
        }
        self.pick(true, "Add review", window, cx, |this, paths, window, cx| {
            this.add_reviews(paths, window, cx);
        });
    }

    /// Open `path` in place of the current script (whose session is saved first).
    fn open_script(&mut self, path: PathBuf, reviews: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if !has_extension(&path, "fdx") {
            self.set_notice(format!("{} isn't a Final Draft script (they end in .fdx).", file_name(&path)), cx);
            return;
        }
        // Save first: reopening the same script must read what's on screen
        // (a pending autosave or a rewrite in progress), not an older session.
        let set_aside = self.view.clone().and_then(|old| old.update(cx, |v, cx| v.save_now(cx)));
        let opened = match Opened::load(&path, &reviews) {
            Ok(o) => o,
            Err(e) => {
                // The current script stays open.
                crate::log::warn(&format!("opening {}: {e}", path.display()));
                self.set_notice(set_aside.map_or(e.clone(), |a| format!("{e}  {a}")), cx);
                return;
            }
        };
        self.show(opened, window, cx);
        if let Some(a) = set_aside {
            self.set_notice(a, cx);
        }
        cx.notify();
    }

    /// Show `opened` in place of the current view (dropped as it is).
    fn show(&mut self, opened: Opened, window: &mut Window, cx: &mut Context<Self>) {
        self.view = None;
        crate::log::info(&format!("opened {}", opened.script_path.display()));
        window.set_window_title(&format!("{} — Scriptorium", file_name(&opened.script_path)));
        let view = cx.new(|cx| ScriptView::new(opened, window, cx));
        // The toolbar shows the view's state, so redraw when it changes.
        self._observe = Some(cx.observe(&view, |_, _, cx| cx.notify()));
        self._reopen =
            Some(cx.subscribe_in(&view, window, |this, _, e: &Reopen, window, cx| {
                this.reopen_written(e.clone(), window, cx)
            }));
        self.view = Some(view);
        self.notice = "".into();
        self.note_open(cx);
        self.refresh_apps_menu(cx);
    }

    /// Write .fdx wrote the version in place: drop its view without saving
    /// it (the write saved a fresh session for the new bytes) and open the
    /// written file, saying what was written.
    fn reopen_written(&mut self, reopen: Reopen, window: &mut Window, cx: &mut Context<Self>) {
        match Opened::load(&reopen.path, &[]) {
            Ok(opened) => {
                self.show(opened, window, cx);
                self.set_notice(reopen.notice, cx);
            }
            Err(e) => {
                // The old view is retired: never left showing as if usable.
                crate::log::warn(&format!("reopening {}: {e}", reopen.path.display()));
                self.view = None;
                self._observe = None;
                self._reopen = None;
                self.note_open(cx);
                self.refresh_apps_menu(cx);
                window.set_window_title("Scriptorium");
                self.set_notice(format!("{}  Reopening it failed: {e}", reopen.notice), cx);
            }
        }
        cx.notify();
    }

    fn add_reviews(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(v) = &self.view {
            v.update(cx, |v, cx| v.add_reviews(&paths, window, cx));
        }
    }

    /// Dropped files: a `.fdx` opens (one), `.txt` files are added as reviews.
    fn dropped(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        let paths = paths.paths();
        let script = paths.iter().find(|p| has_extension(p, "fdx")).cloned();
        let reviews: Vec<PathBuf> = paths.iter().filter(|p| has_extension(p, "txt")).cloned().collect();
        let others: Vec<String> = paths
            .iter()
            .filter(|p| !has_extension(p, "fdx") && !has_extension(p, "txt"))
            .map(|p| file_name(p))
            .collect();
        match script {
            Some(s) => self.open_script(s, reviews, window, cx),
            None if !reviews.is_empty() && self.view.is_some() => self.add_reviews(reviews, window, cx),
            None if !reviews.is_empty() => self.set_notice("Open a script first, then drop reviews on it.", cx),
            None => {}
        }
        if !others.is_empty() {
            self.set_notice(
                format!("Not opened: {}. Drop a script (.fdx) or review files (.txt).", others.join(", ")),
                cx,
            );
        }
    }

    // ---- actions forwarded to the script -------------------------------------

    fn with_view(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut ScriptView, &mut Context<ScriptView>)) {
        if let Some(v) = self.view.clone() {
            v.update(cx, f);
        }
    }

    fn on_apply(&mut self, a: &ApplyToFdx, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        self.with_view(cx, |v, cx| v.apply_to_fdx(a, window, cx));
    }

    fn on_quit(&mut self, _: &SaveAndQuit, _: &mut Window, cx: &mut Context<Self>) {
        self.with_view(cx, |v, cx| _ = v.save_now(cx));
        cx.quit();
    }

    fn on_about(&mut self, _: &About, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        let log = crate::log::path().map_or("none".into(), |p| p.display().to_string());
        self.set_notice(format!("Scriptorium {} · log file: {log}", env!("CARGO_PKG_VERSION")), cx);
    }

    /// Scriptorium › Install command-line tool…: the result goes in the
    /// status bar.
    fn on_install_cli(&mut self, _: &InstallCli, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        if self.installing_cli {
            return;
        }
        self.installing_cli = true;
        self.set_notice("Installing the command-line tool…", cx);
        let task = connect::install_cli(cx);
        cx.spawn(async move |this, cx| {
            let (said, copy) = task.await;
            this.update(cx, |this, cx| {
                this.installing_cli = false;
                if let Some(command) = copy {
                    cx.write_to_clipboard(ClipboardItem::new_string(command));
                }
                this.set_notice(said, cx);
            })
            .ok();
        })
        .detach();
    }

    // ---- Connect an AI agent… and the launch check (plan E3) -------------------

    /// Scriptorium › Connect an AI agent…: the sheet, over the whole
    /// window (the agents are the user's, not the script's).
    fn on_connect_agent(&mut self, _: &ConnectAgent, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        if self.connect.is_some() || self.accounts.is_some() || self.new_version.is_some() {
            return;
        }
        let back = window.focused(cx);
        let sheet = cx.new(ConnectSheet::new);
        let sub = cx.subscribe_in(&sheet, window, |this, _, _: &connect::Closed, window, cx| {
            if let Some((_, back, _)) = this.connect.take() {
                window.focus(&back.unwrap_or_else(|| this.focus.clone()), cx);
            }
            cx.notify();
        });
        let into = sheet.focus_handle(cx);
        window.focus(&into, cx);
        self.connect = Some((sheet, back, sub));
        cx.notify();
    }

    /// The launch check: agents connected from here before are looked at
    /// off the UI thread; what was repaired silently is said in the status
    /// bar, and a fix that needs Claude quit is offered in a bar.
    fn check_agents(&mut self, cx: &mut Context<Self>) {
        let remembered = connect::connected_before();
        if remembered.is_empty() {
            return;
        }
        let task = connect::launch_check(remembered, cx);
        cx.spawn(async move |this, cx| {
            let checked = task.await;
            this.update(cx, |this, cx| {
                if let Some(said) = checked.said {
                    this.set_notice(said, cx);
                }
                if let Some(offer) = checked.offer {
                    this.fix_offer = Some((offer, false));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The offered fix's button: through "Quit and reopen Claude for me"
    /// (or the `.mcpb` again), off the UI thread.
    fn take_fix_offer(&mut self, cx: &mut Context<Self>) {
        let Some((offer, false)) = self.fix_offer else { return };
        self.fix_offer = Some((offer, true));
        let task = connect::fix_offer(offer, cx);
        cx.spawn(async move |this, cx| {
            let said = task.await;
            this.update(cx, |this, cx| {
                this.fix_offer = None;
                this.set_notice(said, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn render_fix_offer(&self, cx: &mut Context<Self>) -> Option<Div> {
        let (offer, running) = self.fix_offer?;
        Some(
            div()
                .flex_none()
                .px_3()
                .py_1p5()
                .flex()
                .items_center()
                .gap_2()
                .bg(WARN_SOFT)
                .border_b_1()
                .border_color(RULE)
                .text_size(px(12.))
                .child(div().flex_1().text_color(INK).child(offer.text()))
                .child(if running {
                    div().text_color(INK_2).child("Fixing\u{2026}").into_any_element()
                } else {
                    crate::script_view::button("fix-offer", offer.button(), None, true)
                        .on_click(cx.listener(|this, _, _, cx| this.take_fix_offer(cx)))
                        .into_any_element()
                })
                .when(!running, |d| {
                    d.child(crate::script_view::button("fix-offer-later", "Not now", None, false).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.fix_offer = None;
                            cx.notify();
                        }),
                    ))
                }),
        )
    }

    // ---- the agent panel and AI accounts ---------------------------------------

    fn on_toggle_agent(&mut self, _: &ToggleAgentPanel, window: &mut Window, cx: &mut Context<Self>) {
        // Under the accounts sheet, keys stay with the sheet.
        if self.accounts.is_some() || self.connect.is_some() {
            return;
        }
        self.close_menu(window, cx);
        if self.view.is_none() {
            self.set_notice("Open a script to chat about it.", cx);
            return;
        }
        self.with_view(cx, |v, cx| v.toggle_agent(window, cx));
    }

    /// The accounts are the user's, not the script's, so the sheet is drawn
    /// here over the whole window and opens with no script too.
    fn on_open_accounts(&mut self, _: &OpenAccounts, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        if self.accounts.is_some() || self.connect.is_some() {
            return;
        }
        let back = window.focused(cx);
        let sheet = cx.new(|cx| AccountsSheet::new(window, cx));
        let sub = cx.subscribe_in(&sheet, window, |this, _, _: &Dismissed, window, cx| {
            if let Some((_, back, _)) = this.accounts.take() {
                window.focus(&back.unwrap_or_else(|| this.focus.clone()), cx);
            }
            cx.notify();
        });
        let into = sheet.focus_handle(cx);
        window.focus(&into, cx);
        self.accounts = Some((sheet, back, sub));
        cx.notify();
    }

    // ---- the pair: New version, Swap, Mark in sync (plan B3) --------------------

    fn on_new_version(&mut self, _: &NewVersion, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        if self.accounts.is_some() || self.new_version.is_some() {
            return;
        }
        let Some(view) = self.view.clone() else {
            self.set_notice("Open a script first, then make a version of it.", cx);
            return;
        };
        let (script, refused) = {
            let v = view.read(cx);
            (file_name(v.script_path()), v.new_version_refusal())
        };
        if let Some(why) = refused {
            self.set_notice(why, cx);
            return;
        }
        let back = window.focused(cx);
        let sheet = cx.new(|cx| NewVersionSheet::new(script, window, cx));
        let sub = cx.subscribe_in(&sheet, window, |this, sheet, e: &SheetEvent, window, cx| match e {
            SheetEvent::Cancel => this.close_new_version(window, cx),
            SheetEvent::Create { lang, from, name } => {
                this.create_version(sheet.clone(), lang.clone(), from.clone(), name.clone(), window, cx)
            }
        });
        self.new_version = Some((sheet, back, sub));
        cx.notify();
    }

    fn close_new_version(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, back, _)) = self.new_version.take() {
            window.focus(&back.unwrap_or_else(|| self.focus.clone()), cx);
        }
        cx.notify();
    }

    /// The sheet's Create and open: make the version of the open script and
    /// open it with the original lined up on the left. Refused: said in the
    /// status bar and on the sheet, which stays open.
    fn create_version(
        &mut self,
        sheet: Entity<NewVersionSheet>,
        lang: String,
        from: Option<String>,
        name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.view.clone() else { return self.close_new_version(window, cx) };
        let original = file_name(view.read(cx).script_path());
        match view.update(cx, |v, cx| v.make_version(&lang, from.as_deref(), name.as_deref(), cx)) {
            Ok(made) => {
                self.close_new_version(window, cx);
                let said = made.summary().lines().next().unwrap_or_default().to_string();
                let mut chars = said.chars();
                let said: String = chars.next().into_iter().flat_map(char::to_uppercase).chain(chars).collect();
                self.open_lined_up(made.version, window, cx);
                if self.showing(&original, cx) {
                    return;
                }
                self.set_notice(format!("{said}. {original} is lined up on the left."), cx);
            }
            Err(e) => {
                crate::log::warn(&format!("new version: {e}"));
                sheet.update(cx, |s, cx| s.set_error(e.clone(), cx));
                self.set_notice(format!("No version made: {e}"), cx);
            }
        }
    }

    /// Whether the open script is the file named `name`.
    fn showing(&self, name: &str, cx: &App) -> bool {
        self.view.as_ref().is_some_and(|v| file_name(v.read(cx).script_path()) == name)
    }

    /// Open `path` in place of the current script, with the file of its pair
    /// lined up on the left (even if "Nothing" was chosen there before).
    fn open_lined_up(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = file_name(&path);
        self.open_script(path, vec![], window, cx);
        if self.showing(&name, cx) {
            self.with_view(cx, |v, cx| v.line_up_pair(cx));
        }
    }

    /// Swap: open the file lined up on the left, with this one lined up.
    fn on_swap(&mut self, _: &Swap, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        match self.view.as_ref().and_then(|v| v.read(cx).swap_target()) {
            Some(other) => self.open_lined_up(other, window, cx),
            None => self.set_notice("Nothing is lined up on the left to swap with.", cx),
        }
    }

    fn on_mark(&mut self, sync: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        if self.view.is_none() {
            self.set_notice("Open a script first.", cx);
            return;
        }
        self.with_view(cx, |v, cx| v.mark_row(sync, window, cx));
    }

    // ---- the Linux File dropdown ---------------------------------------------

    fn toggle_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.is_some() {
            self.close_menu(window, cx);
        } else {
            self.refresh_apps_menu(cx);
            self.menu = Some(window.focused(cx));
            window.focus(&self.menu_focus, cx);
            cx.notify();
        }
    }

    /// Gives focus back to what had it when the menu opened. Menu items call
    /// this before dispatching their action, and `dispatch_action` goes to the
    /// element focused when it is called, so an item acts where the writer
    /// was (Edit › Undo with the composer focused undoes the composer).
    fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(prev) = self.menu.take() {
            match prev {
                Some(f) => window.focus(&f, cx),
                None => window.focus(&self.focus, cx),
            }
            cx.notify();
        }
    }

    fn render_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let has_script = self.view.is_some();
        let items: Vec<gpui::AnyElement> = menus::file_dropdown(&self.apps_menu)
            .into_iter()
            .enumerate()
            .map(|(ix, item)| match item {
                Item::Separator => div().h(px(1.)).my_1().bg(RULE).into_any_element(),
                Item::Heading(h) => div()
                    .px_3()
                    .pt_1()
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(INK_3)
                    .child(h)
                    .into_any_element(),
                Item::Action(e) => {
                    let enabled = e.action.is_some() && (has_script || !e.needs_script);
                    let action = e.action;
                    div()
                        .id(("menu-item", ix))
                        .flex()
                        .items_center()
                        .gap_4()
                        .px_3()
                        .py_1()
                        .rounded_sm()
                        .text_color(if enabled { INK } else { INK_3 })
                        .when(enabled, |d| {
                            d.cursor_pointer().hover(|s| s.bg(ACCENT_SOFT)).on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.close_menu(window, cx);
                                    if let Some(action) = &action {
                                        window.dispatch_action(action.boxed_clone(), cx);
                                    }
                                },
                            ))
                        })
                        .child(div().flex_1().min_w_0().truncate().child(e.name))
                        .when_some(e.key, |d, k| d.child(div().text_color(INK_3).child(menus::shortcut(k))))
                        .into_any_element()
                }
            })
            .collect();
        // A full-window layer: clicking outside the menu closes it.
        deferred(
            div()
                .id("menu-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close_menu(window, cx)))
                .child(
                    div()
                        .id("file-menu")
                        .track_focus(&self.menu_focus)
                        .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                            if e.keystroke.key == "escape" {
                                this.close_menu(window, cx);
                            }
                        }))
                        // Clicks inside the menu don't reach the backdrop.
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .absolute()
                        .top(px(34.))
                        .left(px(8.))
                        .w(px(260.))
                        .p_1()
                        .flex()
                        .flex_col()
                        .bg(PAPER)
                        .border_1()
                        .border_color(RULE)
                        .rounded_md()
                        .shadow_md()
                        .children(items),
                ),
        )
        .with_priority(1)
    }

    // ---- rendering -----------------------------------------------------------

    fn render_toolbar(&self, cx: &mut Context<Self>) -> Div {
        let show_file_button = !cfg!(target_os = "macos");
        let (name, state, header) = match &self.view {
            Some(v) => {
                let v = v.read(cx);
                (file_name(v.script_path()), v.save_state(cx), v.pair_header())
            }
            None => ("Scriptorium".to_string(), String::new(), None),
        };
        // The columns in their order, the open file's bold: "English |
        // **Español**", and Swap. It names the open file, so the file's name
        // isn't shown before it (on macOS the title bar names the file).
        // The left's label is dimmed while that column gave way for width.
        let name = if header.is_some() { None } else { Some(name) };
        let header = header.map(|h| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .text_color(INK_2)
                .child(div().truncate().when(h.left_hidden, |d| d.text_color(INK_3)).child(h.left))
                .child(div().flex_none().text_color(RULE).child("|"))
                .child(div().truncate().font_weight(FontWeight::BOLD).text_color(INK).child(h.right))
                .child(
                    crate::script_view::button("swap", "Swap", None, false)
                        .flex_none()
                        .on_click(cx.listener(|this, _, window, cx| this.on_swap(&Swap, window, cx))),
                )
        });
        div()
            .flex_none()
            .h(px(38.))
            // A family with bold faces (the Linux system UI font shows none).
            .font_family(crate::agent_panel::panel_font(cx))
            .px_2()
            .flex()
            .items_center()
            .gap_3()
            .bg(CHROME)
            .border_b_1()
            .border_color(RULE)
            .when(show_file_button, |d| {
                d.child(
                    toolbar_button("file-button", "File", self.menu.is_some())
                        // Pressing here would focus the workspace (it tracks
                        // focus) before the menu opens; keep the focus where
                        // the writer was, so the menu can give it back and its
                        // items act there (Edit › Undo in the composer).
                        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                        .on_click(cx.listener(|this, _, window, cx| this.toggle_menu(window, cx))),
                )
            })
            .children(name.map(|name| div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(INK).child(name)))
            .children(header)
            .child(div().flex_1())
            .child(div().text_color(INK_2).child(state))
            .when(self.view.is_some(), |d| {
                d.child(
                    crate::script_view::button("apply", "Write .fdx", Some(&menus::shortcut("s")), true)
                        .on_click(cx.listener(|this, _, window, cx| this.on_apply(&ApplyToFdx, window, cx))),
                )
            })
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> Div {
        div().flex_1().min_h_0().flex().items_center().justify_center().bg(SIDE).child(
            div()
                .w(px(460.))
                .p_6()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .bg(PAPER)
                .border_1()
                .border_color(RULE)
                .rounded_lg()
                .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).child("Scriptorium"))
                .child(
                    div()
                        .text_color(INK_2)
                        .text_center()
                        .child("Work through proposed changes to a Final Draft script, one at a time."),
                )
                .child(
                    crate::script_view::button("open-script", "Open a script…", Some(&menus::shortcut("o")), true)
                        .on_click(cx.listener(|this, _, window, cx| this.on_open(&Open, window, cx))),
                )
                .child(div().text_size(px(12.)).text_color(INK_3).child("or drop a .fdx file on this window")),
        )
    }

    fn render_status(&self) -> Div {
        div()
            .flex_none()
            .h(px(28.))
            .px_3()
            .flex()
            .items_center()
            .bg(CHROME)
            .border_t_1()
            .border_color(RULE)
            .text_size(px(12.))
            .text_color(INK_2)
            .child(div().flex_1())
            .child(if self.notice.is_empty() {
                div().child(format!("{} open a script", menus::shortcut("o")))
            } else {
                div().text_color(INK).font_weight(FontWeight::SEMIBOLD).child(self.notice.clone())
            })
    }
}

fn toolbar_button(id: &'static str, text: &'static str, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded_md()
        .cursor_pointer()
        .text_color(INK)
        .hover(|s| s.bg(ACCENT_SOFT))
        .when(on, |d| d.bg(ACCENT_SOFT))
        .child(text)
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let menu = self.menu.is_some().then(|| self.render_menu(cx));
        let accounts = self.accounts.as_ref().map(|(sheet, _, _)| sheet.clone());
        let new_version = self.new_version.as_ref().map(|(sheet, _, _)| sheet.clone());
        let connect = self.connect.as_ref().map(|(sheet, _, _)| sheet.clone());
        let fix_offer = self.render_fix_offer(cx);
        let body = match &self.view {
            Some(v) => div().flex_1().min_h_0().flex().flex_col().child(v.clone()),
            None => div().flex_1().min_h_0().flex().flex_col().child(self.render_empty(cx)).child(self.render_status()),
        };
        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(CHROME)
            .text_color(INK)
            .text_size(px(13.))
            .on_action(cx.listener(Self::on_open))
            .on_action(cx.listener(Self::on_add_review))
            .on_action(cx.listener(Self::on_apply))
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(Self::on_about))
            .on_action(cx.listener(Self::on_open_app))
            .on_action(cx.listener(Self::on_install_cli))
            .on_action(cx.listener(Self::on_connect_agent))
            .on_action(cx.listener(Self::on_toggle_agent))
            .on_action(
                cx.listener(|this, _: &crate::agent_panel::Stop, _, cx| this.with_view(cx, |v, cx| v.stop_agent(cx))),
            )
            .on_action(cx.listener(Self::on_open_accounts))
            .on_action(cx.listener(Self::on_new_version))
            .on_action(cx.listener(Self::on_swap))
            .on_action(cx.listener(|this, _: &MarkInSync, window, cx| this.on_mark(true, window, cx)))
            .on_action(cx.listener(|this, _: &MarkNotInSync, window, cx| this.on_mark(false, window, cx)))
            .on_action(
                cx.listener(|this, _: &UndoAny, window, cx| this.with_view(cx, |v, cx| v.undo_any(false, window, cx))),
            )
            .on_action(
                cx.listener(|this, _: &RedoAny, window, cx| this.with_view(cx, |v, cx| v.undo_any(true, window, cx))),
            )
            .on_action(
                cx.listener(|this, _: &ShowScript, window, cx| this.with_view(cx, |v, cx| v.show_nth(0, window, cx))),
            )
            .on_action(
                cx.listener(|this, _: &ShowReview1, window, cx| this.with_view(cx, |v, cx| v.show_nth(1, window, cx))),
            )
            .on_action(
                cx.listener(|this, _: &ShowReview2, window, cx| this.with_view(cx, |v, cx| v.show_nth(2, window, cx))),
            )
            .on_action(
                cx.listener(|this, _: &ShowReview3, window, cx| this.with_view(cx, |v, cx| v.show_nth(3, window, cx))),
            )
            .on_action(
                cx.listener(|this, _: &NextView, window, cx| {
                    this.with_view(cx, |v, cx| v.cycle_view(true, window, cx))
                }),
            )
            .on_action(
                cx.listener(|this, _: &PrevView, window, cx| {
                    this.with_view(cx, |v, cx| v.cycle_view(false, window, cx))
                }),
            )
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| this.dropped(paths, window, cx)))
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(ACCENT_SOFT))
            .child(self.render_toolbar(cx))
            .children(fix_offer)
            .child(body)
            .children(accounts)
            .children(new_version)
            .children(connect)
            .children(menu)
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, WindowHandle};

    use super::*;
    use crate::menus::{AppLine, AppsMenu};

    const SCRIPT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Scene Heading"><Text>INT. KITCHEN - DAY</Text></Paragraph>
    <Paragraph Type="Action"><Text>Laura smiles.</Text></Paragraph>
  </Content>
</FinalDraft>
"#;

    /// A folder holding `film.fdx`, removed when dropped.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("scriptorium-ws-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
            Folder(dir.canonicalize().unwrap())
        }

        fn app(&self, name: &str, title: &str, index: bool) {
            let app = self.0.join("apps").join(name);
            std::fs::create_dir_all(&app).unwrap();
            let manifest = format!(r#"{{"scriptorium": 1, "title": "{title}", "description": "d"}}"#);
            std::fs::write(app.join("app.json"), manifest).unwrap();
            if index {
                std::fs::write(app.join("index.html"), "<p>hi</p>").unwrap();
            }
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn workspace(cx: &mut TestAppContext, script: Option<PathBuf>) -> WindowHandle<Workspace> {
        cx.update(|cx| {
            crate::agent_panel::init(cx);
            cx.bind_keys(crate::bindings());
        });
        let w = cx.add_window(|window, cx| Workspace::new(script, vec![], window, cx));
        // As served (on a free port, calls going nowhere), so apps list.
        let port = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port();
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        let host = std::sync::Arc::new(apps_host::AppHost::new(OpenScript::default(), tx));
        let running = scriptorium_apps::serve::Server::start(port, "t".repeat(64), host).unwrap();
        let apps = Apps::Serving { running: std::sync::Arc::new(running), token: "t".repeat(64) };
        w.update(cx, |w, _, cx| {
            w.apps = apps;
            w.refresh_apps_menu(cx);
        })
        .unwrap();
        w
    }

    #[gpui::test]
    fn not_serving_apps_the_menu_says_where_they_open(cx: &mut TestAppContext) {
        let f = Folder::new("off");
        f.app("scene-list", "Scene list", true);
        let w = workspace(cx, Some(f.0.join("film.fdx")));
        w.update(cx, |w, _, cx| {
            w.apps = Apps::Off;
            w.refresh_apps_menu(cx);
        })
        .unwrap();
        assert_eq!(menu(cx, w), AppsMenu::Unavailable(crate::browser::APPS_OFF.into()));
    }

    fn menu(cx: &mut TestAppContext, w: WindowHandle<Workspace>) -> AppsMenu {
        w.read_with(cx, |w, _| w.apps_menu().clone()).unwrap()
    }

    fn line(name: &str, title: &str, problem: Option<&str>) -> AppLine {
        AppLine { name: name.into(), title: title.into(), problem: problem.map(Into::into) }
    }

    #[gpui::test]
    fn the_apps_menu_follows_the_folder_when_the_dropdown_opens(cx: &mut TestAppContext) {
        let f = Folder::new("follows");
        f.app("scene-list", "Scene list", true);
        let w = workspace(cx, Some(f.0.join("film.fdx")));
        // Scanned when the script opened.
        assert_eq!(menu(cx, w), AppsMenu::Listed(vec![line("scene-list", "Scene list", None)]));

        // An agent builds another app (one not finished yet): the next
        // look at the dropdown shows it, with no server thread involved.
        f.app("storyboard", "Storyboard", false);
        w.update(cx, |w, window, cx| w.toggle_menu(window, cx)).unwrap();
        assert_eq!(
            menu(cx, w),
            AppsMenu::Listed(vec![
                line("scene-list", "Scene list", None),
                line("storyboard", "Storyboard", Some("index.html is missing")),
            ])
        );
        w.update(cx, |w, window, cx| w.close_menu(window, cx)).unwrap();

        std::fs::remove_dir_all(f.0.join("apps")).unwrap();
        w.update(cx, |w, _, cx| w.refresh_apps_menu(cx)).unwrap();
        assert_eq!(menu(cx, w), AppsMenu::Listed(vec![]));
    }

    #[gpui::test]
    fn with_no_script_the_apps_menu_says_open_one(cx: &mut TestAppContext) {
        let w = workspace(cx, None);
        assert_eq!(menu(cx, w), AppsMenu::NoScript);
    }

    #[gpui::test]
    fn with_no_server_the_menu_and_opening_say_why(cx: &mut TestAppContext) {
        let f = Folder::new("unavailable");
        f.app("scene-list", "Scene list", true);
        let w = workspace(cx, Some(f.0.join("film.fdx")));
        let why = "Apps can't open: another program is using port 28417. Quit it and restart Scriptorium.";
        w.update(cx, |w, _, cx| {
            w.apps = Apps::Unavailable(why.into());
            w.refresh_apps_menu(cx);
        })
        .unwrap();
        assert_eq!(menu(cx, w), AppsMenu::Unavailable(why.into()));

        w.update(cx, |_, window, cx| window.dispatch_action(Box::new(OpenApp { name: "scene-list".into() }), cx))
            .unwrap();
        cx.run_until_parked();
        let notice = w.read_with(cx, |w, cx| w.script_view().unwrap().read(cx).notice_text()).unwrap();
        assert_eq!(notice.as_ref(), why);
    }
}
