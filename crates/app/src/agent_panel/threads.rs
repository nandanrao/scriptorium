//! The thread list: the header's thread title, which opens the list of this
//! script's conversations. The title is a Tab stop in the panel.
//!
//! The list is read each time it opens (`ThreadStore::list`, then one probe
//! per readable thread) and never polled: what it shows is as of the moment
//! it opened. Rows, newest first: title, a marker when the conversation is
//! running here, in use by another program, or can't be opened, and a line
//! of model, time and cost. Keys: Up/Down, Enter (open), Delete (remove,
//! asks first), Escape (close, or "keep" while asking).
//!
//! The pure part ([`build_rows`] and the small helpers) is tested below; the
//! rest is the entity.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    App, ClickEvent, Context, Entity, FocusHandle, FontWeight, KeyBinding, ScrollHandle, SharedString, Task, Window,
    actions, deferred, div, prelude::*, px,
};
use scriptorium_chat::core::labels::usage_text;
use scriptorium_chat::core::model::{Entry, Problem, ago, sorted};
use scriptorium_chat::naming;
use scriptorium_chat::store::{Listed, StoreError};
use scriptorium_providers::shell::config;
use scriptorium_providers::{PriceBook, thread_cost};

use super::driver::Chat;
use crate::theme::*;

actions!(
    agent_threads,
    [
        /// Ctrl/Cmd+T in the panel, or Enter / Space / Down on the title.
        OpenThreads,
        ListUp,
        ListDown,
        /// Enter: open the highlighted conversation (or start a new one).
        ListChoose,
        /// Escape: close the list; while asking about removal, keep the conversation.
        ListClose,
        /// Delete / Backspace: remove the highlighted conversation (asks first).
        ListRemove,
    ]
);

/// Key bindings of the title and the list (added to the panel's).
pub fn bindings() -> Vec<KeyBinding> {
    let t = if cfg!(target_os = "macos") { "cmd-t" } else { "ctrl-t" };
    let title = Some("ThreadTitle");
    let list = Some("ThreadList");
    vec![
        KeyBinding::new(t, OpenThreads, Some("AgentPanel")),
        KeyBinding::new("enter", OpenThreads, title),
        KeyBinding::new("space", OpenThreads, title),
        KeyBinding::new("down", OpenThreads, title),
        KeyBinding::new("up", ListUp, list),
        KeyBinding::new("down", ListDown, list),
        KeyBinding::new("enter", ListChoose, list),
        KeyBinding::new("escape", ListClose, list),
        KeyBinding::new("delete", ListRemove, list),
        KeyBinding::new("backspace", ListRemove, list),
        // Tab leaves the list, back to the title.
        KeyBinding::new("tab", ListClose, list),
        KeyBinding::new("shift-tab", ListClose, list),
    ]
}

/// The list's key line. The key Remove takes is labelled "delete" on a Mac
/// keyboard (it is Backspace elsewhere, and both work).
const LIST_KEYS: &str = if cfg!(target_os = "macos") {
    "↑↓ · Return open · Delete remove"
} else {
    "↑↓ · Enter open · Del remove"
};

/// Whether a conversation can be run now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    Free,
    /// This app is running it.
    Running,
    /// Another program is running it.
    Busy,
}

/// One row of the list.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub entry: Entry,
    /// The name of its review in this script, if it has one there.
    pub review: Option<String>,
    /// "18.2k tokens · $0.24"; `None` for a conversation with no replies yet.
    pub usage: Option<String>,
    pub hold: Hold,
}

