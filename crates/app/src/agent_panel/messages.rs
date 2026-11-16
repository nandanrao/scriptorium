//! The message list: the panel's middle section, between the header and the
//! composer. Data in only: the shell hands it a [`Thread`] and feeds it
//! stream events; it turns them into rows ([`super::view_model`]) and draws
//! them in a bottom-aligned `list()` that follows the tail unless the writer
//! scrolls up.
//!
//! API (all data-in; there is no run logic here). Every call only records the
//! change and asks for a repaint; the rows are rebuilt once per frame, in
//! `render`, so a driver may call as often as it likes between frames.
//! - [`MessageView::set_thread`]: show a thread (another id switches: rows
//!   close, the view jumps to the end).
//! - [`MessageView::push_turn`]: the reply was pushed to the thread: clears
//!   the stream and sets the thread in one step, so the text rows survive.
//! - [`MessageView::stream_event`], [`MessageView::stream_events`],
//!   [`MessageView::update_stream`]: the streaming reply and the run's state
//!   lines (wait, fail, stop, clear).
//! - [`MessageView::set_run_active`], [`MessageView::set_review`].
//!
//! Each assistant text row keeps one `TextViewState` per [`Key`], so it
//! survives streaming and the push (same keys). `set_text` with text that
//! extends the old text is a `push_str`, and with equal text a no-op. Only
//! rows from the first changed one onward are touched, so the cost of a
//! token does not grow with the thread.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, Entity, FocusHandle, FontWeight, KeyDownEvent, ListAlignment, ListState, Rgba,
    SharedString, Stateful, WeakEntity, Window, div, list, prelude::*, px,
};
use gpui_base::text::{TextView, TextViewState};
use scriptorium_agent::{Event, Thread};
use serde_json::Value;

use super::view_model::{self as vm, Key, Kind, Row, RowCache, Stream, ToolRow, ToolState};
use crate::script_view::ScriptView;
use crate::theme::*;

/// The thread's review: its id for the suggestion links, its name for labels
/// (empty: the thread's own, unnamed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRef {
    pub id: String,
    pub name: String,
}

/// The things a row can offer that take focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Slot {
    Toggle,
    Link,
    All,
    Action,
}

/// What the message view asks of the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageEvent {
    /// The error row's button.
    Action(vm::ErrorAction),
}

impl gpui::EventEmitter<MessageEvent> for MessageView {}

type Act = Rc<dyn Fn(&mut MessageView, &mut Window, &mut Context<MessageView>)>;

pub struct MessageView {
    script: WeakEntity<ScriptView>,
    thread_id: Option<String>,
    thread: Thread,
    stream: Stream,
    cache: RowCache,
    run_active: bool,
    review: Option<ReviewRef>,
    /// Something changed since the rows were built.
    dirty: bool,
    rows: Vec<Row>,
    list: ListState,
    texts: HashMap<Key, Entity<TextViewState>>,
    /// Reasoning, tool and error rows the writer opened.
    open: HashSet<Key>,
    /// Tool results shown in full ("Show all").
    full: HashSet<Key>,
    focus: HashMap<(Key, Slot), FocusHandle>,
    jump_focus: FocusHandle,
    /// The rows the list last showed (first, end); everything until it scrolls.
    visible: Rc<Cell<(usize, usize)>>,
}

