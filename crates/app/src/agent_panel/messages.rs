//! The message list: the panel's middle section, between the header and the
//! composer. Data in only: the shell hands it a [`Thread`] and feeds it
//! stream events; it turns them into rows ([`view_model`]) and draws them in
//! a bottom-aligned `list()` that follows the tail unless the writer scrolls up.
//!
//! API (all of it is data-in; there is no run logic here):
//! - [`MessageView::set_thread`]: show another thread (or the same one, grown).
//! - [`MessageView::stream_event`], [`MessageView::update_stream`]: the
//!   streaming reply and the run's state lines (wait, fail, stop, clear).
//! - [`MessageView::set_run_active`], [`MessageView::set_review`]: what tool
//!   labels and links need.
//!
//! Each assistant text row keeps one `TextViewState` per [`Key`], so it
//! survives streaming and the moment the pushed turn replaces the streaming
//! rows (same keys). `set_text` with text that extends the old text is a
//! `push_str`, and with equal text a no-op, so there is no flash.

use std::collections::{HashMap, HashSet};

use gpui::{
    AnyElement, Context, Entity, FontWeight, ListAlignment, ListState, Rgba, WeakEntity, Window, div, list, prelude::*,
    px,
};
use gpui_base::text::{TextView, TextViewState};
use scriptorium_agent::{Event, Thread};
use serde_json::Value;

use super::view_model::{self as vm, Key, Kind, Row, RowCache, Stream, ToolRow, ToolState};
use crate::script_view::ScriptView;
use crate::theme::*;

pub struct MessageView {
    script: WeakEntity<ScriptView>,
    thread: Thread,
    stream: Stream,
    cache: RowCache,
    run_active: bool,
    /// The thread's review: its id for the "→ review" link, its name for labels.
    review_id: Option<String>,
    review_name: Option<String>,
    rows: Vec<Row>,
    list: ListState,
    texts: HashMap<Key, Entity<TextViewState>>,
    /// Reasoning and tool rows the writer opened.
    open: HashSet<Key>,
    /// Tool results shown in full ("Show all").
    full: HashSet<Key>,
}

impl MessageView {
    pub fn new(script: WeakEntity<ScriptView>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let list = ListState::new(0, ListAlignment::Bottom, px(600.));
        list.set_follow_mode(gpui::FollowMode::Tail);
        // Repaint when the scroll position changes, for "Jump to latest".
        let id = cx.entity_id();
        list.set_scroll_handler(move |_, _, cx| cx.notify(id));
        let mut this = Self {
            script,
            thread: Thread::default(),
            stream: Stream::new(),
            cache: RowCache::default(),
            run_active: false,
            review_id: None,
            review_name: None,
            rows: Vec::new(),
            list,
            texts: HashMap::new(),
            open: HashSet::new(),
            full: HashSet::new(),
        };
        #[cfg(debug_assertions)]
        this.load_debug_fixture(cx);
        this
    }

    /// Shows `thread`. A thread that only grew keeps its scroll position and
    /// open rows; the caller says `switched` when it is another thread (rows
    /// close, the view jumps to the end).
    #[allow(dead_code)] // The driver (U4).
    pub fn set_thread(&mut self, thread: Thread, switched: bool, cx: &mut Context<Self>) {
        self.thread = thread;
        if switched {
            self.open.clear();
            self.full.clear();
            self.texts.clear();
            self.cache = RowCache::default();
            self.rows.clear();
            self.list.reset(0);
            self.list.set_follow_mode(gpui::FollowMode::Tail);
        }
        self.rebuild(cx);
    }

    /// One reply event for the streaming buffer.
    #[allow(dead_code)] // The driver (U4).
    pub fn stream_event(&mut self, event: Event, cx: &mut Context<Self>) {
        self.update_stream(|s| s.event(event), cx);
    }

    /// Any other change to the stream: `wait`, `fail`, `stop`, `clear`.
    #[allow(dead_code)] // The driver (U4).
    pub fn update_stream(&mut self, f: impl FnOnce(&mut Stream), cx: &mut Context<Self>) {
        f(&mut self.stream);
        self.rebuild(cx);
    }