impl Row {
    /// The small label at the right of the title.
    pub fn marker(&self) -> Option<&'static str> {
        match (&self.entry.problem, self.hold) {
            (Some(Problem::Newer(_)), _) => Some("Newer version"),
            (Some(Problem::Unreadable(_)), _) => Some("Can't open"),
            (None, Hold::Running) => Some("Working now"),
            (None, Hold::Busy) => Some("In use elsewhere"),
            (None, Hold::Free) => None,
        }
    }

    /// The first line: the conversation's title, or for a file that can't be
    /// opened what it is (its file name means nothing to the writer).
    pub fn title(&self) -> String {
        match &self.entry.problem {
            Some(Problem::Unreadable(_)) => "Damaged conversation".into(),
            Some(Problem::Newer(_)) => "Conversation from a newer Scriptorium".into(),
            None => self.entry.title.clone(),
        }
    }

    /// The second line: why it can't be opened, else model · time · cost.
    pub fn detail(&self, now: u64) -> String {
        // Plain words for the writer (the raw reason is for the CLI and logs).
        match &self.entry.problem {
            Some(Problem::Newer(_)) => return "Made by a newer Scriptorium. Update Scriptorium to open it.".into(),
            Some(Problem::Unreadable(_)) => return "This file is damaged, so it can't be opened.".into(),
            None => {}
        }
        let mut parts = Vec::new();
        if !self.entry.model.is_empty() {
            // The model's name as the footer shows it ("Claude Opus 5.5").
            let m = &self.entry.model;
            parts.push(scriptorium_providers::model_name(m).map_or_else(|| m.clone(), String::from));
        }
        parts.push(ago(now, self.entry.updated));
        if let Some(n) = self.entry.suggestions {
            parts.push(format!("{n} {}", if n == 1 { "suggestion" } else { "suggestions" }));
        }
        parts.extend(self.usage.clone());
        parts.join(" · ")
    }
}

/// The rows for what the folder held, newest first: each file with whether
/// it is free to run now (`Chat::probe`'s answer; only asked of readable
/// ones). `reviews` (id: name, suggestions) is the open script's; `book`
/// prices the cost.
pub fn build_rows(
    listed: Vec<(Listed, Result<(), StoreError>)>,
    reviews: &HashMap<String, (String, usize)>,
    book: &PriceBook,
) -> Vec<Row> {
    let review_of = |l: &Listed| {
        let f = l.file.as_ref().ok()?;
        let id = f.review_id.clone().unwrap_or_else(|| naming::review_id(&f.id));
        reviews.get(&id).cloned()
    };
    let entries = sorted(listed.iter().map(|(l, _)| Entry::from_listed(l, review_of(l).map(|(_, n)| n))).collect());
    let mut by_id: HashMap<String, (Listed, Result<(), StoreError>)> =
        listed.into_iter().map(|l| (l.0.id.clone(), l)).collect();
    entries
        .into_iter()
        .map(|entry| {
            let (listed, held) = by_id.remove(&entry.id).expect("an entry per listing");
            let review = review_of(&listed).map(|(name, _)| name);
            let file = listed.file.ok();
            let usage = file.as_ref().filter(|f| f.thread.messages.iter().any(|m| m.usage.is_some())).map(|f| {
                let cost = thread_cost(book, &f.thread);
                usage_text(&f.thread.usage(), cost.complete.then_some(cost.usd))
            });
            let hold = match held {
                _ if file.is_none() => Hold::Free,
                Err(StoreError::RunningHere) => Hold::Running,
                Err(StoreError::Busy) => Hold::Busy,
                _ => Hold::Free,
            };
            Row { entry, review, usage, hold }
        })
        .collect()
}

/// The highlight after Up or Down in a list of `len` items (the first is
/// "New conversation"); it stops at the ends.
pub fn step(selected: usize, len: usize, down: bool) -> usize {
    if down { (selected + 1).min(len.saturating_sub(1)) } else { selected.saturating_sub(1) }
}

/// The title's tooltip.
struct TitleTip(SharedString);

impl Render for TitleTip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(PAPER)
            .border_1()
            .border_color(RULE)
            .text_size(px(12.))
            .text_color(INK)
            .child(self.0.clone())
    }
}

pub struct ThreadList {
    /// The title (the list's trigger).
    focus: FocusHandle,
    /// The open list takes focus here.
    list_focus: FocusHandle,
    scroll: ScrollHandle,
    chat: Entity<Chat>,
    /// The open thread's title (the panel keeps it current from the chat).
    title: SharedString,
    open: bool,
    /// The folder is being read (off the UI thread).
    loading: bool,
    /// Bumped by each read; a result from an older one is dropped.
    generation: u64,
    _load: Option<Task<()>>,
    rows: Vec<Row>,
    /// When the list was read, Unix seconds.
    read_at: u64,
    /// The highlighted item: 0 is "New conversation", then the rows.
    selected: usize,
    /// The row being asked about (remove?).
    confirming: Option<String>,
    /// Why the last action didn't happen, or what it did.
    note: Option<String>,
}

