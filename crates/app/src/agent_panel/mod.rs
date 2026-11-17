//! The agent panel: a fixed 360 px column at the right of the open script
//! (after the review card) where the writer chats with an agent. See
//! docs/plans/chat-panel.md.
//!
//! This module is the shell: the `AgentPanel` entity, its actions and key
//! bindings, focus and undo routing. The parts are child entities, each in
//! its own file, made in [`AgentPanel::new`]:
//!
//! - `view_model.rs` (pure): a thread plus the streaming buffer become rows.
//! - `messages.rs` (U3): [`messages::MessageView`], the message list.
//! - `composer.rs` (U6): [`composer::Composer`], the one textarea focus and
//!   undo route to, Send/Stop, the cards above it (disclosure, whole-script
//!   read, setup), notices, the model chip and usage. It observes the chat
//!   and [`accounts::AccountsChanged`] itself; the panel forwards its keys.
//! - `threads.rs` (U7): [`threads::ThreadList`], the header's title and the
//!   thread list behind it.
//! - `driver.rs` (U4) and `live.rs` (U5): runs. The panel holds a weak handle
//!   to its [`ScriptView`] and the script's path ([`AgentPanel::script`],
//!   [`AgentPanel::script_path`]); tool calls run inside one update of the
//!   script view, over its `pub(crate)` live-store helpers
//!   (`ScriptView::try_lock_session`, `reload_or_busy`, `attach_review`,
//!   `session_mut`, `editing_para`, `after_agent_change`, `show_suggestion`).
//! - `accounts.rs` (U8): the AI accounts sheet, which the workspace draws over
//!   the whole window ([`OpenAccounts`]), so it opens with no script too.
//!
//! The panel belongs to the open script (`ScriptView` owns it and keeps it
//! while closed, so a draft and a run survive Ctrl+L); ScriptView decides
//! where focus goes when the panel gives it back ([`AgentPanelEvent`]).
//! Focus never moves into the panel on its own.

pub mod accounts;
pub mod composer;
#[cfg(debug_assertions)]
mod debug_fixture;
pub mod driver;
mod live;
pub mod messages;
pub mod threads;
mod view_model;

use std::path::{Path, PathBuf};

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, Global, KeyBinding, SharedString, WeakEntity, Window,
    actions, div, prelude::*, px,
};

use crate::layout::GUTTER;
use crate::script_view::{RedoAny, ScriptView, UndoAny};
use crate::theme::*;
use composer::{Composer, ComposerEvent};
use driver::{Chat, ChatEvent, ErrorAction, ViewOp};
use messages::{MessageEvent, MessageView};
use scriptorium_providers::shell::Keys;
use threads::ThreadList;

actions!(
    agent,
    [
        /// Ctrl/Cmd+L: open the panel and focus the composer; with the
        /// composer focused, close it and give focus back to the script.
        ToggleAgentPanel,
        /// Escape in the panel: focus back to the script; the panel stays open.
        ReturnFocus,
        /// Enter in the composer.
        Send,
        /// Cmd/Ctrl+. in the panel.
        Stop,
        /// Cmd/Ctrl+N in the panel.
        NewThread,
        /// Tab / Shift+Tab in the panel: the next / previous control.
        FocusNext,
        FocusPrev,
        /// Scriptorium › AI accounts… (Linux: the File dropdown), and the
        /// model picker's "Manage accounts…".
        OpenAccounts,
    ]
);

/// The panel's width (drag-resizing is deferred).
pub const WIDTH: f32 = 360.;

/// The sidebar's and the review card's widths.
pub const SIDEBAR_WIDTH: f32 = 270.;
pub const CARD_WIDTH: f32 = 380.;

/// The sidebar's width when it is a rail (letters for the views).
pub const RAIL_WIDTH: f32 = 44.;

/// The narrowest the panel and the card get when the window is narrow.
pub const PANEL_MIN: f32 = 300.;
pub const CARD_MIN: f32 = 300.;

/// The narrowest the script column gets while it shows: the text column and
/// its marks at the smallest readable zoom ([`crate::layout::READABLE_ZOOM`]).
pub fn script_min() -> f32 {
    crate::layout::column_width_at(crate::layout::READABLE_ZOOM)
}

/// The window's minimum size: the rail, the narrowest panel and the
/// narrowest script column (about 757 × 500 px), or the rail, card and
/// panel at their narrowest if that is wider. Small enough for half of a
/// 1920 px screen, or 1366 px at 125 %, or 1920 px at 200 %.
pub fn window_min_size() -> (f32, f32) {
    let w = (RAIL_WIDTH + PANEL_MIN + script_min()).max(RAIL_WIDTH + CARD_MIN + PANEL_MIN);
    (w.ceil(), 500.)
}

