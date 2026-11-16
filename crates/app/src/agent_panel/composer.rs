//! The composer (U6 fills it in: Send/Stop, the model chip, usage): the
//! text field at the foot of the panel. Its key context, `AgentComposer`, is
//! what the panel's composer key bindings match (`super::bindings`); keep it
//! on the element that holds the textarea.

use gpui::{App, Context, Entity, FocusHandle, Focusable, Subscription, Window, div, prelude::*, px};
use gpui_base::Textarea;
use gpui_base::input::TextareaState;

use super::accounts::AccountsChanged;
use crate::theme::*;

pub struct Composer {
    input: Entity<TextareaState>,
    /// Redraws when the accounts change (the model chip shows them).
    _accounts: Subscription,
}

impl Composer {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx).auto_grow(1, 10).placeholder("Ask about the script, or ask for changes…")
        });
        let accounts = cx.observe_global::<AccountsChanged>(|_, cx| cx.notify());
        Self { input, _accounts: accounts }
    }

    /// The text field: focus and undo route to it.
    pub fn input(&self) -> &Entity<TextareaState> {
        &self.input
    }

    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |c, cx| c.focus(window, cx));
    }

    pub fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle(cx).contains_focused(window, cx)
    }

    /// The draft as typed.
    pub fn text(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.is_focused(window, cx);
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
                    .child(Textarea::new(&self.input)),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(INK_3)
                    .child("Enter send · Shift+Enter new line · Esc back to the script"),
            )
    }
}
