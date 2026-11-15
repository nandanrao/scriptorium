//! The thread list (U7 fills it in): the header's thread title, which opens
//! the list of this script's conversations. A Tab stop in the panel.

use gpui::{App, Context, FocusHandle, FontWeight, Window, div, prelude::*};

use crate::theme::*;

pub struct ThreadList {
    /// The title (the list's trigger).
    focus: FocusHandle,
}

impl ThreadList {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self { focus: cx.focus_handle() }
    }

    pub fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ThreadList {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        div()
            .id("agent-title")
            .track_focus(&self.focus)
            .flex_1()
            .min_w_0()
            .truncate()
            .px_1()
            .rounded_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .when(focused, |d| d.bg(ACCENT_SOFT))
            .child("New conversation")
    }
}
