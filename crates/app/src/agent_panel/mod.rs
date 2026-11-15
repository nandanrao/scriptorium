//! The agent panel: a fixed 360 px column at the right of the open script
//! (after the review card) where the writer chats with an agent. See
//! docs/plans/chat-panel.md.
//!
//! This module is the shell: the `AgentPanel` entity, its actions and key
//! bindings, focus and undo routing. The parts come from their own files:
//!
//! - `view_model.rs` (pure): a thread plus the streaming buffer become rows.
//! - `messages.rs` (U3): the message list, drawn in [`AgentPanel::render`]'s
//!   middle section (`render_messages`).
//! - `composer.rs` (U6): the composer below it (`render_composer`), keeping
//!   [`AgentPanel::composer`] as the one textarea focus and undo route to.
//! - `driver.rs` (U4) and `live.rs` (U5): runs, over the hooks ScriptView
//!   hands out (`hooks.rs`; `ScriptView::agent_ctx`, `agent_review`,
//!   `agent_write_review`, `show_suggestion`).
//! - `threads.rs` (U7): the thread list behind the header's title.
//! - `accounts.rs` (U8): the AI accounts sheet, which the workspace draws over
//!   the whole window ([`OpenAccounts`]), so it opens with no script too.
//!
//! The panel belongs to the open script (`ScriptView` owns it and keeps it
//! while closed, so a draft and a run survive Ctrl+L); ScriptView decides
//! where focus goes when the panel gives it back ([`AgentPanelEvent`]).
//! Focus never moves into the panel on its own.

pub mod accounts;
pub mod hooks;
mod view_model;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Global, KeyBinding, SharedString, Window,
    actions, div, prelude::*, px,
};
use gpui_base::Textarea;
use gpui_base::input::TextareaState;

use crate::script_view::{RedoAny, UndoAny};
use crate::theme::*;