/// How the window's width is shared out ([`columns`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Columns {
    /// The sidebar shows as a rail.
    pub rail: bool,
    /// The card's and the panel's widths (0 when not showing).
    pub card: f32,
    pub panel: f32,
    /// The script column shows. When even the narrowest card and panel leave
    /// it too little room, one of the two gives way (see [`columns`]).
    pub script: bool,
    /// The script lined up on the left shows beside the open one (asked
    /// for with `lined_up`, and there is room for both at a readable zoom).
    pub left: bool,
}

impl Columns {
    /// A review card that gave way to the script (rewriting, typing in it).
    pub fn card_hidden(&self, card: bool) -> bool {
        card && self.card == 0.
    }

    /// The script lined up on the left gave way for width: it was asked
    /// for but isn't shown.
    pub fn left_hidden(&self, lined_up: bool) -> bool {
        lined_up && !self.left
    }
}

/// The columns for a window `width` wide, with the panel open or not, a
/// review card or not, and the other script `lined_up` on the left or not.
/// Narrowing goes in this order, so nothing is ever cut off at or above
/// [`window_min_size`]: the sidebar becomes a rail; the page's zoom shrinks
/// (down to `READABLE_ZOOM`, by the editor's fit; both columns share it);
/// the panel narrows to [`PANEL_MIN`]; the card narrows to [`CARD_MIN`];
/// then the left column gives way (the script area shows the open script
/// alone, with a note); then the script gives way to the card, which shows
/// the line under review — unless `keep_script` (the writer is rewriting or
/// typing in the script): then the card gives way instead, so the keys
/// never go to a hidden column.
pub fn columns(width: f32, panel_open: bool, card: bool, keep_script: bool, lined_up: bool) -> Columns {
    if lined_up {
        let (c, over) = share(width, panel_open, card, crate::layout::two_columns_min(), 2. * EDITOR_MIN + GUTTER);
        if over <= 0. {
            return Columns { left: true, ..c };
        }
        // The left gives way; the sidebar stays a rail (it gave way first).
        let (c, over) = share_beside(width, true, panel_open, card, script_min());
        return finish(c, over, width, card, keep_script);
    }
    let (c, over) = share(width, panel_open, card, script_min(), EDITOR_MIN);
    finish(c, over, width, card, keep_script)
}

/// The columns beside a script area of at least `need`, and by how much
/// they fall short of it (`> 0`) with the panel and card at their narrowest.
/// With the panel open the sidebar is a rail already when the script area
/// would be under `roomy` ([`EDITOR_MIN`] for one column).
fn share(width: f32, panel_open: bool, card: bool, need: f32, roomy: f32) -> (Columns, f32) {
    let panel = if panel_open { WIDTH } else { 0. };
    let card_w = if card { CARD_WIDTH } else { 0. };
    let area = width - SIDEBAR_WIDTH - panel - card_w;
    let rail = (panel_open && area < roomy) || area < need;
    share_beside(width, rail, panel_open, card, need)
}

/// [`share`] with the sidebar's look given.
fn share_beside(width: f32, rail: bool, panel_open: bool, card: bool, need: f32) -> (Columns, f32) {
    let mut panel = if panel_open { WIDTH } else { 0. };
    let mut card_w = if card { CARD_WIDTH } else { 0. };
    let side = if rail { RAIL_WIDTH } else { SIDEBAR_WIDTH };
    let mut over = side + panel + card_w + need - width;
    if over > 0. && panel_open {
        let d = over.min(WIDTH - PANEL_MIN);
        panel -= d;
        over -= d;
    }
    if over > 0. && card {
        let d = over.min(CARD_WIDTH - CARD_MIN);
        card_w -= d;
        over -= d;
    }
    (Columns { rail, card: card_w, panel, script: true, left: false }, over)
}

/// The last step for one script column: too little room, and the script
/// or the card gives way.
fn finish(c: Columns, over: f32, width: f32, card: bool, keep_script: bool) -> Columns {
    if over <= 0. || !card {
        return c;
    }
    let side = if c.rail { RAIL_WIDTH } else { SIDEBAR_WIDTH };
    if keep_script {
        Columns { card: 0., ..c }
    } else {
        Columns { card: (width - side - c.panel).max(CARD_MIN), script: false, ..c }
    }
}

/// The narrowest the editor gets before the sidebar gives way (the page at
/// about 70 % zoom; its fit-to-width zoom absorbs the rest).
pub const EDITOR_MIN: f32 = 600.;

/// Cmd on macOS, Ctrl elsewhere (as in main.rs).
macro_rules! sec {
    ($k:literal) => {
        if cfg!(target_os = "macos") { concat!("cmd-", $k) } else { concat!("ctrl-", $k) }
    };
}

#[cfg(not(target_os = "macos"))]
/// The panel's text font on Linux, bundled: GPUI draws only the default
/// instance of a variable font, and the usual system UI fonts (Fedora's,
/// Adwaita Sans) are variable, so they show no bold or italic.
const BUNDLED_FONT: &str = "Inter";

