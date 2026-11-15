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
    /// A notice for the status bar, e.g. how a run ended while the panel was
    /// closed.
    #[allow(dead_code)] // The driver (U4).
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
        Self {
            focus: cx.focus_handle(),
            script,
            script_path,
            threads: cx.new(|cx| ThreadList::new(window, cx)),
            messages: cx.new(|cx| MessageView::new(window, cx)),
            composer: cx.new(|cx| Composer::new(window, cx)),
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
        vec![self.composer.read(cx).focus_handle(cx), self.threads.read(cx).focus_handle(cx)]
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

    fn on_send(&mut self, _: &Send, _: &mut Window, cx: &mut Context<Self>) {
        // The driver (U4) and composer (U6) send; until then the draft stays.
        let len = self.composer.read(cx).text(cx).chars().count();
        crate::log::info(&format!("agent panel: send ({len} chars; not wired yet)"));
    }

    fn on_stop(&mut self, _: &Stop, _: &mut Window, _: &mut Context<Self>) {
        crate::log::info("agent panel: stop (no run)");
    }

    fn on_new_thread(&mut self, _: &NewThread, _: &mut Window, _: &mut Context<Self>) {
        crate::log::info("agent panel: new thread (not wired yet)");
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
            .child(self.composer.clone())
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
