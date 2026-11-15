//! The message list (U3 fills it in from `view_model`): the panel's middle
//! section, between the header and the composer.

use gpui::{Context, FontWeight, Window, div, prelude::*};

use crate::theme::*;

pub struct MessageView {}

impl MessageView {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {}
    }
}

impl Render for MessageView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().id("agent-messages").size_full().overflow_y_scroll().p_4().flex().flex_col().gap_3().child(
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
}