/// Registers gpui-base (its `Input`, `Select`, `Sheet` bindings) and the
/// panel's font. Call before binding the app's keys, so ours, added after,
/// win where both match at the same depth.
pub fn init(cx: &mut App) {
    gpui_base::init(cx);
    // A replayed cassette (SCRIPTORIUM_REPLAY) with exchanges never requested
    // fails the scenario: checked once, when the app quits.
    cx.on_app_quit(|_| {
        let finished = driver::finish_transport();
        async move {
            if let Err(e) = finished {
                crate::log::error(&format!("agent transport: {e}"));
                eprintln!("scriptorium-app: {e}");
                std::process::exit(3);
            }
        }
    })
    .detach();
    #[cfg(target_os = "macos")]
    let font = ".SystemUIFont";
    #[cfg(not(target_os = "macos"))]
    let font = load_bundled_font(cx);
    crate::log::info(&format!("agent panel font: {font}"));
    if let Some(cassette) = replaying() {
        crate::log::warn(&format!(
            "replaying {cassette} ({} is set): nothing is sent to a provider",
            scriptorium_providers::shell::REPLAY_ENV
        ));
    }
    if scriptorium_providers::shell::keyring_off() {
        crate::log::info(&format!(
            "{}=off: keys come from the environment only; the keyring isn't touched",
            scriptorium_providers::shell::KEYRING_ENV
        ));
    }
    cx.set_global(PanelFont(font.into()));
}

/// The cassette replayed instead of talking to providers, when
/// `SCRIPTORIUM_REPLAY` is set (see `transport_from_env`): logged at start
/// and shown under the panel's header, as the CLI prints a note, so nobody
/// mistakes recorded replies for a live model.
fn replaying() -> Option<String> {
    std::env::var_os(scriptorium_providers::shell::REPLAY_ENV)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string_lossy().into_owned())
}

/// The strip under the header while [`replaying`].
pub(crate) const REPLAY_NOTICE: &str = "Replaying a recording: nothing is sent to a provider.";

/// The keys a run and the accounts sheet use, so both always agree: the
/// keyring and the environment, or the environment alone with
/// `SCRIPTORIUM_KEYRING=off` (as the CLI). In debug builds
/// `SCRIPTORIUM_TEST_KEYS=memory` (kept in this process only) or `=env`
/// replace the keyring for tests; release builds ignore it.
pub(crate) fn keys() -> Keys {
    #[cfg(debug_assertions)]
    match std::env::var("SCRIPTORIUM_TEST_KEYS").as_deref() {
        Ok("memory") => return Keys::in_memory(|k| std::env::var(k).ok()),
        Ok("env") => return Keys::env_only(),
        _ => {}
    }
    Keys::from_env()
}

/// Inter (SIL OFL 1.1, assets/fonts/LICENSE-Inter.txt): static regular,
/// italic, semibold, bold and bold italic faces.
#[cfg(not(target_os = "macos"))]
fn load_bundled_font(cx: &mut App) -> &'static str {
    use std::borrow::Cow;
    {
        let fonts: Vec<Cow<'static, [u8]>> = vec![
            Cow::Borrowed(include_bytes!("../../assets/fonts/Inter-Regular.ttf")),
            Cow::Borrowed(include_bytes!("../../assets/fonts/Inter-Italic.ttf")),
            Cow::Borrowed(include_bytes!("../../assets/fonts/Inter-SemiBold.ttf")),
            Cow::Borrowed(include_bytes!("../../assets/fonts/Inter-Bold.ttf")),
            Cow::Borrowed(include_bytes!("../../assets/fonts/Inter-BoldItalic.ttf")),
        ];
        if let Err(e) = cx.text_system().add_fonts(fonts) {
            crate::log::warn(&format!("could not load {BUNDLED_FONT}: {e}"));
            return ".SystemUIFont";
        }
    }
    BUNDLED_FONT
}

struct PanelFont(SharedString);
impl Global for PanelFont {}

/// The panel's text font (a family with bold and italic).
pub fn panel_font(cx: &App) -> SharedString {
    cx.try_global::<PanelFont>().map_or_else(|| ".SystemUIFont".into(), |f| f.0.clone())
}