impl ThreadList {
    pub fn new(chat: Entity<Chat>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            list_focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            chat,
            title: "New conversation".into(),
            open: false,
            loading: false,
            generation: 0,
            _load: None,
            rows: Vec::new(),
            read_at: 0,
            selected: 0,
            confirming: None,
            note: None,
        }
    }

    pub fn set_title(&mut self, title: impl Into<SharedString>, cx: &mut Context<Self>) {
        let title = title.into();
        if self.title != title {
            self.title = title;
            cx.notify();
        }
    }

    pub fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }

    /// Opens the list on the open conversation and reads the folder.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirming = None;
        self.note = None;
        self.open = true;
        self.selected = 0;
        self.load(true, cx);
        let list = self.list_focus.clone();
        window.defer(cx, move |window, cx| list.focus(window, cx));
        cx.notify();
    }

    /// Reads the folder and probes the threads on a background thread. The
    /// script's reviews are read here: they live on the UI thread.
    fn load(&mut self, select_open: bool, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.loading = true;
        let (lister, reviews, open_id) = {
            let chat = self.chat.read(cx);
            (chat.lister(), chat.reviews(cx), chat.thread().map(|t| t.id.clone()))
        };
        let work = cx.background_spawn(async move {
            let book = config::default_path()
                .and_then(|p| config::load(&p).ok())
                .map_or_else(PriceBook::builtin, |a| PriceBook::from_accounts(&a));
            lister.read().map(|listed| build_rows(listed, &reviews, &book))
        });
        self._load = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(rows) => this.rows = rows,
                    Err(e) => {
                        crate::log::warn(&format!("thread list: {e}"));
                        this.rows.clear();
                        this.note = Some("Couldn't read the conversations for this script.".into());
                    }
                }
                this.read_at = now_secs();
                if select_open {
                    this.selected =
                        open_id.and_then(|id| this.rows.iter().position(|r| r.entry.id == id)).map_or(0, |i| i + 1);
                } else {
                    this.selected = this.selected.min(this.rows.len());
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Closes the list without moving focus (it was clicked away).
    fn close(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            self.confirming = None;
            self.generation += 1;
            self.loading = false;
            self._load = None;
            cx.notify();
        }
    }

    /// Closes the list and gives focus back to the title.
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close(cx);
        window.focus(&self.focus, cx);
    }

    fn move_selection(&mut self, down: bool, cx: &mut Context<Self>) {
        self.confirming = None;
        self.selected = step(self.selected, self.rows.len() + 1, down);
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }

    fn on_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirming.take().is_some() {
            cx.notify();
        } else {
            self.dismiss(window, cx);
        }
    }

    fn on_choose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.confirming.clone() {
            self.remove(&id, window, cx);
        } else {
            self.activate(self.selected, window, cx);
        }
    }

    fn on_remove(&mut self, cx: &mut Context<Self>) {
        if let Some(row) = self.selected.checked_sub(1).and_then(|i| self.rows.get(i)) {
            self.confirming = Some(row.entry.id.clone());
            self.note = None;
            cx.notify();
        }
    }

    /// Item `ix` (0: new conversation): open it.
    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = ix;
        self.confirming = None;
        self.note = None;
        let chat = self.chat.clone();
        let Some(row) = ix.checked_sub(1).map(|i| self.rows.get(i).cloned()) else {
            return self.start_new(window, cx);
        };
        let Some(row) = row else { return };
        if row.entry.greyed() {
            // The row already says why.
            return cx.notify();
        }
        let id = row.entry.id.clone();
        if chat.read(cx).thread().is_some_and(|t| t.id == id) {
            return self.dismiss(window, cx);
        }
        // A busy conversation opens read-only, with its note (the chat does both).
        match chat.update(cx, |c, cx| c.open_thread(&id, cx)) {
            Ok(()) => self.dismiss(window, cx),
            Err(e) => {
                self.note = Some(e);
                cx.notify();
            }
        }
    }

    /// Through the panel's own New conversation action, so its draft and
    /// disclosure state are cleared the same way.
    fn start_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat.read(cx).is_running() {
            self.note = Some("Stop the agent before starting a new conversation.".into());
            return cx.notify();
        }
        self.dismiss(window, cx);
        window.dispatch_action(Box::new(super::NewThread), cx);
    }

    /// Removes the conversation (`Chat::remove_thread`: its file only).
    fn remove(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.confirming = None;
        match self.chat.update(cx, |c, cx| c.remove_thread(id, cx)) {
            Ok(()) => {
                self.note = Some("Removed.".into());
                self.load(false, cx);
            }
            Err(e) => self.note = Some(e),
        }
        window.focus(&self.list_focus, cx);
        cx.notify();
    }

    fn render_row(&self, i: usize, row: &Row, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let ix = i + 1;
        let asking = self.confirming.as_deref() == Some(row.entry.id.as_str());
        let current = self.chat.read(cx).thread().is_some_and(|t| t.id == row.entry.id);
        let greyed = row.entry.greyed();
        let id = row.entry.id.clone();
        let marker = row.marker();
        let group: SharedString = format!("thread-g{i}").into();
        let highlighted = ix == self.selected;
        let detail = if asking {
            match &row.review {
                Some(name) => format!(
                    "Its suggestions stay in the review '{name}' under Reviews, and you can still decide them there."
                ),
                None => "Remove this conversation?".to_string(),
            }
        } else {
            row.detail(self.read_at)
        };
        div()
            .id(("thread-row", i))
            .group(group.clone())
            .px_2()
            .py(px(5.))
            .cursor_pointer()
            .rounded_sm()
            .flex()
            .flex_col()
            .gap(px(1.))
            .when(ix == self.selected, |d| d.bg(ACCENT_SOFT))
            .when(asking, |d| d.bg(WARN_SOFT))
            .hover(|d| d.bg(ACCENT_SOFT))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.activate(ix, window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(if current { FontWeight::BOLD } else { FontWeight::SEMIBOLD })
                            .when(greyed, |d| d.text_color(INK_3))
                            .child(row.title()),
                    )
                    // Remove first, so the markers line up at the right
                    // edge (Remove is invisible until the row is hovered).
                    .when(!asking, |d| {
                        d.child(
                            div()
                                .id(("thread-remove", i))
                                .flex_none()
                                .px_1()
                                .rounded_sm()
                                .text_size(px(11.5))
                                .text_color(if highlighted { INK_3 } else { gpui::transparent_black().into() })
                                .group_hover(group, |s| s.text_color(INK_3))
                                .hover(|s| s.bg(WARN_SOFT).text_color(WARN))
                                .child("Remove")
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.selected = ix;
                                    this.confirming = Some(id.clone());
                                    this.note = None;
                                    cx.notify();
                                })),
                        )
                    })
                    .children(marker.map(|m| {
                        let problem = greyed;
                        div()
                            .flex_none()
                            .px_1()
                            .rounded_sm()
                            .text_size(px(10.5))
                            .bg(if problem {
                                WARN_SOFT
                            } else if row.hold == Hold::Busy {
                                ACCENT_SOFT
                            } else {
                                OK_SOFT
                            })
                            .text_color(if problem {
                                WARN
                            } else if row.hold == Hold::Busy {
                                OWN
                            } else {
                                OK
                            })
                            .child(m)
                    })),
            )
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(if asking {
                        WARN
                    } else if greyed {
                        INK_3
                    } else {
                        INK_2
                    })
                    .child(detail),
            )
            .when(asking, |d| {
                let yes = row.entry.id.clone();
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .text_size(px(11.5))
                        .child(
                            div()
                                .id(("thread-remove-yes", i))
                                .cursor_pointer()
                                .text_color(WARN)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Remove (Enter)")
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.remove(&yes, window, cx);
                                })),
                        )
                        .child(
                            div()
                                .id(("thread-remove-no", i))
                                .cursor_pointer()
                                .text_color(INK_2)
                                .child("Keep (Esc)")
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.confirming = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
    }

    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let new_row = div()
            .id("thread-new")
            .px_2()
            .py(px(5.))
            .rounded_sm()
            .cursor_pointer()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(ACCENT)
            .when(self.selected == 0, |d| d.bg(ACCENT_SOFT))
            .hover(|d| d.bg(ACCENT_SOFT))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.activate(0, window, cx)))
            .child(format!("+ New conversation  ({})", crate::menus::shortcut("n")));
        let rows: Vec<_> =
            self.rows.iter().enumerate().map(|(i, r)| self.render_row(i, r, cx).into_any_element()).collect();
        deferred(
            div()
                .id("thread-list")
                .track_focus(&self.list_focus)
                .key_context("ThreadList")
                .absolute()
                .top(px(30.))
                .left(px(-8.))
                .w(px(super::WIDTH - 16.))
                .max_h(px(440.))
                .occlude()
                .p_1()
                .bg(PAPER)
                .border_1()
                .border_color(RULE)
                .rounded_md()
                .shadow_md()
                .flex()
                .flex_col()
                .text_size(px(13.))
                .font_weight(FontWeight::NORMAL)
                .text_color(INK)
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close(cx)))
                .on_action(cx.listener(|this, _: &ListUp, _, cx| this.move_selection(false, cx)))
                .on_action(cx.listener(|this, _: &ListDown, _, cx| this.move_selection(true, cx)))
                .on_action(cx.listener(|this, _: &ListChoose, window, cx| this.on_choose(window, cx)))
                .on_action(cx.listener(|this, _: &ListClose, window, cx| this.on_close(window, cx)))
                .on_action(cx.listener(|this, _: &ListRemove, _, cx| this.on_remove(cx)))
                .child(
                    div()
                        .id("thread-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .child(new_row)
                        .children(rows)
                        .when(self.loading && self.rows.is_empty(), |d| {
                            d.child(div().px_2().py_1().text_color(INK_3).child("Reading…"))
                        })
                        .when(!self.loading && self.rows.is_empty(), |d| {
                            d.child(div().px_2().py_1().text_color(INK_3).child("No earlier conversations yet."))
                        }),
                )
                .children(self.note.clone().map(|n| {
                    div()
                        .flex_none()
                        .px_2()
                        .py_1()
                        .border_t_1()
                        .border_color(RULE)
                        .text_size(px(11.5))
                        .text_color(INK_2)
                        .child(n)
                }))
                .child(div().flex_none().px_2().pt_1().text_size(px(10.5)).text_color(INK_3).child(LIST_KEYS)),
        )
        .with_priority(2)
    }
}