impl MessageView {
    pub fn new(script: WeakEntity<ScriptView>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let list = ListState::new(0, ListAlignment::Bottom, px(600.));
        list.set_follow_mode(gpui::FollowMode::Tail);
        // Repaint when the scroll position changes, for "Jump to latest".
        let id = cx.entity_id();
        let visible = Rc::new(Cell::new((0, usize::MAX)));
        let seen = visible.clone();
        list.set_scroll_handler(move |e, _, cx| {
            seen.set((e.visible_range.start, e.visible_range.end));
            cx.notify(id)
        });
        let mut this = Self {
            script,
            thread_id: None,
            thread: Thread::default(),
            stream: Stream::new(),
            cache: RowCache::default(),
            run_active: false,
            review: None,
            dirty: false,
            rows: Vec::new(),
            list,
            texts: HashMap::new(),
            open: HashSet::new(),
            full: HashSet::new(),
            focus: HashMap::new(),
            jump_focus: cx.focus_handle(),
            visible,
        };
        #[cfg(debug_assertions)]
        if let Some(fx) = super::debug_fixture::load() {
            this.thread_id = Some("debug".into());
            this.thread = fx.thread.clone();
            this.run_active = fx.active;
            this.review = fx.review.clone();
            for e in fx.events.clone() {
                this.stream.event(e);
            }
            if let Some(e) = fx.error() {
                this.stream.fail(&e);
            }
            this.rebuild(cx);
            if fx.open {
                let keys = this.rows.iter().filter(|r| matches!(r.kind, Kind::Tool(_) | Kind::Reasoning { .. }));
                this.open.extend(keys.map(|r| r.key.clone()));
                this.list.reset(this.rows.len());
            }
        }
        this
    }

    /// Shows `thread` (the thread with file id `id`). Another id than the last
    /// one switches threads: rows close and the view jumps to the end. The
    /// stream is left alone; feed it after switching.
    pub fn set_thread(&mut self, id: &str, thread: Thread, cx: &mut Context<Self>) {
        if self.thread_id.as_deref() != Some(id) {
            self.thread_id = Some(id.to_string());
            self.open.clear();
            self.full.clear();
            self.texts.clear();
            self.focus.clear();
            self.cache = RowCache::default();
            self.rows.clear();
            self.list.reset(0);
            self.list.set_follow_mode(gpui::FollowMode::Tail);
        }
        self.thread = thread;
        self.changed(cx);
    }

    /// The reply was pushed to the thread: clears the stream and sets the
    /// thread together, so the streamed text rows carry over.
    pub fn push_turn(&mut self, thread: Thread, cx: &mut Context<Self>) {
        self.stream.clear();
        self.thread = thread;
        self.changed(cx);
    }

    /// One reply event for the streaming buffer.
    #[allow(dead_code)] // The driver sends a frame's worth (stream_events).
    pub fn stream_event(&mut self, event: Event, cx: &mut Context<Self>) {
        self.stream.event(event);
        self.changed(cx);
    }

    /// Several events at once (a driver drains its channel once per frame).
    pub fn stream_events(&mut self, events: Vec<Event>, cx: &mut Context<Self>) {
        for e in events {
            self.stream.event(e);
        }
        self.changed(cx);
    }

    /// Any other change to the stream: `wait`, `fail`, `stop`, `clear`.
    pub fn update_stream(&mut self, f: impl FnOnce(&mut Stream), cx: &mut Context<Self>) {
        f(&mut self.stream);
        self.changed(cx);
    }