actions!(
    agent,
    [
        /// Ctrl/Cmd+L: open the panel and focus the composer; with the panel
        /// focused, close it and give focus back to the script.
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

/// Below this window width, the sidebar collapses to a rail while the panel
/// is open (the editor's fit-to-width zoom absorbs the rest).
pub const RAIL_BELOW: f32 = 1600.;

/// Whether the sidebar shows as a rail.
pub fn sidebar_collapsed(panel_open: bool, window_width: f32) -> bool {
    panel_open && window_width < RAIL_BELOW
}

/// Cmd on macOS, Ctrl elsewhere (as in main.rs).
macro_rules! sec {
    ($k:literal) => {
        if cfg!(target_os = "macos") { concat!("cmd-", $k) } else { concat!("ctrl-", $k) }
    };
}

/// Registers gpui-base (its `Input`, `Select`, `Sheet` bindings) and picks
/// the panel's font. Call before binding the app's keys, so ours, added
/// after, win where both match at the same depth.
pub fn init(cx: &mut App) {
    gpui_base::init(cx);
    let names = cx.text_system().all_font_names();
    let font = pick_font(&names);
    crate::log::info(&format!("agent panel font: {font}"));
    cx.set_global(PanelFont(font.into()));
}

/// The system UI font on Linux (often Adwaita Sans, a variable font) shows
/// no bold, which markdown needs: GPUI draws a variable font's default
/// instance only. Take the first installed family that ships static bold and
/// italic faces.
fn pick_font(installed: &[String]) -> &'static str {
    const CANDIDATES: &[&str] = if cfg!(target_os = "macos") {
        &[".SystemUIFont"]
    } else {
        &["Noto Sans", "DejaVu Sans", "Liberation Sans", "Open Sans", "Ubuntu", "Arial"]
    };
    CANDIDATES.iter().copied().find(|c| installed.iter().any(|n| n == c)).unwrap_or(CANDIDATES[0])
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
}

pub struct AgentPanel {
    /// Tracks the whole column, so "focus is in the panel" is one check.
    focus: FocusHandle,
    /// The header's thread title (U7 makes it the thread list's trigger).
    title_focus: FocusHandle,
    composer: Entity<TextareaState>,
}

impl EventEmitter<AgentPanelEvent> for AgentPanel {}

impl AgentPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx).auto_grow(1, 10).placeholder("Ask about the script, or ask for changes…")
        });
        Self { focus: cx.focus_handle(), title_focus: cx.focus_handle(), composer }
    }

    /// The composer's text field.
    #[allow(dead_code)] // The composer and driver (U4, U6).
    pub fn composer(&self) -> &Entity<TextareaState> {
        &self.composer
    }

    pub fn focus_composer(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |c, cx| c.focus(window, cx));
    }

    /// Whether focus is anywhere in the panel.
    pub fn has_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    fn composer_focused(&self, window: &Window, cx: &App) -> bool {
        self.composer.focus_handle(cx).contains_focused(window, cx)
    }

    /// The panel's focus stops for Tab, in order. U6 and U7 add Send/Stop,
    /// the model chip and the thread title's real trigger.
    fn tab_stops(&self, cx: &App) -> Vec<FocusHandle> {
        vec![self.composer.focus_handle(cx), self.title_focus.clone()]
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
        let len = self.composer.read(cx).value().chars().count();
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

    fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let title_focused = self.title_focus.is_focused(window);
        div()
            .flex_none()
            .h(px(38.))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(RULE)
            .child(
                div()
                    .id("agent-title")
                    .track_focus(&self.title_focus)
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .px_1()
                    .rounded_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(title_focused, |d| d.bg(ACCENT_SOFT))
                    .child("New conversation"),
            )
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
                    .on_click(cx.listener(|_, _, window, cx| window.dispatch_action(Box::new(ToggleAgentPanel), cx))),
            )
    }

    /// U3 replaces this with the message list.
    fn render_messages(&self) -> impl IntoElement + use<> {
        div().id("agent-messages").flex_1().min_h_0().overflow_y_scroll().p_4().flex().flex_col().gap_3().child(
            div()
                .text_color(INK_2)
                .flex()
                .flex_col()
                .gap_2()
                .child(div().font_weight(FontWeight::BOLD).child("Chat about the script."))
                .child(div().child(
                    "The agent reads the script and proposes changes into this conversation's own review. \
                     You decide them on the review card, as with any review.",
                ))
                .child(
                    div().italic().text_color(INK_3).child("Nothing is sent yet: the agent arrives in a later build."),
                ),
        )
    }

    /// U6 replaces this with the composer (Send/Stop, model chip, usage).
    fn render_composer(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let focused = self.composer_focused(window, cx);
        div()
            .flex_none()
            .p_2()
            .border_t_1()
            .border_color(RULE)
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .id("agent-composer")
                    .key_context("AgentComposer")
                    .p_2()
                    .rounded_md()
                    .bg(PAPER)
                    .border_1()
                    .border_color(if focused { ACCENT } else { RULE })
                    .child(Textarea::new(&self.composer)),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(INK_3)
                    .child("Enter send · Shift+Enter new line · Esc back to the script"),
            )
    }
}

impl Focusable for AgentPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AgentPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .child(self.render_header(window, cx))
            .child(self.render_messages())
            .child(self.render_composer(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_collapses_only_with_the_panel_open_in_a_narrow_window() {
        assert!(sidebar_collapsed(true, 1500.));
        assert!(!sidebar_collapsed(true, 1700.));
        assert!(!sidebar_collapsed(false, 1500.));
    }

    #[test]
    fn panel_font_is_the_first_installed_candidate() {
        if cfg!(target_os = "macos") {
            return;
        }
        let installed = ["Liberation Sans".to_string(), "Noto Sans".to_string()];
        assert_eq!(pick_font(&installed), "Noto Sans");
        assert_eq!(pick_font(&[]), "Noto Sans");
    }
}