/// The panel's key bindings. Added after `gpui_base::init` and the app's own.
pub fn bindings() -> Vec<KeyBinding> {
    let panel = Some("AgentPanel");
    // Overrides of gpui-base's `Input` keys, only in the composer: the same
    // depth as gpui-base's bindings and registered after them, so these win.
    let composer = Some("AgentComposer > Input");
    let accounts_field = Some("AccountsSheet > Input");
    let mut b = vec![
        KeyBinding::new(sec!("l"), ToggleAgentPanel, Some("Workspace")),
        KeyBinding::new("escape", ReturnFocus, panel),
        KeyBinding::new(sec!("."), Stop, panel),
        // Stop works from anywhere in the window too (editor, review card,
        // the accounts sheet) while a run goes on, panel open or closed:
        // the workspace forwards it.
        KeyBinding::new(sec!("."), Stop, Some("Workspace")),
        KeyBinding::new(sec!("n"), NewThread, panel),
        KeyBinding::new("tab", FocusNext, panel),
        KeyBinding::new("shift-tab", FocusPrev, panel),
        KeyBinding::new("enter", Send, composer),
        KeyBinding::new(sec!("enter"), Send, composer),
        KeyBinding::new("escape", ReturnFocus, composer),
        KeyBinding::new("tab", FocusNext, composer),
        KeyBinding::new("shift-tab", FocusPrev, composer),
        // The accounts sheet's text fields: gpui-base's Tab is "indent"; here it moves on.
        KeyBinding::new("tab", FocusNext, accounts_field),
        KeyBinding::new("shift-tab", FocusPrev, accounts_field),
        KeyBinding::new("tab", FocusNext, Some("AccountsSheet")),
        KeyBinding::new("shift-tab", FocusPrev, Some("AccountsSheet")),
        // gpui-base: Cmd/Ctrl+. is ToggleCodeActions inside `Input`.
        KeyBinding::new(sec!("."), Stop, composer),
        KeyBinding::new(sec!("."), Stop, accounts_field),
        // gpui-base's search and replace have no UI here.
        KeyBinding::new(sec!("f"), gpui::NoAction, composer),
        KeyBinding::new(if cfg!(target_os = "macos") { "cmd-shift-f" } else { "ctrl-h" }, gpui::NoAction, composer),
    ];
    b.extend(threads::bindings());
    if !cfg!(target_os = "macos") {
        // gpui-base's only redo key on Linux is Ctrl+Y; the app's is Ctrl+Shift+Z.
        // Every text field (the composer, the accounts sheet's) gets it.
        b.push(KeyBinding::new("ctrl-shift-z", gpui_base::input::Redo, Some("Input")));
    }
    b
}

/// What the panel asks of the script view around it.
pub enum AgentPanelEvent {
    /// Give focus back to the script: the card in a review view, else the editor.
    ReturnFocus,
    /// The header's ×: close the panel (focus goes back to the script if it
    /// was in the panel).
    Close,
    /// A notice for the status bar: how a run ended, or that it needs the
    /// writer, while the panel was closed.
    Say(SharedString),
    /// Take back a [`Say`](Self::Say) line if the status bar still shows it:
    /// the question it pointed to is on screen or answered.
    Unsay(SharedString),
}

pub struct AgentPanel {
    /// Tracks the whole column, so "focus is in the panel" is one check.
    focus: FocusHandle,
    script: WeakEntity<ScriptView>,
    script_path: PathBuf,
    threads: Entity<ThreadList>,
    messages: Entity<MessageView>,
    composer: Entity<Composer>,
    /// The conversation and its runs (driver.rs).
    chat: Entity<Chat>,
    /// The status bar shows [`say_asking`] (taken back once the question
    /// shows in the open panel, or is answered).
    said_asking: bool,
    /// The column's width ([`columns`]), set by the script view.
    width: f32,
    _chat: Vec<gpui::Subscription>,
}

impl EventEmitter<AgentPanelEvent> for AgentPanel {}

#[allow(dead_code)] // Accessors for the driver, live store and parts (U3–U7).
impl AgentPanel {
    pub fn new(
        script: WeakEntity<ScriptView>,
        script_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let chat = cx.new(|_| Chat::new(script.clone(), script_path.clone()));
        let messages = cx.new(|cx| MessageView::new(script.clone(), window, cx));
        let observe = cx.observe(&chat, |this: &mut Self, chat, cx| {
            let (ops, title) = chat.update(cx, |c, _| (c.take_view_ops(), c.title()));
            this.messages.update(cx, |m, cx| apply_view_ops(m, ops, cx));
            this.threads.update(cx, |t, cx| t.set_title(title, cx));
            if chat.read(cx).question().is_none() {
                this.unsay_asking(cx);
            }
            cx.notify();
        });
        let events = cx.subscribe_in(&chat, window, Self::on_chat_event);
        let actions = cx.subscribe_in(&messages, window, Self::on_message_event);
        let composer = cx.new(|cx| Composer::new(chat.clone(), window, cx));
        let mut subs = vec![observe, events, actions, cx.subscribe(&composer, Self::on_composer_event)];
        // Closing the panel (Ctrl+L or ×) closes the composer's cards and picker.
        if let Some(view) = script.upgrade() {
            // Not read here: the script view is making this panel.
            let mut was_open = false;
            subs.push(cx.observe(&view, move |this: &mut Self, view, cx| {
                let open = view.read(cx).agent_open();
                if was_open && !open {
                    this.composer.update(cx, |c, cx| c.reset(cx));
                    // Closed with a question showing: the run still waits for it.
                    if this.chat.read(cx).question().is_some() {
                        this.say_asking(cx);
                    }
                } else if open && !was_open {
                    // The question card is on screen now.
                    this.unsay_asking(cx);
                }
                was_open = open;
            }));
        }
        Self {
            focus: cx.focus_handle(),
            script: script.clone(),
            script_path,
            threads: cx.new(|cx| ThreadList::new(chat.clone(), window, cx)),
            messages,
            composer,
            chat,
            said_asking: false,
            width: WIDTH,
            _chat: subs,
        }
    }