    /// Whether a run is in progress (a call without a result then reads "running").
    #[allow(dead_code)] // The driver (U4).
    pub fn set_run_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.run_active != active {
            self.run_active = active;
            self.rebuild(cx);
        }
    }

    /// The thread's review: `id` for the suggestion links, `name` for labels.
    #[allow(dead_code)] // The driver (U4).
    pub fn set_review(&mut self, id: Option<String>, name: Option<String>, cx: &mut Context<Self>) {
        self.review_id = id;
        self.review_name = name;
        self.rebuild(cx);
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let ctx = vm::Context { active: self.run_active, review: self.review_name.as_deref() };
        let new = self.cache.rows(&self.thread, &self.stream, ctx);
        let prefix = self.rows.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let (old_len, new_len) = (self.rows.len(), new.len());
        self.rows = new;
        if prefix != old_len || prefix != new_len {
            self.list.splice(prefix..old_len, new_len - prefix);
        }
        self.sync_texts(cx);
        cx.notify();
    }

    fn sync_texts(&mut self, cx: &mut Context<Self>) {
        let mut keep = HashSet::new();
        for row in &self.rows {
            let Kind::Text { text, .. } = &row.kind else { continue };
            keep.insert(row.key.clone());
            let state = self
                .texts
                .entry(row.key.clone())
                .or_insert_with(|| cx.new(|cx| TextViewState::markdown("", cx).selectable(true)))
                .clone();
            state.update(cx, |s, cx| s.set_text(text, cx));
        }
        self.texts.retain(|k, _| keep.contains(k));
        let live: HashSet<&Key> = self.rows.iter().map(|r| &r.key).collect();
        self.open.retain(|k| live.contains(k));
        self.full.retain(|k| live.contains(k));
    }

    /// Re-measure row `ix` after its look changed.
    fn row_changed(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.list.splice(ix..ix + 1, 1);
        cx.notify();
    }

    fn toggle(&mut self, ix: usize, cx: &mut Context<Self>) {
        let key = self.rows[ix].key.clone();
        if !self.open.remove(&key) {
            self.open.insert(key);
        }
        self.row_changed(ix, cx);
    }

    fn show_all(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.full.insert(self.rows[ix].key.clone());
        self.row_changed(ix, cx);
    }

    fn jump_to_latest(&mut self, cx: &mut Context<Self>) {
        self.list.set_follow_mode(gpui::FollowMode::Tail);
        self.list.scroll_to_end();
        cx.notify();
    }

    fn render_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else { return div().into_any_element() };
        let body = match row.kind {
            Kind::User { text } => div()
                .flex()
                .justify_end()
                .child(
                    div()
                        .max_w_full()
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .bg(ACCENT_SOFT)
                        .text_color(INK)
                        .child(text.to_string()),
                )
                .into_any_element(),
            Kind::Text { .. } => match self.texts.get(&row.key) {
                Some(state) => div().text_color(INK).child(TextView::new(state).selectable(true)).into_any_element(),
                None => div().into_any_element(),
            },
            Kind::Reasoning { text, hidden, streaming } => {
                self.render_reasoning(ix, &row.key, &text, hidden, streaming, cx)
            }
            Kind::Tool(tool) => self.render_tool(ix, &row.key, &tool, cx),
            Kind::Notice { text } => div().italic().text_size(px(12.)).text_color(INK_3).child(text).into_any_element(),
            Kind::Error { text, retryable } => div()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(WARN_SOFT)
                .text_color(WARN)
                .flex()
                .flex_col()
                .gap_1()
                .child(div().font_weight(FontWeight::SEMIBOLD).child("The agent stopped"))
                .child(text)
                .when(retryable, |d| d.child(div().text_color(INK_2).child("You can send your message again.")))
                .into_any_element(),
            Kind::Waiting { secs } => div()
                .italic()
                .text_size(px(12.))
                .text_color(INK_3)
                .child(format!("Busy: trying again in {secs}s…"))
                .into_any_element(),
        };
        div().w_full().px_3().py_1p5().child(body).into_any_element()
    }

    fn render_reasoning(
        &self,
        ix: usize,
        key: &Key,
        text: &str,
        hidden: bool,
        streaming: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = !hidden && self.open.contains(key);
        let title = if streaming { "Reasoning…" } else { Row::reasoning_title(hidden) };
        let head = div()
            .id(("reasoning", ix))
            .flex()
            .gap_1()
            .text_size(px(12.))
            .text_color(INK_3)
            .when(!hidden, |d| d.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| this.toggle(ix, cx))))
            .child(if hidden {
                "  "
            } else if open {
                "▾"
            } else {
                "▸"
            })
            .child(div().italic().child(title));
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(head)
            .when(open, |d| {
                d.child(
                    div()
                        .pl_3()
                        .ml_1()
                        .border_l_2()
                        .border_color(RULE)
                        .text_size(px(12.))
                        .text_color(INK_2)
                        .child(text.to_string()),
                )
            })
            .into_any_element()
    }

    fn render_tool(&self, ix: usize, key: &Key, tool: &ToolRow, cx: &mut Context<Self>) -> AnyElement {
        let (icon, color): (&str, Rgba) = match tool.state {
            ToolState::Streaming | ToolState::Running => ("●", ACCENT),
            ToolState::Done => ("✓", OK),
            ToolState::Failed => ("✕", WARN),
            ToolState::NotRun => ("–", INK_3),
        };
        let has_details = !tool.args.is_empty() || tool.result.is_some();
        let open = has_details && self.open.contains(key);
        let link = self.suggestion_link(tool);
        let head = div()
            .id(("tool", ix))
            .flex()
            .items_start()
            .gap_2()
            .when(has_details, |d| d.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| this.toggle(ix, cx))))
            .child(div().flex_none().w(px(12.)).text_color(color).child(icon))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(if tool.state == ToolState::Failed { WARN } else { INK_2 })
                    .child(tool.label.clone()),
            )
            .when(has_details, |d| {
                d.child(div().flex_none().text_color(INK_3).child(if open { "▾" } else { "▸" }))
            });
        let mono = |text: String| {
            div()
                .font_family("monospace")
                .text_size(px(11.))
                .text_color(INK_2)
                .bg(PAPER)
                .border_1()
                .border_color(RULE)
                .rounded_md()
                .p_2()
                .child(text)
        };
        let shown_all = self.full.contains(key);
        div()
            .flex()
            .flex_col()
            .gap_1()
            .text_size(px(12.))
            .child(head)
            .when_some(link, |d, (id, n)| {
                d.child(
                    div().pl(px(20.)).child(
                        div()
                            .id(("tool-link", ix))
                            .cursor_pointer()
                            .text_color(ACCENT)
                            .hover(|s| s.underline())
                            .child(format!("Show suggestion #{n}"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.show_suggestion(&id, n, window, cx);
                            })),
                    ),
                )
            })
            .when(open, |d| {
                d.child(
                    div()
                        .pl(px(20.))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .when(!tool.args.is_empty(), |d| d.child(mono(tool.args.to_string())))
                        .when_some(tool.result.as_ref(), |d, p| {
                            let more = p.hidden > 0 && !shown_all;
                            d.child(mono(if shown_all { p.full() } else { p.shown() }.to_string())).when(more, |d| {
                                d.child(
                                    div()
                                        .id(("show-all", ix))
                                        .cursor_pointer()
                                        .text_color(ACCENT)
                                        .hover(|s| s.underline())
                                        .child(format!("Show all ({} more lines)", p.hidden))
                                        .on_click(cx.listener(move |this, _, _, cx| this.show_all(ix, cx))),
                                )
                            })
                        }),
                )
            })
            .into_any_element()
    }

    /// The review id and first new suggestion number of a finished
    /// `suggest_change(s)` row, when the thread's review is known.
    fn suggestion_link(&self, tool: &ToolRow) -> Option<(String, usize)> {
        if tool.state != ToolState::Done || !matches!(tool.name.as_str(), "suggest_change" | "suggest_changes") {
            return None;
        }
        let n = first_suggestion(tool.result.as_ref()?.full())?;
        Some((self.review_id.clone()?, n))
    }

    fn show_suggestion(&mut self, id: &str, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(script) = self.script.upgrade() else { return };
        let id = id.to_string();
        // Deferred: the click may be dispatched while the script view is on the stack.
        window.defer(cx, move |window, cx| {
            script.update(cx, |sv, cx| sv.show_suggestion(&id, n, window, cx));
        });
    }
}

