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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{
    App, ClickEvent, Context, Entity, FocusHandle, FontWeight, KeyBinding, ScrollHandle, SharedString, Window, actions,
    deferred, div, prelude::*, px,
};
use scriptorium_chat::core::labels::usage_text;
use scriptorium_chat::core::model::{Entry, Problem, ago, sorted};
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
            parts.push(self.entry.model.clone());
        }
        parts.push(ago(now, self.entry.updated));
        parts.extend(self.usage.clone());
        parts.join(" · ")
    }
}

/// The rows for what `list` found, newest first. `probe` says whether a
/// readable thread is free (the panel passes `Chat::probe`); `book` prices
/// the cost.
pub fn build_rows(listed: Vec<Listed>, probe: impl Fn(&str) -> Result<(), StoreError>, book: &PriceBook) -> Vec<Row> {
    let entries = sorted(listed.iter().map(|l| Entry::from_listed(l, None)).collect());
    let mut by_id: HashMap<String, Listed> = listed.into_iter().map(|l| (l.id.clone(), l)).collect();
    entries
        .into_iter()
        .map(|entry| {
            let file = by_id.remove(&entry.id).and_then(|l| l.file.ok());
            let usage = file.as_ref().filter(|f| f.thread.messages.iter().any(|m| m.usage.is_some())).map(|f| {
                let cost = thread_cost(book, &f.thread);
                usage_text(&f.thread.usage(), cost.complete.then_some(cost.usd))
            });
            let hold = if file.is_none() {
                Hold::Free
            } else {
                match probe(&entry.id) {
                    Err(StoreError::RunningHere) => Hold::Running,
                    Err(StoreError::Busy) => Hold::Busy,
                    _ => Hold::Free,
                }
            };
            Row { entry, usage, hold }
        })
        .collect()
}

/// The highlight after Up or Down in a list of `len` items (the first is
/// "New conversation"); it stops at the ends.
pub fn step(selected: usize, len: usize, down: bool) -> usize {
    if down { (selected + 1).min(len.saturating_sub(1)) } else { selected.saturating_sub(1) }
}