    /// The column's width, as [`columns`] shares out the window.
    pub fn set_width(&mut self, width: f32, cx: &mut Context<Self>) {
        if self.width != width {
            self.width = width;
            self.threads.update(cx, |t, cx| t.set_width(width, cx));
            self.composer.update(cx, |c, cx| c.set_width(width, cx));
            cx.notify();
        }
    }

    /// The conversation and its runs: send, stop, retry, open a thread.
    pub fn chat(&self) -> &Entity<Chat> {
        &self.chat
    }

    /// Whether the panel is showing (it lives on while closed).
    fn is_open(&self, cx: &App) -> bool {
        self.script.upgrade().is_some_and(|v| v.read(cx).agent_open())
    }

    /// An error row's button.
    fn on_message_event(
        &mut self,
        _: &Entity<MessageView>,
        e: &MessageEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let MessageEvent::Action(action) = e;
        crate::log::info(&format!("agent panel: {action:?}"));
        match action {
            ErrorAction::Retry | ErrorAction::Continue => {
                if let Err(e) = self.chat.update(cx, |c, cx| c.retry(cx)) {
                    self.chat.update(cx, |c, cx| c.notice(e.to_string(), cx));
                }
                // The row and its button go away with the run: focus left on
                // the button would leave no element focused, and every key
                // (Ctrl+Q, Ctrl+L…) dead until a click.
                self.focus_composer(window, cx);
            }
            ErrorAction::NewThread => {
                self.new_thread(cx);
                self.focus_composer(window, cx);
            }
            ErrorAction::ManageAccounts => window.dispatch_action(Box::new(OpenAccounts), cx),
            ErrorAction::ChooseModel => self.composer.update(cx, |c, cx| c.open_picker(window, cx)),
        }
    }

    /// A model chosen after an error row asked for one: the row offers to
    /// try again with it.
    fn on_composer_event(&mut self, _: Entity<Composer>, e: &ComposerEvent, cx: &mut Context<Self>) {
        let ComposerEvent::ModelChosen(name) = e;
        let text = format!("Now using {name}.");
        self.messages.update(cx, |m, cx| {
            m.update_stream(
                |s| {
                    if s.failure().is_some_and(|f| f.action == Some(ErrorAction::ChooseModel)) {
                        s.fail_with(text, Some(ErrorAction::Retry));
                    }
                },
                cx,
            )
        });
    }

    fn on_chat_event(&mut self, _: &Entity<Chat>, e: &ChatEvent, _: &mut Window, cx: &mut Context<Self>) {
        match e {
            // The composer handles the draft (MessageSaved, Withdrawn).
            ChatEvent::Asking if !self.is_open(cx) => {
                self.say_asking(cx);
            }
            ChatEvent::Ended { say: Some(text) } if !self.is_open(cx) => {
                cx.emit(AgentPanelEvent::Say(say_closed(text)));
            }
            _ => {}
        }
    }

    fn say_asking(&mut self, cx: &mut Context<Self>) {
        self.said_asking = true;
        cx.emit(AgentPanelEvent::Say(say_asking()));
    }