/// The number of the first suggestion a `suggest_change(s)` result added:
/// `#n` in the sentence, or `suggestions[0].n` in the batch JSON.
fn first_suggestion(result: &str) -> Option<usize> {
    if let Ok(v) = serde_json::from_str::<Value>(result) {
        return v.get("suggestions")?.as_array()?.first()?.get("n")?.as_u64().map(|n| n as usize);
    }
    let rest = &result[result.find('#')? + 1..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

impl Render for MessageView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let away = self.list.is_scrolled_to_end() == Some(false);
        let body = if self.rows.is_empty() {
            div()
                .size_full()
                .p_4()
                .text_color(INK_2)
                .flex()
                .flex_col()
                .gap_2()
                .child(div().font_weight(FontWeight::BOLD).child("Chat about the script."))
                .child(div().child(
                    "The agent reads the script and proposes changes into this conversation's own review. \
                     You decide them on the review card, as with any review.",
                ))
                .into_any_element()
        } else {
            list(self.list.clone(), cx.processor(|this, ix, _, cx| this.render_row(ix, cx)))
                .size_full()
                .py_2()
                .into_any_element()
        };
        div().id("agent-messages").relative().size_full().child(body).when(away, |d| {
            d.child(
                div().absolute().bottom_2().w_full().flex().justify_center().child(
                    div()
                        .id("jump-to-latest")
                        .cursor_pointer()
                        .px_3()
                        .py_1()
                        .rounded_full()
                        .bg(PAPER)
                        .border_1()
                        .border_color(RULE)
                        .shadow_sm()
                        .text_size(px(12.))
                        .text_color(ACCENT)
                        .child("Jump to latest ↓")
                        .on_click(cx.listener(|this, _, _, cx| this.jump_to_latest(cx))),
                ),
            )
        })
    }
}

