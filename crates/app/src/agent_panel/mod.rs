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
//!   undo route to; it observes [`accounts::AccountsChanged`] for the chip.
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

use crate::script_view::{RedoAny, ScriptView, UndoAny};
use crate::theme::*;
use composer::Composer;
use driver::{Chat, ChatEvent, SendError, ViewOp};
use messages::MessageView;
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

/// The narrowest the editor gets before the sidebar gives way (the page at
/// about 70 % zoom; its fit-to-width zoom absorbs the rest).
pub const EDITOR_MIN: f32 = 600.;

/// Whether the sidebar shows as a rail: the panel is open and the editor
/// would be narrower than [`EDITOR_MIN`] beside the columns showing (the
/// card shows only in a review view).
pub fn sidebar_collapsed(panel_open: bool, card: bool, window_width: f32) -> bool {
    let columns = SIDEBAR_WIDTH + WIDTH + if card { CARD_WIDTH } else { 0. };
    panel_open && window_width - columns < EDITOR_MIN
}

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
    #[cfg(target_os = "macos")]
    let font = ".SystemUIFont";
    #[cfg(not(target_os = "macos"))]
    let font = load_bundled_font(cx);
    crate::log::info(&format!("agent panel font: {font}"));
    cx.set_global(PanelFont(font.into()));
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
    let mut b = vec![
        KeyBinding::new(sec!("l"), ToggleAgentPanel, Some("Workspace")),
        KeyBinding::new("escape", ReturnFocus, panel),
        KeyBinding::new(sec!("."), Stop, panel),
        KeyBinding::new(sec!("n"), NewThread, panel),
        KeyBinding::new("tab", FocusNext, panel),
        KeyBinding::new("shift-tab", FocusPrev, panel),
        KeyBinding::new("enter", Send, composer),
        KeyBinding::new(sec!("enter"), Send, composer),
        KeyBinding::new("escape", ReturnFocus, composer),
        KeyBinding::new("tab", FocusNext, composer),
        KeyBinding::new("shift-tab", FocusPrev, composer),
        // gpui-base: Cmd/Ctrl+. is ToggleCodeActions inside `Input`.
        KeyBinding::new(sec!("."), Stop, composer),
        // gpui-base's search and replace have no UI here.
        KeyBinding::new(sec!("f"), gpui::NoAction, composer),
        KeyBinding::new(if cfg!(target_os = "macos") { "cmd-shift-f" } else { "ctrl-h" }, gpui::NoAction, composer),
    ];
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
    /// A disclosure shown in place of sending: the next Send accepts it.
    /// (Stand-in until U6's disclosure card.)
    disclosure: Option<String>,
    _chat: [gpui::Subscription; 2],
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
            let ops = chat.update(cx, |c, _| c.take_view_ops());
            this.messages.update(cx, |m, cx| apply_view_ops(m, ops, cx));
            cx.notify();
        });
        let events = cx.subscribe_in(&chat, window, Self::on_chat_event);
        Self {
            focus: cx.focus_handle(),
            script: script.clone(),
            script_path,
            threads: cx.new(|cx| ThreadList::new(window, cx)),
            messages,
            composer: cx.new(|cx| Composer::new(window, cx)),
            chat,
            disclosure: None,
            _chat: [observe, events],
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

    fn on_chat_event(&mut self, _: &Entity<Chat>, e: &ChatEvent, window: &mut Window, cx: &mut Context<Self>) {
        match e {
            ChatEvent::MessageSaved => {
                // Stand-in until U6: the draft goes once the message is saved.
                let input = self.composer.read(cx).input().clone();
                input.update(cx, |s, cx| s.set_value("", window, cx));
            }
            ChatEvent::Withdrawn(text) => {
                let input = self.composer.read(cx).input().clone();
                let text = text.clone();
                input.update(cx, |s, cx| s.set_value(text, window, cx));
            }
            ChatEvent::Asking if !self.is_open(cx) => {
                cx.emit(AgentPanelEvent::Say(say_closed("The agent is waiting for you.")));
            }
            ChatEvent::Ended { say: Some(text) } if !self.is_open(cx) => {
                cx.emit(AgentPanelEvent::Say(say_closed(text)));
            }
            _ => {}
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

    /// The panel's focus stops for Tab, in order. U6 and U7 add Send/Stop,
    /// the model chip and the thread list's own stops.
    fn tab_stops(&self, cx: &App) -> Vec<FocusHandle> {
        let mut stops = vec![self.composer.read(cx).focus_handle(cx)];
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

    fn on_return_focus(&mut self, _: &ReturnFocus, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(AgentPanelEvent::ReturnFocus);
    }

    /// Enter: send the draft. A stand-in for U6's composer: a disclosure is
    /// shown as a line and the next Enter accepts it; a whole-script question
    /// is answered by Enter (yes) or Stop.
    fn on_send(&mut self, _: &Send, _: &mut Window, cx: &mut Context<Self>) {
        crate::log::info("agent panel: send");
        if self.chat.read(cx).question().is_some() {
            self.chat.update(cx, |c, cx| c.answer(true, cx));
            return;
        }
        let text = self.composer.read(cx).text(cx);
        if let Some(key) = self.disclosure.take()
            && let Err(e) = self.chat.update(cx, |c, _| c.accept_disclosure(&key))
        {
            crate::log::warn(&format!("couldn't remember the disclosure: {e}"));
        }
        let sent = self.chat.update(cx, |c, cx| c.send(&text, cx));
        if let Err(e) = &sent {
            crate::log::info(&format!("agent panel: not sent: {e:?}"));
        }
        match sent {
            Ok(()) | Err(SendError::Blank) => {}
            Err(SendError::Disclose { key, text }) => {
                self.disclosure = Some(key);
                self.chat.update(cx, |c, cx| c.notice(format!("{text} Press Enter to send."), cx));
            }
            Err(e) => self.chat.update(cx, |c, cx| c.notice(e.to_string(), cx)),
        }
    }

    fn on_stop(&mut self, _: &Stop, _: &mut Window, cx: &mut Context<Self>) {
        self.chat.update(cx, |c, cx| {
            if c.question().is_some() && !c.is_running() {
                c.answer(false, cx);
            }
            c.stop(cx);
        });
    }

    fn on_new_thread(&mut self, _: &NewThread, _: &mut Window, cx: &mut Context<Self>) {
        self.disclosure = None;
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

    /// The run's notices and question, above the composer (a stand-in for
    /// U6's notices and question card).
    fn render_notices(&self, cx: &App) -> Option<impl IntoElement + use<>> {
        let lines = extra_lines(self.chat.read(cx));
        (!lines.is_empty()).then(|| {
            div()
                .id("agent-notices")
                .flex_none()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(RULE)
                .flex()
                .flex_col()
                .gap_1()
                .text_size(px(12.))
                .text_color(INK_2)
                .children(lines.into_iter().map(|l| div().child(l)))
        })
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
            .w(px(WIDTH))
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
            .on_action(cx.listener(|this, _: &FocusNext, window, cx| this.cycle_focus(true, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPrev, window, cx| this.cycle_focus(false, window, cx)))
            .on_action(cx.listener(|this, _: &UndoAny, window, cx| this.undo_menu(false, window, cx)))
            .on_action(cx.listener(|this, _: &RedoAny, window, cx| this.undo_menu(true, window, cx)))
            .child(self.render_header(cx))
            .child(div().flex_1().min_h_0().child(self.messages.clone()))
            .children(self.render_notices(cx))
            .child(self.composer.clone())
    }
}

/// The status bar line for `text` while the panel is closed.
fn say_closed(text: &str) -> SharedString {
    format!("{text} {} to see.", crate::menus::shortcut("l")).into()
}

/// The driver's lines the message view shows after the rows (stand-in for
/// U6's notices and question card).
fn extra_lines(c: &Chat) -> Vec<String> {
    let mut lines = c.notices().to_vec();
    if let Some(q) = c.question() {
        lines.push(format!("{} Enter: send · {}: not now", q.text(), crate::menus::shortcut(".")));
    }
    lines
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
    fn sidebar_collapses_when_the_editor_would_be_too_narrow() {
        // Review view: sidebar, editor, card, panel.
        assert!(sidebar_collapsed(true, true, 1500.));
        assert!(!sidebar_collapsed(true, true, 1700.));
        // The script view has no card: room to spare at 1500.
        assert!(!sidebar_collapsed(true, false, 1500.));
        assert!(sidebar_collapsed(true, false, 1100.));
        assert!(!sidebar_collapsed(false, true, 1000.));
    }
}