const BUSY_NOTE: &str = "Another program is using this conversation right now, so you can read it but not add to it. \
                         It will work again once the other program is done.";

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
        let list_focus = cx.focus_handle();
        Self {
            focus: cx.focus_handle(),
            list_focus,
            scroll: ScrollHandle::new(),
            chat,
            title: "New conversation".into(),
            open: false,
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

    /// Reads the folder and opens the list on the open conversation.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh(cx);
        let open_id = self.chat.read(cx).thread().map(|t| t.id.clone());
        self.selected = open_id.and_then(|id| self.rows.iter().position(|r| r.entry.id == id)).map_or(0, |i| i + 1);
        self.confirming = None;
        self.open = true;
        let list = self.list_focus.clone();
        window.defer(cx, move |window, cx| list.focus(window, cx));
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let chat = self.chat.read(cx);
        let store = chat.store();
        let book = config::default_path()
            .and_then(|p| config::load(&p).ok())
            .map_or_else(PriceBook::builtin, |a| PriceBook::from_accounts(&a));
        match store.list() {
            Ok(listed) => {
                self.rows = build_rows(listed, |id| chat.probe(id), &book);
                self.note = None;
            }
            Err(e) => {
                crate::log::warn(&format!("thread list: {e}"));
                self.rows.clear();
                self.note = Some("Couldn't read the conversations for this script.".into());
            }
        }
        self.read_at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    }

    /// Closes the list without moving focus (it was lost or clicked away).
    fn close(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            self.confirming = None;
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
        match chat.update(cx, |c, cx| c.open_thread(&id, cx)) {
            Ok(()) => {
                if row.hold == Hold::Busy {
                    chat.update(cx, |c, cx| c.notice(BUSY_NOTE, cx));
                }
                self.dismiss(window, cx);
            }
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

    /// Deletes the conversation's file. Its lock file stays (lock files are
    /// never deleted), and so does its review file with the suggestions.
    fn remove(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.confirming = None;
        let chat = self.chat.clone();
        if chat.read(cx).running_thread() == Some(id) {
            self.note = Some("Stop the agent before removing this conversation.".into());
            return cx.notify();
        }
        let store = chat.read(cx).store();
        let removed = store.lock(id, Duration::ZERO).and_then(|lock| store.delete(&lock));
        match removed {
            Ok(()) | Err(StoreError::NotFound(_)) => {
                if chat.read(cx).thread().is_some_and(|t| t.id == id) {
                    // The open conversation is gone: show a fresh one.
                    chat.update(cx, |c, cx| c.new_thread(cx)).ok();
                }
                self.refresh(cx);
                self.selected = self.selected.min(self.rows.len());
                self.note = Some("Removed. Any suggestions it made stay in the review.".into());
            }
            Err(StoreError::Busy) => {
                self.note = Some("Another program is using this conversation. Try again when it's done.".into());
            }
            Err(e) => self.note = Some(format!("Couldn't remove it: {e}")),
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
        let detail = if asking {
            "Remove this conversation? Suggestions it made stay in the review.".to_string()
        } else {
            row.detail(self.read_at)
        };
        div()
            .id(("thread-row", i))
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
                            .child(row.entry.title.clone()),
                    )
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
                    }))
                    .when(!asking, |d| {
                        d.child(
                            div()
                                .id(("thread-remove", i))
                                .flex_none()
                                .px_1()
                                .rounded_sm()
                                .text_size(px(11.5))
                                .text_color(INK_3)
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
                    }),
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
                        .when(self.rows.is_empty(), |d| {
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
                .child(
                    div()
                        .flex_none()
                        .px_2()
                        .pt_1()
                        .text_size(px(10.5))
                        .text_color(INK_3)
                        .child("Up/Down to choose · Enter to open · Delete to remove · Esc to close"),
                ),
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
                    .child(div().min_w_0().truncate().child(self.title.clone()))
                    .child(div().flex_none().text_color(INK_3).child("▾")),
            )
            .when(self.open, |d| d.child(self.render_list(cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scriptorium_agent::file::{ThreadFile, VERSION, tools_hash};
    use scriptorium_agent::{Block, Message, Origin, Thread, Usage};
    use scriptorium_chat::store::ThreadStore;

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

    fn ok(f: ThreadFile) -> Listed {
        Listed { id: f.id.clone(), file: Ok(f) }
    }

    fn free(_: &str) -> Result<(), StoreError> {
        Ok(())
    }

    #[test]
    fn newest_first_with_markers() {
        let listed = vec![
            ok(file("a", "Old one", "2026-11-10T09:00:00Z")),
            ok(with_reply(file("b", "Newest", "2026-11-15T09:00:00Z"))),
            ok(file("c", "Middle", "2026-11-12T09:00:00Z")),
            Listed { id: "d".into(), file: Err(StoreError::Newer { id: "d".into(), message: "version 9".into() }) },
            Listed { id: "e".into(), file: Err(StoreError::File { id: "e".into(), message: "not JSON".into() }) },
        ];
        let probe = |id: &str| match id {
            "a" => Err(StoreError::Busy),
            "b" => Err(StoreError::RunningHere),
            _ => Ok(()),
        };
        let rows = build_rows(listed, probe, &PriceBook::builtin());
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
    fn probe_skips_unreadable_files() {
        let listed =
            vec![Listed { id: "x".into(), file: Err(StoreError::File { id: "x".into(), message: "bad".into() }) }];
        let rows = build_rows(listed, |_| panic!("no probe for a file that can't be read"), &PriceBook::builtin());
        assert_eq!(rows[0].hold, Hold::Free);
    }

    #[test]
    fn detail_line_shows_model_time_and_cost() {
        let now = scriptorium_chat::core::model::parse_rfc3339("2026-11-15T09:05:00Z").unwrap();
        let rows = build_rows(
            vec![ok(with_reply(file("b", "T", "2026-11-15T09:00:00Z"))), ok(file("c", "U", "2026-11-14T09:05:00Z"))],
            free,
            &PriceBook::builtin(),
        );
        // 10k in and 1k out on Opus: a few cents.
        let d = rows[0].detail(now);
        assert!(d.starts_with("claude-opus-5-5 · 5 min ago · 11.0k tokens · $0."), "{d}");
        // No replies yet: no usage part.
        assert_eq!(rows[1].detail(now), "claude-opus-5-5 · 1 day ago");
        let bad = build_rows(
            vec![Listed { id: "x".into(), file: Err(StoreError::Newer { id: "x".into(), message: "v9".into() }) }],
            free,
            &PriceBook::builtin(),
        );
        assert_eq!(bad[0].detail(now), "Made by a newer Scriptorium. Update Scriptorium to open it.");
    }

    #[test]
    fn highlight_stops_at_the_ends() {
        assert_eq!(step(0, 3, false), 0);
        assert_eq!(step(0, 3, true), 1);
        assert_eq!(step(2, 3, true), 2);
        assert_eq!(step(2, 3, false), 1);
        assert_eq!(step(0, 1, true), 0, "just the new-conversation item");
    }

    #[test]
    fn removing_deletes_the_file_and_keeps_the_lock_file() {
        let dir = std::env::temp_dir().join(format!("scriptorium-u7-remove-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = ThreadStore::for_script(&dir.join("film.fdx"));
        let lock = store.create(&mut scriptorium_chat::clock::SequentialIds::default()).unwrap();
        store.write(&lock, &file(lock.id(), "Gone", "2026-11-15T09:00:00Z")).unwrap();
        let id = lock.id().to_string();
        drop(lock);
        // What `ThreadList::remove` does.
        let l = store.lock(&id, Duration::ZERO).unwrap();
        store.delete(&l).unwrap();
        drop(l);
        assert!(store.list().unwrap().is_empty());
        assert!(store.lock_path(&id).unwrap().exists(), "lock files are never deleted");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