impl Render for ThreadList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .id("agent-title")
                    .track_focus(&self.focus)
                    .key_context("ThreadTitle")
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(focused || self.open, |d| d.bg(ACCENT_SOFT))
                    .hover(|d| d.bg(ACCENT_SOFT))
                    .on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| {
                            if this.open { this.dismiss(window, cx) } else { this.open(window, cx) }
                        },
                    ))
                    .tooltip({
                        let tip: SharedString = format!("Conversations ({})", crate::menus::shortcut("t")).into();
                        move |_, cx| cx.new(|_| TitleTip(tip.clone())).into()
                    })
                    .child(div().min_w_0().truncate().child(self.title.clone()))
                    .child(div().flex_none().text_color(INK_3).child("▾")),
            )
            .when(self.open, |d| d.child(self.render_list(cx)))
    }
}

/// Now, by the chat clock (so `SCRIPTORIUM_TEST_CLOCK` fixes "5 min ago"
/// as it fixes the threads' own times), else the system clock.
fn now_secs() -> u64 {
    scriptorium_chat::clock::from_env()
        .ok()
        .and_then(|c| scriptorium_chat::core::model::parse_rfc3339(&c.clock.now()))
        .unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use scriptorium_agent::file::{ThreadFile, VERSION, tools_hash};
    use scriptorium_agent::{Block, Message, Origin, Thread, Usage};

    fn file(id: &str, title: &str, updated: &str) -> ThreadFile {
        let mut thread = Thread::default();
        assert!(thread.push_user("hello"));
        ThreadFile {
            version: VERSION,
            id: id.into(),
            title: title.into(),
            created: updated.into(),
            updated: updated.into(),
            account: "a".into(),
            model: "claude-opus-5-5".into(),
            review_id: None,
            review_path: None,
            whole_script: None,
            system: "s".into(),
            tools_hash: tools_hash(&[]),
            thread,
            extra: Default::default(),
        }
    }

    fn with_reply(mut f: ThreadFile) -> ThreadFile {
        let mut m = Message::assistant(vec![Block::Text { text: "ok".into() }]);
        m.origin = Some(Origin::new("https://api.anthropic.com/v1", "claude-opus-5-5"));
        m.usage = Some(Usage { input: 10_000, output: 1_000, known: true, ..Usage::default() });
        f.thread.messages.push(m);
        f
    }

    fn ok(f: ThreadFile) -> (Listed, Result<(), StoreError>) {
        (Listed { id: f.id.clone(), file: Ok(f) }, Ok(()))
    }

    fn held(mut l: (Listed, Result<(), StoreError>), e: StoreError) -> (Listed, Result<(), StoreError>) {
        l.1 = Err(e);
        l
    }

    fn broken(id: &str, e: StoreError) -> (Listed, Result<(), StoreError>) {
        (Listed { id: id.into(), file: Err(e) }, Ok(()))
    }

    fn rows(listed: Vec<(Listed, Result<(), StoreError>)>) -> Vec<Row> {
        build_rows(listed, &HashMap::new(), &PriceBook::builtin())
    }

    #[test]
    fn newest_first_with_markers() {
        let listed = vec![
            held(ok(file("a", "Old one", "2026-11-10T09:00:00Z")), StoreError::Busy),
            held(ok(with_reply(file("b", "Newest", "2026-11-15T09:00:00Z"))), StoreError::RunningHere),
            ok(file("c", "Middle", "2026-11-12T09:00:00Z")),
            broken("d", StoreError::Newer { id: "d".into(), message: "version 9".into() }),
            broken("e", StoreError::File { id: "e".into(), message: "not JSON".into() }),
        ];
        let rows = rows(listed);
        let ids: Vec<_> = rows.iter().map(|r| r.entry.id.as_str()).collect();
        assert_eq!(ids, ["b", "c", "a", "e", "d"], "unreadable ones sort last");
        let markers: Vec<_> = rows.iter().map(Row::marker).collect();
        assert_eq!(
            markers,
            [Some("Working now"), None, Some("In use elsewhere"), Some("Can't open"), Some("Newer version")]
        );
        assert_eq!(rows[0].hold, Hold::Running);
        assert_eq!(rows[2].hold, Hold::Busy);
    }

    #[test]
    fn detail_line_shows_model_time_suggestions_and_cost() {
        let now = scriptorium_chat::core::model::parse_rfc3339("2026-11-15T09:05:00Z").unwrap();
        let mut reviews = HashMap::new();
        reviews.insert(naming::review_id("b"), ("Tighten the dialogue".to_string(), 3));
        let rows = build_rows(
            vec![ok(with_reply(file("b", "T", "2026-11-15T09:00:00Z"))), ok(file("c", "U", "2026-11-14T09:05:00Z"))],
            &reviews,
            &PriceBook::builtin(),
        );
        // 10k in and 1k out on Opus: a few cents.
        let d = rows[0].detail(now);
        assert!(d.starts_with("Claude Opus 5.5 · 5 min ago · 3 suggestions · 11.0k tokens · $0."), "{d}");
        assert_eq!(rows[0].review.as_deref(), Some("Tighten the dialogue"));
        // No replies and no review yet.
        assert_eq!(rows[1].detail(now), "Claude Opus 5.5 · 1 day ago");
        assert_eq!(rows[1].review, None);
        let bad = rows_of_one(broken("x", StoreError::Newer { id: "x".into(), message: "v9".into() }));
        assert_eq!(bad[0].detail(now), "Made by a newer Scriptorium. Update Scriptorium to open it.");
    }

    fn rows_of_one(l: (Listed, Result<(), StoreError>)) -> Vec<Row> {
        rows(vec![l])
    }

    #[test]
    fn highlight_stops_at_the_ends() {
        assert_eq!(step(0, 3, false), 0);
        assert_eq!(step(0, 3, true), 1);
        assert_eq!(step(2, 3, true), 2);
        assert_eq!(step(2, 3, false), 1);
        assert_eq!(step(0, 1, true), 0, "just the new-conversation item");
    }
}
