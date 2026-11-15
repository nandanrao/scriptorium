//! The message list: the panel's middle section, between the header and the
//! composer. Data in only (U3 draws it properly): the panel hands it a
//! [`Thread`] and stream changes through `AgentPanel::apply_view_ops`, the one
//! adapter between the driver and this view.
//!
//! Until U3's view lands this draws the rows as plain lines, so runs can be
//! seen and tested; its data API is the one U3's view has.

use gpui::{Context, FontWeight, ScrollHandle, WeakEntity, Window, div, prelude::*};
use scriptorium_agent::{Event, Thread};

use super::driver::ReviewRef;
use super::view_model::{self as vm, Kind, Row, RowCache, Stream};
use crate::script_view::ScriptView;
use crate::theme::*;

pub struct MessageView {
    #[allow(dead_code)] // U3: the suggestion links.
    script: WeakEntity<ScriptView>,
    thread: Thread,
    stream: Stream,
    cache: RowCache,
    run_active: bool,
    review: Option<ReviewRef>,
    rows: Vec<Row>,
    /// Lines from the driver (notices, a question) shown after the rows.
    extra: Vec<String>,
    scroll: ScrollHandle,
}

impl MessageView {
    pub fn new(script: WeakEntity<ScriptView>, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            script,
            thread: Thread::default(),
            stream: Stream::new(),
            cache: RowCache::default(),
            run_active: false,
            review: None,
            rows: vec![],
            extra: vec![],
            scroll: ScrollHandle::new(),
        }
    }

    /// Shows `thread` (another one, or the same one grown).
    pub fn set_thread(&mut self, thread: Thread, cx: &mut Context<Self>) {
        self.thread = thread;
        self.rebuild(cx);
    }

    /// A step's thread: replaces the streaming rows (clears the stream).
    pub fn push_turn(&mut self, thread: Thread, cx: &mut Context<Self>) {
        self.stream.clear();
        self.thread = thread;
        self.rebuild(cx);
    }

    /// A frame's worth of reply events.
    pub fn stream_events(&mut self, events: Vec<Event>, cx: &mut Context<Self>) {
        for e in events {
            self.stream.event(e);
        }
        self.rebuild(cx);
    }

    /// Any other change to the stream: `wait`, `fail`, `stop`, `clear`.
    pub fn update_stream(&mut self, f: impl FnOnce(&mut Stream), cx: &mut Context<Self>) {
        f(&mut self.stream);
        self.rebuild(cx);
    }

    pub fn set_run_active(&mut self, active: bool, cx: &mut Context<Self>) {
        self.run_active = active;
        self.rebuild(cx);
    }

    pub fn set_review(&mut self, review: Option<ReviewRef>, cx: &mut Context<Self>) {
        self.review = review;
        self.rebuild(cx);
    }

    /// Lines after the rows (stand-in for U6's notices and question card).
    pub fn set_extra(&mut self, extra: Vec<String>, cx: &mut Context<Self>) {
        if self.extra != extra {
            self.extra = extra;
            self.scroll.scroll_to_bottom();
            cx.notify();
        }
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let ctx = vm::Context { active: self.run_active, review: self.review.as_ref().map(|r| r.name.as_str()) };
        self.rows = self.cache.rows(&self.thread, &self.stream, ctx);
        self.scroll.scroll_to_bottom();
        cx.notify();
    }
}

fn row_text(row: &Row) -> (String, bool) {
    match &row.kind {
        Kind::User { text } => (format!("You: {text}"), true),
        Kind::Text { text, .. } => (text.to_string(), false),
        Kind::Reasoning { hidden, .. } => (Row::reasoning_title(*hidden).to_string(), false),
        Kind::Tool(t) => (format!("▸ {}", t.label), false),
        Kind::Notice { text } => (text.clone(), false),
        Kind::Error { text, retryable } => (format!("{text}{}", if *retryable { " [Retry]" } else { "" }), false),
        Kind::Waiting { secs } => (format!("Busy. Retrying in {secs} s."), false),
    }
}

impl Render for MessageView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let list = div()
            .id("agent-messages")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p_4()
            .flex()
            .flex_col()
            .gap_3();
        if self.rows.is_empty() && self.extra.is_empty() {
            return list.child(
                div()
                    .text_color(INK_2)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().font_weight(FontWeight::BOLD).child("Chat about the script."))
                    .child(div().child(
                        "The agent reads the script and proposes changes into this conversation's own review. \
                         You decide them on the review card, as with any review.",
                    )),
            );
        }
        list.children(self.rows.iter().map(|r| {
            let (text, user) = row_text(r);
            let muted = matches!(r.kind, Kind::Tool(_) | Kind::Notice { .. } | Kind::Reasoning { .. });
            div()
                .when(user, |d| d.font_weight(FontWeight::SEMIBOLD))
                .when(muted, |d| d.text_color(INK_2))
                .when(matches!(r.kind, Kind::Error { .. }), |d| d.text_color(WARN))
                .child(text)
        }))
        .children(self.extra.iter().map(|t| div().italic().text_color(INK_3).child(t.clone())))
    }
}
