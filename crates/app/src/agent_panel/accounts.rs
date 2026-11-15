//! The AI accounts sheet (placeholder; U8 builds the list, add, remove and
//! check here). The workspace opens it on [`super::OpenAccounts`] and draws
//! it over the whole window, with or without a script; it closes on Escape,
//! a click outside, or [`AccountsSheet::close`], and the workspace then
//! gives focus back to what had it.
//!
//! Keep the API (`new`, `Focusable` for where focus goes on open, the
//! [`Dismissed`] event) when filling it in.

use gpui::{App, Context, EventEmitter, FocusHandle, Focusable, FontWeight, Window, div, prelude::*, px, rgba};
use gpui_base::Sheet;

use crate::theme::*;

/// The sheet asks to be closed.
pub struct Dismissed;

pub struct AccountsSheet {
    focus: FocusHandle,
}

impl EventEmitter<Dismissed> for AccountsSheet {}

impl AccountsSheet {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self { focus: cx.focus_handle() }
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(Dismissed);
    }
}

impl Focusable for AccountsSheet {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AccountsSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        Sheet::new(cx)
            .focus_handle(self.focus.clone())
            .request_close(move |_, cx| {
                this.update(cx, |s, cx| s.close(cx)).ok();
            })
            .overlay(div().absolute().inset_0().bg(rgba(0x1c1f2455)))
            .surface(
                div()
                    .absolute()
                    .right_0()
                    .top_0()
                    .h_full()
                    .w(px(420.))
                    .occlude()
                    .bg(PAPER)
                    .border_l_1()
                    .border_color(RULE)
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .font_family(super::panel_font(cx))
                    .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child("AI accounts"))
                    .child(div().text_color(INK_2).child(
                        "The accounts the agent uses: a provider and an API key, kept in the system keychain. \
                         Adding and checking accounts arrives in a later build.",
                    ))
                    .child(div().text_size(px(12.)).text_color(INK_3).child("Esc to close")),
            )
    }
}