/// Debug builds only: `SCRIPTORIUM_DEBUG_THREAD=file.json` loads a fixed
/// conversation, for screenshots. The file is a `Thread`, or an object
/// `{"thread": Thread, "stream": [...], "error": "...", "active": bool,
/// "review_id": "...", "review_name": "...", "open": bool}` where each `stream` entry is
/// `{"text": ".."}`, `{"thinking": ".."}` or `{"tool": {"id": "..", "name": ".."}}`.
#[cfg(debug_assertions)]
impl MessageView {
    fn load_debug_fixture(&mut self, cx: &mut Context<Self>) {
        let Some(path) = std::env::var_os("SCRIPTORIUM_DEBUG_THREAD") else { return };
        let loaded = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str::<Value>(&s).map_err(|e| e.to_string()));
        let v = match loaded {
            Ok(v) => v,
            Err(e) => return crate::log::warn(&format!("SCRIPTORIUM_DEBUG_THREAD: {e}")),
        };
        let (thread, wrapper) = match v.get("thread") {
            Some(t) => (t.clone(), Some(&v)),
            None => (v.clone(), None),
        };
        match serde_json::from_value::<Thread>(thread) {
            Ok(t) => self.thread = t,
            Err(e) => return crate::log::warn(&format!("SCRIPTORIUM_DEBUG_THREAD: {e}")),
        }
        if let Some(w) = wrapper {
            let s = |k: &str| w.get(k).and_then(Value::as_str).map(str::to_string);
            self.review_id = s("review_id");
            self.review_name = s("review_name");
            self.run_active = w.get("active").and_then(Value::as_bool).unwrap_or(false);
            for (i, e) in w.get("stream").and_then(Value::as_array).into_iter().flatten().enumerate() {
                let text = |k: &str| e.get(k).and_then(Value::as_str).map(str::to_string);
                if let Some(text) = text("text") {
                    self.stream.event(Event::Text { index: i, text });
                } else if let Some(text) = text("thinking") {
                    self.stream.event(Event::Thinking { index: i, text });
                } else if let Some(t) = e.get("tool") {
                    let f = |k: &str| t.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    self.stream.event(Event::ToolCallStarted { id: f("id"), name: f("name") });
                }
            }
            if let Some(msg) = s("error") {
                self.stream.fail(&scriptorium_agent::AgentError::Parse(msg));
            }
        }
        self.rebuild(cx);
        if wrapper.is_some_and(|w| w.get("open").and_then(Value::as_bool) == Some(true)) {
            let keys = self.rows.iter().filter(|r| matches!(r.kind, Kind::Tool(_) | Kind::Reasoning { .. }));
            self.open.extend(keys.map(|r| r.key.clone()));
            self.list.reset(self.rows.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::first_suggestion;

    #[test]
    fn first_number_from_a_sentence_or_a_batch() {
        assert_eq!(first_suggestion("Added suggestion #14 (e7) on ¶812"), Some(14));
        assert_eq!(first_suggestion(r#"{"added":2,"suggestions":[{"n":9},{"n":10}],"failed":[]}"#), Some(9));
        assert_eq!(first_suggestion(r#"{"added":0,"suggestions":[],"failed":[]}"#), None);
        assert_eq!(first_suggestion("Nothing was written."), None);
    }
}