    /// Whether a run is in progress (a call without a result then reads "running").
    pub fn set_run_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.run_active != active {
            self.run_active = active;
            self.changed(cx);
        }
    }

    /// The thread's review, for labels and the suggestion links.
    pub fn set_review(&mut self, review: Option<ReviewRef>, cx: &mut Context<Self>) {
        if self.review != review {
            self.review = review;
            self.changed(cx);
        }
    }

    /// The focusable parts of the rows on screen, in order, for Tab.
    pub fn tab_stops(&self) -> Vec<FocusHandle> {
        let (first, end) = self.visible.get();
        let mut out = Vec::new();
        for row in self.rows.iter().enumerate().filter(|(ix, _)| (first..end).contains(ix)).map(|(_, r)| r) {
            for slot in [Slot::Toggle, Slot::Link, Slot::All] {
                out.extend(self.focus.get(&(row.key.clone(), slot)).cloned());
            }
        }
        out.push(self.jump_focus.clone());
        out
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        self.dirty = true;
        cx.notify();
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.dirty = false;
        let ctx = vm::Context { active: self.run_active, review: self.review.as_ref().map(|r| r.name.as_str()) };
        let mut new = self.cache.rows(&self.thread, &self.stream, ctx);
        // A streamed part that is only whitespace so far shows nothing.
        new.retain(|r| !matches!(&r.kind, Kind::Text { text, .. } if text.trim().is_empty()));
        let prefix = self.rows.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let (old_len, new_len) = (self.rows.len(), new.len());
        let was_streaming: HashSet<Key> = self.rows[prefix..]
            .iter()
            .filter(|r| matches!(r.kind, Kind::Text { streaming: true, .. }))
            .map(|r| r.key.clone())
            .collect();
        self.rows = new;
        if prefix != old_len || prefix != new_len {
            self.list.splice(prefix..old_len, new_len - prefix);
        }
        for row in &self.rows[prefix..] {
            let Kind::Text { text, streaming } = &row.kind else { continue };
            let state = self
                .texts
                .entry(row.key.clone())
                .or_insert_with(|| cx.new(|cx| TextViewState::markdown("", cx).selectable(true)))
                .clone();
            let finished = !streaming && was_streaming.contains(&row.key);
            state.update(cx, |s, cx| {
                s.set_text(text, cx);
                if finished {
                    // One full parse now the text is complete, so a reference
                    // link defined late in the reply resolves: a text that is
                    // not an extension of the current one is parsed afresh.
                    s.set_text(&format!("{text} "), cx);
                    s.set_text(text, cx);
                }
            });
        }
        let live: HashSet<&Key> = self.rows.iter().map(|r| &r.key).collect();
        self.texts.retain(|k, _| live.contains(k));
        self.open.retain(|k| live.contains(k));
        self.full.retain(|k| live.contains(k));
        self.focus.retain(|(k, _), _| live.contains(k));
    }

    fn row_ix(&self, key: &Key) -> Option<usize> {
        self.rows.iter().position(|r| &r.key == key)
    }

    /// Re-measure the row `key` after its look changed.
    fn row_changed(&mut self, key: &Key, cx: &mut Context<Self>) {
        if let Some(ix) = self.row_ix(key) {
            self.list.splice(ix..ix + 1, 1);
        }
        cx.notify();
    }

    fn toggle(&mut self, key: &Key, cx: &mut Context<Self>) {
        if !self.open.remove(key) {
            self.open.insert(key.clone());
        }
        self.row_changed(key, cx);
    }

    fn show_all(&mut self, key: &Key, cx: &mut Context<Self>) {
        self.full.insert(key.clone());
        self.row_changed(key, cx);
    }

    fn jump_to_latest(&mut self, cx: &mut Context<Self>) {
        self.list.set_follow_mode(gpui::FollowMode::Tail);
        self.list.scroll_to_end();
        cx.notify();
    }

    /// Makes `el` a control: focusable (with a ring), and run `act` on a click
    /// or on Enter or Space.
    fn control(
        &mut self,
        el: Stateful<gpui::Div>,
        key: &Key,
        slot: Slot,
        act: Act,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let handle = self.focus.entry((key.clone(), slot)).or_insert_with(|| cx.focus_handle()).clone();
        let (a, b) = (act.clone(), act);
        el.track_focus(&handle)
            .cursor_pointer()
            .rounded_sm()
            .focus_visible(|s| s.bg(ACCENT_SOFT))
            .on_click(cx.listener(move |this, _: &ClickEvent, w, cx| a(this, w, cx)))
            .on_key_down(cx.listener(move |this, e: &KeyDownEvent, w, cx| {
                if matches!(e.keystroke.key.as_str(), "enter" | "space") {
                    b(this, w, cx);
                    cx.stop_propagation();
                }
            }))
    }

    fn toggler(&mut self, el: Stateful<gpui::Div>, key: &Key, cx: &mut Context<Self>) -> Stateful<gpui::Div> {
        let k = key.clone();
        self.control(el, key, Slot::Toggle, Rc::new(move |this, _, cx| this.toggle(&k, cx)), cx)
    }

    fn render_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else { return div().into_any_element() };
        let key = row.key.clone();
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
                        .child(SharedString::from(text)),
                )
                .into_any_element(),
            Kind::Text { .. } => match self.texts.get(&key) {
                Some(state) => div().text_color(INK).child(TextView::new(state).selectable(true)).into_any_element(),
                None => div().into_any_element(),
            },
            Kind::Reasoning { text, hidden, streaming } => self.render_reasoning(ix, &key, text, hidden, streaming, cx),
            Kind::Tool(tool) => self.render_tool(ix, &key, &tool, cx),
            Kind::Notice { text } => div().italic().text_size(px(12.)).text_color(INK_2).child(text).into_any_element(),
            Kind::Error { text, detail, retryable, action } => {
                self.render_error(ix, &key, text, detail, retryable, action, cx)
            }
            Kind::Waiting { secs } => div()
                .italic()
                .text_size(px(12.))
                .text_color(INK_2)
                .child(format!("The service is busy, trying again in {secs}s…"))
                .into_any_element(),
        };
        div().w_full().px_3().py_1p5().child(body).into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_error(
        &mut self,
        ix: usize,
        key: &Key,
        text: String,
        detail: String,
        retryable: bool,
        action: Option<vm::ErrorAction>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.open.contains(key) && !detail.is_empty();
        let head = div().id(("error-detail", ix)).text_size(px(12.)).text_color(WARN).child(if open {
            "▾ Hide details"
        } else {
            "▸ Details"
        });
        let head = (!detail.is_empty()).then(|| self.toggler(head, key, cx));
        let button = action.map(|a| {
            let el = div()
                .id(("error-action", ix))
                .px_2()
                .py_0p5()
                .border_1()
                .border_color(WARN)
                .text_size(px(12.))
                .child(a.label());
            let act: Act = Rc::new(move |_, _, cx| cx.emit(MessageEvent::Action(a)));
            self.control(el, key, Slot::Action, act, cx)
        });
        div()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(WARN_SOFT)
            .text_color(WARN)
            .flex()
            .flex_col()
            .gap_1()
            .child(div().font_weight(FontWeight::SEMIBOLD).child(text))
            .when(retryable && action.is_none(), |d| {
                d.child(div().text_color(INK_2).child("You can send your message again."))
            })
            .children(button.map(|b| div().flex().child(b)))
            .children(head.map(|h| div().flex().child(h)))
            .when(open, |d| d.child(div().font_family("monospace").text_size(px(11.)).text_color(INK_2).child(detail)))
            .into_any_element()
    }

    fn render_reasoning(
        &mut self,
        ix: usize,
        key: &Key,
        text: std::sync::Arc<str>,
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
            .text_color(INK_2)
            .child(if hidden {
                "  "
            } else if open {
                "▾"
            } else {
                "▸"
            })
            .child(div().italic().child(title));
        let head = if hidden { head } else { self.toggler(head, key, cx) };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().flex().child(head))
            .when(open, |d| {
                d.child(
                    div()
                        .pl_3()
                        .ml_1()
                        .border_l_2()
                        .border_color(RULE)
                        .text_size(px(12.))
                        .text_color(INK_2)
                        .child(SharedString::from(text)),
                )
            })
            .into_any_element()
    }

    fn render_tool(&mut self, ix: usize, key: &Key, tool: &ToolRow, cx: &mut Context<Self>) -> AnyElement {
        let (icon, color): (&str, Rgba) = match tool.state {
            ToolState::Streaming | ToolState::Running => ("●", ACCENT),
            ToolState::Done => ("✓", OK),
            ToolState::Failed => ("✕", WARN),
            ToolState::NotRun => ("–", INK_2),
        };
        let has_details = !tool.args.is_empty() || tool.result.is_some();
        let open = has_details && self.open.contains(key);
        let link = self.suggestion_link(tool);
        let label_color = if tool.state == ToolState::Failed { WARN } else { INK_2 };
        // min_w_0: a long label (a review named after a long title) wraps
        // instead of running past the panel's edge and over its chevron.
        let label = div().id(("tool-label", ix)).min_w_0().text_color(label_color).child(tool.label.clone());
        let label = match link {
            // The label itself is the link: it shows the suggestion on the card.
            Some((id, n)) => {
                let act: Act = Rc::new(move |this, w, cx| this.show_suggestion(&id, n, w, cx));
                let l = self.control(label, key, Slot::Link, act, cx).text_color(ACCENT);
                l.hover(|s| s.underline())
            }
            None => label,
        };
        let chevron =
            div().id(("tool-chevron", ix)).flex_none().px_1().text_color(INK_2).child(if open { "▾" } else { "▸" });
        let head = div()
            .flex()
            .items_start()
            .gap_2()
            .child(div().flex_none().w(px(12.)).text_color(color).child(icon))
            .child(div().flex_1().min_w_0().flex().child(label))
            .when(has_details, |d| d.child(self.toggler(chevron, key, cx)));
        let shown_all = self.full.contains(key);
        let section = |title: &'static str, text: SharedString| {
            div()
                .flex()
                .flex_col()
                .child(div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(INK_2).child(title))
                .child(
                    div()
                        .pl_2()
                        .border_l_1()
                        .border_color(RULE)
                        .font_family("monospace")
                        .text_size(px(11.))
                        .text_color(INK_2)
                        .child(text),
                )
        };
        let mut details = div().pl(px(20.)).flex().flex_col().gap_1p5();
        if open {
            if !tool.args.is_empty() {
                details = details.child(section("Request", SharedString::from(tool.args.clone())));
            }
            if let Some(p) = &tool.result {
                let text = if shown_all {
                    SharedString::from(p.full_shared())
                } else {
                    SharedString::from(p.shown().to_string())
                };
                details = details.child(section("Result", text));
                if p.hidden > 0 && !shown_all {
                    let k = key.clone();
                    let more = div()
                        .id(("show-all", ix))
                        .text_color(ACCENT)
                        .child(format!("Show all ({} more lines)", p.hidden));
                    let more =
                        self.control(more, key, Slot::All, Rc::new(move |this, _, cx| this.show_all(&k, cx)), cx);
                    details = details.child(div().flex().child(more.hover(|s| s.underline())));
                }
            }
        }
        div()
            .flex()
            .flex_col()
            .gap_1()
            .text_size(px(12.))
            .child(head)
            .when(open, |d| d.child(details))
            .into_any_element()
    }

    /// The review id and first new suggestion number of a finished
    /// `suggest_change(s)` row, when the thread's review is known.
    fn suggestion_link(&self, tool: &ToolRow) -> Option<(String, usize)> {
        if tool.state != ToolState::Done || !matches!(tool.name.as_str(), "suggest_change" | "suggest_changes") {
            return None;
        }
        let n = first_suggestion(tool.result.as_ref()?.full())?;
        Some((self.review.as_ref()?.id.clone(), n))
    }

    /// The script view shows the suggestion and focuses the review card.
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
        if self.dirty {
            self.rebuild(cx);
        }
        // `is_scrolled_to_end` is None while rows are unmeasured, so ask the
        // follow state first.
        let away = !self.list.is_following_tail() && self.list.is_scrolled_to_end() != Some(true);
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
        let jump = away.then(|| {
            let button = div()
                .id("jump-to-latest")
                .track_focus(&self.jump_focus)
                .cursor_pointer()
                .px_3()
                .py_1()
                .rounded_full()
                .bg(PAPER)
                .border_1()
                .border_color(RULE)
                .focus_visible(|s| s.border_color(ACCENT))
                .shadow_sm()
                .text_size(px(12.))
                .text_color(ACCENT)
                .child("Jump to latest ↓")
                .on_click(cx.listener(|this, _, _, cx| this.jump_to_latest(cx)))
                .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                    if matches!(e.keystroke.key.as_str(), "enter" | "space") {
                        this.jump_to_latest(cx);
                        cx.stop_propagation();
                    }
                }));
            div().absolute().bottom_2().w_full().flex().justify_center().child(button)
        });
        div().id("agent-messages").relative().size_full().child(body).children(jump)
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