    fn unsay_asking(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.said_asking) {
            cx.emit(AgentPanelEvent::Unsay(say_asking()));
        }
    }

    /// The script view this panel belongs to (gone once the script closes).
    pub fn script(&self) -> &WeakEntity<ScriptView> {
        &self.script
    }

    /// The open script's `.fdx`, as opened.
    pub fn script_path(&self) -> &Path {
        &self.script_path
    }

    pub fn threads(&self) -> &Entity<ThreadList> {
        &self.threads
    }

    pub fn messages(&self) -> &Entity<MessageView> {
        &self.messages
    }

    pub fn composer(&self) -> &Entity<Composer> {
        &self.composer
    }

    pub fn focus_composer(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |c, cx| c.focus(window, cx));
    }

    /// Whether focus is anywhere in the panel.
    pub fn has_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    /// Whether the composer's text field has focus.
    pub fn composer_focused(&self, window: &Window, cx: &App) -> bool {
        self.composer.read(cx).is_focused(window, cx)
    }

    /// The panel's focus stops for Tab, in order: the composer's (field,
    /// card buttons, Send/Stop, model chip), the messages', the thread list.
    fn tab_stops(&self, cx: &App) -> Vec<FocusHandle> {
        let mut stops = self.composer.read(cx).tab_stops(cx);
        stops.extend(self.messages.read(cx).tab_stops());
        stops.push(self.threads.read(cx).focus_handle(cx));
        stops
    }

    fn cycle_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let stops = self.tab_stops(cx);
        let at = stops.iter().position(|f| f.contains_focused(window, cx)).unwrap_or(0);
        let n = stops.len();
        let next = if forward { (at + 1) % n } else { (at + n - 1) % n };
        window.focus(&stops[next], cx);
        cx.notify();
    }

    /// Escape: closes what the composer has open (the model picker or
    /// field, the disclosure card), else gives focus back to the script.
    fn on_return_focus(&mut self, _: &ReturnFocus, window: &mut Window, cx: &mut Context<Self>) {
        if !self.composer.update(cx, |c, cx| c.dismiss(window, cx)) {
            cx.emit(AgentPanelEvent::ReturnFocus);
        }
    }

    /// Enter in the composer.
    fn on_send(&mut self, _: &Send, window: &mut Window, cx: &mut Context<Self>) {
        crate::log::info("agent panel: send");
        self.composer.update(cx, |c, cx| c.send(window, cx));
    }

    fn on_stop(&mut self, _: &Stop, _: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |c, cx| c.stop(cx));
    }

    fn on_new_thread(&mut self, _: &NewThread, _: &mut Window, cx: &mut Context<Self>) {
        self.new_thread(cx);
    }

    fn new_thread(&mut self, cx: &mut Context<Self>) {
        // The composer's cards go with the old thread (ChatEvent::ThreadChanged).
        if let Err(e) = self.chat.update(cx, |c, cx| c.new_thread(cx)) {
            self.chat.update(cx, |c, cx| c.notice(e, cx));
        }
    }

    /// Edit › Undo/Redo with the composer focused undo the composer's text,
    /// not the script. Keys never get here (gpui-base's `Input` binding is
    /// deeper); menus do. A menu item dispatches on the focused element: the
    /// macOS menu bar leaves focus where it was, and the Linux File dropdown
    /// gives focus back to what had it before dispatching
    /// (`Workspace::close_menu`), so "the composer is focused" is right for both.
    fn undo_menu(&mut self, redo: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.composer_focused(window, cx) {
            cx.propagate();
            return;
        }
        crate::log::info(&format!("agent panel: menu {} in the composer", if redo { "redo" } else { "undo" }));
        if redo {
            window.dispatch_action(Box::new(gpui_base::input::Redo), cx);
        } else {
            window.dispatch_action(Box::new(gpui_base::input::Undo), cx);
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .flex_none()
            .h(px(38.))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(RULE)
            .child(self.threads.clone())
            .child(div().text_size(px(11.)).text_color(INK_3).child(crate::menus::shortcut("l")))
            .child(
                div()
                    .id("agent-close")
                    .px_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_color(INK_2)
                    .hover(|s| s.bg(ACCENT_SOFT))
                    .child("×")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AgentPanelEvent::Close))),
            )
    }
}

impl Focusable for AgentPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AgentPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("agent-panel")
            .key_context("AgentPanel")
            .track_focus(&self.focus)
            .flex_none()
            .w(px(self.width))
            .h_full()
            .flex()
            .flex_col()
            .bg(SIDE)
            .border_l_1()
            .border_color(RULE)
            .font_family(panel_font(cx))
            .text_size(px(13.))
            .on_action(cx.listener(Self::on_return_focus))
            .on_action(cx.listener(Self::on_send))
            .on_action(cx.listener(Self::on_stop))
            .on_action(cx.listener(Self::on_new_thread))
            .on_action(cx.listener(|this, _: &threads::OpenThreads, window, cx| {
                this.threads.update(cx, |t, cx| t.open(window, cx))
            }))
            .on_action(cx.listener(|this, _: &FocusNext, window, cx| this.cycle_focus(true, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPrev, window, cx| this.cycle_focus(false, window, cx)))
            .on_action(cx.listener(|this, _: &UndoAny, window, cx| this.undo_menu(false, window, cx)))
            .on_action(cx.listener(|this, _: &RedoAny, window, cx| this.undo_menu(true, window, cx)))
            .child(self.render_header(cx))
            .children(replaying().map(|cassette| {
                div()
                    .id("agent-replay-notice")
                    .flex_none()
                    .px_3()
                    .py_1()
                    .border_b_1()
                    .border_color(RULE)
                    .bg(ACCENT_SOFT)
                    .text_size(px(11.))
                    .text_color(INK_2)
                    .child(format!("{REPLAY_NOTICE} ({})", file_label(&cassette)))
            }))
            .child(div().flex_1().min_h_0().child(self.messages.clone()))
            .child(self.composer.clone())
    }
}

/// A path's file name, for the replay strip.
fn file_label(path: &str) -> String {
    Path::new(path).file_name().map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
}

/// The status bar's line when the run asks something (the whole-script
/// read, a disclosure) while the panel is closed: the run waits until the
/// writer opens the panel and answers the card.
fn say_asking() -> SharedString {
    format!("The agent has a question \u{2014} open the chat ({}).", crate::menus::shortcut("l")).into()
}

/// The status bar line for `text` while the panel is closed.
fn say_closed(text: &str) -> SharedString {
    format!("{text} {} to see.", crate::menus::shortcut("l")).into()
}

/// The one adapter from the driver's [`ViewOp`]s to the message view's API.
fn apply_view_ops(m: &mut MessageView, ops: Vec<ViewOp>, cx: &mut Context<MessageView>) {
    for op in ops {
        match op {
            ViewOp::SetThread { id, thread } => m.set_thread(&id, thread, cx),
            ViewOp::PushTurn(t) => m.push_turn(t, cx),
            ViewOp::StreamEvents(events) => m.stream_events(events, cx),
            ViewOp::Stream(op) => m.update_stream(|s| op.apply(s), cx),
            ViewOp::RunActive(active) => m.set_run_active(active, cx),
            ViewOp::Review(r) => m.set_review(r, cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_minimum_window_fits_scaled_and_half_screens() {
        let (w, h) = window_min_size();
        // Half of 1920 px; 1366 × 768 at 125 %; 1920 × 1080 at 200 %.
        assert!(w <= 900. && h <= 540., "{w} × {h}");
    }

    /// Nothing is cut off: the columns showing add up to the window at most.
    fn fits(width: f32, c: Columns) -> bool {
        let side = if c.rail { RAIL_WIDTH } else { SIDEBAR_WIDTH };
        let script = match (c.script, c.left) {
            (_, true) => crate::layout::two_columns_min(),
            (true, false) => script_min(),
            (false, false) => 0.,
        };
        side + c.card + c.panel + script <= width + 0.01
    }

    #[test]
    fn columns_keep_the_full_look_on_wide_windows() {
        for (open, card) in [(true, true), (true, false), (false, true), (false, false)] {
            let c = columns(1500., open, card, false, false);
            assert_eq!(c.panel, if open { WIDTH } else { 0. });
            assert_eq!(c.card, if card { CARD_WIDTH } else { 0. });
            assert!(c.script);
            // The sidebar as before: a rail only with the panel and card open.
            assert_eq!(c.rail, open && card);
        }
        assert!(!columns(1200., false, true, false, false).rail);
        // Everything at full width from rail + card + panel + the readable
        // column (about 1208 px) up.
        let full = RAIL_WIDTH + CARD_WIDTH + WIDTH + script_min();
        assert!(full < 1210., "{full}");
        assert_eq!(columns(1210., true, true, false, false).panel, WIDTH);
    }

    #[test]
    fn columns_narrow_panel_then_card_then_give_way() {
        // Rail + card + panel at full + the narrowest script: 44+380+360+347.
        let full = RAIL_WIDTH + CARD_WIDTH + WIDTH + script_min();
        let c = columns(full - 40., true, true, false, false);
        assert_eq!((c.card, c.panel, c.script), (CARD_WIDTH, WIDTH - 40., true));
        let c = columns(full - (WIDTH - PANEL_MIN) - 30., true, true, false, false);
        assert_eq!((c.card, c.panel, c.script), (CARD_WIDTH - 30., PANEL_MIN, true));
        // Below the narrowest card and panel: the script column gives way.
        let c = columns(900., true, true, false, false);
        assert!(!c.script && c.panel == PANEL_MIN && c.card >= CARD_MIN, "{c:?}");
        let c = columns(1100., true, true, false, false);
        assert!(c.script && c.card >= CARD_MIN && c.panel >= PANEL_MIN, "{c:?}");
        // Rewriting or typing in the script: the card gives way instead.
        let c = columns(900., true, true, true, false);
        assert!(c.script && c.card_hidden(true) && c.panel == PANEL_MIN, "{c:?}");
        // With room for everything, keeping the script changes nothing.
        assert_eq!(columns(1100., true, true, true, false), columns(1100., true, true, false, false));
        // Every width from the minimum up fits, whatever is open.
        let (min, _) = window_min_size();
        let mut w = min;
        while w < 2000. {
            for (open, card, keep) in [
                (true, true, false),
                (true, true, true),
                (true, false, false),
                (false, true, false),
                (false, true, true),
                (false, false, false),
            ] {
                let c = columns(w, open, card, keep, false);
                assert!(fits(w, c), "{w} {open} {card}: {c:?}");
                assert!(c.script || (card && !keep), "the script only gives way to a card, never while kept");
                // Whenever the script shows, it is at a readable zoom.
                if c.script {
                    let side = if c.rail { RAIL_WIDTH } else { SIDEBAR_WIDTH };
                    let z = crate::layout::zoom_for(w - side - c.card - c.panel);
                    assert!(z >= crate::layout::READABLE_ZOOM - 1e-3, "{w}: zoom {z}");
                }
            }
            w += 7.;
        }
    }

    #[test]
    fn lined_up_on_a_13_inch_macbook_air() {
        // About 1,470 px: chat closed, both columns show (rail and card),
        // at about 0.75 zoom.
        let c = columns(1470., false, true, false, true);
        assert!(c.left && c.script && c.rail && c.card == CARD_WIDTH, "{c:?}");
        let z = crate::layout::zoom_for(crate::layout::two_columns(1470. - RAIL_WIDTH - CARD_WIDTH).right_w);
        assert!((0.72..0.78).contains(&z), "{z}");
        // Chat open: even the narrowest panel and card leave too little, so
        // the English gives way; the script, panel and card show in full.
        let c = columns(1470., true, true, false, true);
        assert!(!c.left && c.script && c.left_hidden(true), "{c:?}");
        assert_eq!((c.panel, c.card), (WIDTH, CARD_WIDTH));
    }

    #[test]
    fn lined_up_at_1920_and_1100() {
        // 1,920 px: everything; with the chat open the sidebar is a rail,
        // as for one column, so the two columns keep a comfortable zoom.
        for open in [false, true] {
            let c = columns(1920., open, true, false, true);
            assert!(c.left && c.rail == open && c.card == CARD_WIDTH, "{open}: {c:?}");
            assert_eq!(c.panel, if open { WIDTH } else { 0. });
        }
        // 1,100 px, chat closed: both columns beside the rail without a
        // card; with the card the left gives way.
        let c = columns(1100., false, false, false, true);
        assert!(c.left && c.rail, "{c:?}");
        let c = columns(1100., false, true, false, true);
        assert!(!c.left && c.script && c.card == CARD_WIDTH, "{c:?}");
        // Chat open: the left gives way, then as with nothing lined up.
        let c = columns(1100., true, true, false, true);
        assert!(!c.left && c.rail, "{c:?}");
        assert_eq!(Columns { left: false, ..c }, columns(1100., true, true, false, false));
    }

    #[test]
    fn the_left_gives_way_after_the_panel_and_card_narrow() {
        // Rail + narrowest panel + narrowest card + two readable columns.
        let both = RAIL_WIDTH + PANEL_MIN + CARD_MIN + crate::layout::two_columns_min();
        let c = columns(both + 1., true, true, false, true);
        assert!(c.left && c.panel == PANEL_MIN && c.card < CARD_MIN + 2., "{c:?}");
        let c = columns(both - 1., true, true, false, true);
        assert!(!c.left && c.script, "{c:?}");
        // A little wider: the panel narrows before anything else.
        let c = columns(both + (CARD_WIDTH - CARD_MIN) + 20., true, true, false, true);
        assert!(c.left && c.card == CARD_WIDTH && c.panel == PANEL_MIN + 20., "{c:?}");
        // From the minimum window up: the left only shows with room for two
        // readable columns, gives way before the card or the script, and
        // nothing is cut off.
        let (min, _) = window_min_size();
        let mut w = min;
        while w < 2400. {
            for (open, card, keep) in [
                (true, true, false),
                (true, true, true),
                (true, false, false),
                (false, true, false),
                (false, false, false),
            ] {
                let c = columns(w, open, card, keep, true);
                assert!(fits(w, c), "{w} {open} {card}: {c:?}");
                if !c.script || c.card_hidden(card) {
                    assert!(!c.left, "the left gives way first: {w} {c:?}");
                }
                if c.left {
                    let side = if c.rail { RAIL_WIDTH } else { SIDEBAR_WIDTH };
                    let area = w - side - c.card - c.panel;
                    let z = crate::layout::zoom_for(crate::layout::two_columns(area).right_w);
                    assert!(z >= crate::layout::READABLE_ZOOM - 1e-3, "{w}: zoom {z}");
                }
                // Not lined up, nothing changes but the left.
                let alone = columns(w, open, card, keep, false);
                if !c.left {
                    assert!(alone.script == c.script && alone.panel == c.panel, "{w}: {c:?} {alone:?}");
                }
            }
            w += 7.;
        }
    }

    #[test]
    fn sidebar_collapses_when_the_editor_would_be_too_narrow() {
        // Review view: sidebar, editor, card, panel.
        let rail = |open, card, w| columns(w, open, card, false, false).rail;
        assert!(rail(true, true, 1500.));
        assert!(!rail(true, true, 1700.));
        // The script view has no card: room to spare at 1500.
        assert!(!rail(true, false, 1500.));
        assert!(rail(true, false, 1100.));
        assert!(!rail(false, true, 1100.));
    }
}
