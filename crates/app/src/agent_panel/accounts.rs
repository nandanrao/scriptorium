//! The AI accounts sheet: list, add, edit and remove accounts, set the
//! default, store or delete a key, and "Check" an account. The workspace
//! opens it on [`super::OpenAccounts`] and draws it over the whole window,
//! with or without a script; it closes on Escape (from the list; from a form
//! Escape goes back to the list first), a click outside, or
//! [`AccountsSheet::close`], and the workspace then gives focus back to what
//! had it.
//!
//! Nothing here touches the keyring, the accounts file or the network on the
//! UI thread (a locked Linux keyring can block for about 25 s): every such
//! call is in [`work`], run on a background task, and the sheet shows
//! "Loading…" / "Saving…" / "Checking…" meanwhile. The form's logic (what
//! becomes an `Account`, ids, warnings, key wording) is in [`form`], with its
//! tests. After any change to the accounts the sheet calls
//! [`accounts_changed`]; whatever shows accounts (the composer's model chip)
//! observes [`AccountsChanged`] and redraws.
//!
//! Tests can set `SCRIPTORIUM_TEST_KEYS=memory` (an in-memory keychain) or
//! `=env` (no keychain) so the real one is never touched, and
//! `XDG_CONFIG_HOME` to a scratch folder for the accounts file.

mod form;
mod work;

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Global,
    SharedString, Subscription, Window, deferred, div, prelude::*, px, rgba,
};
use gpui_base::actions::{SelectDown, SelectUp};
use gpui_base::input::{InputEvent, InputState};
use gpui_base::{Input, Select, Sheet};
use scriptorium_providers::shell::{KeyStatus, Keys};
use scriptorium_providers::{Account, AccountsFile};

use crate::theme::*;
use form::{EFFORTS, Form, Kind, PROTOCOLS, Tone};
use work::{Done, Snapshot};

/// Bumped each time the accounts change; observe it with
/// `cx.observe_global::<AccountsChanged>(..)`.
#[derive(Default)]
pub struct AccountsChanged(pub u64);

impl Global for AccountsChanged {}

/// Tell every observer of [`AccountsChanged`] that the accounts changed.
pub fn accounts_changed(cx: &mut App) {
    cx.update_default_global::<AccountsChanged, _>(|g, _| g.0 += 1);
}

/// The sheet asks to be closed.
pub struct Dismissed;

/// The accounts file, as far as the sheet has read it.
enum Loaded {
    Loading,
    /// Couldn't be read (or there's nowhere to keep it): nothing can be
    /// edited, so a bad file is never overwritten.
    Failed(String),
    Ready(Snapshot),
}

/// A line telling the writer how something went.
struct Note {
    tone: Tone,
    text: String,
}

impl Note {
    fn good(text: impl Into<String>) -> Option<Note> {
        Some(Note { tone: Tone::Good, text: text.into() })
    }
    fn bad(text: impl Into<String>) -> Option<Note> {
        Some(Note { tone: Tone::Bad, text: text.into() })
    }
}

/// The drop-downs of the form, each a `Select` with its own focus handles.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dd {
    Model,
    Effort,
    Protocol,
}

/// The add / edit form.
struct FormView {
    form: Form,
    /// The account being edited, whose other settings carry over.
    base: Option<Account>,
    name: Entity<InputState>,
    base_url: Entity<InputState>,
    model: Entity<InputState>,
    max_tokens: Entity<InputState>,
    key: Entity<InputState>,
    /// The key's status; `None` while it is being looked up.
    status: Option<KeyStatus>,
    /// What is in flight ("Saving…", "Checking…").
    busy: Option<&'static str>,
    note: Option<Note>,
    _subs: Vec<Subscription>,
}

impl FormView {
    /// The form with what is typed in the text fields.
    fn current(&self, cx: &App) -> Form {
        let text = |e: &Entity<InputState>| e.read(cx).value().to_string();
        Form {
            name: text(&self.name),
            base_url: text(&self.base_url),
            model: text(&self.model),
            max_tokens: text(&self.max_tokens),
            ..self.form.clone()
        }
    }
}

enum Screen {
    List,
    Form(Box<FormView>),
}

pub struct AccountsSheet {
    focus: FocusHandle,
    keys: Arc<Keys>,
    /// Where the accounts file is; or why it can't be.
    path: Result<PathBuf, String>,
    loaded: Loaded,
    screen: Screen,
    /// The account whose "Remove" is waiting for a second click.
    confirm_remove: Option<String>,
    /// How the last change to the list went.
    notice: Option<Note>,
    /// Something is being saved, removed or loaded.
    busy: bool,
    dropdown: Option<Dd>,
    /// The highlighted option of the open drop-down.
    highlight: usize,
    triggers: [FocusHandle; 3],
    lists: [FocusHandle; 3],
}

impl EventEmitter<Dismissed> for AccountsSheet {}

impl AccountsSheet {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let path = form::accounts_path();
        let mut this = Self {
            focus: cx.focus_handle(),
            keys: Arc::new(work::keys_from_env()),
            loaded: match &path {
                Ok(_) => Loaded::Loading,
                Err(e) => Loaded::Failed(e.clone()),
            },
            path,
            screen: Screen::List,
            confirm_remove: None,
            notice: None,
            busy: false,
            dropdown: None,
            highlight: 0,
            triggers: [cx.focus_handle(), cx.focus_handle(), cx.focus_handle()],
            lists: [cx.focus_handle(), cx.focus_handle(), cx.focus_handle()],
        };
        if let Ok(path) = this.path.clone() {
            let keys = this.keys.clone();
            this.run(
                window,
                cx,
                move || work::snapshot(&path, &keys),
                |this, r, _, _| {
                    this.loaded = match r {
                        Ok(s) => Loaded::Ready(s),
                        Err(e) => Loaded::Failed(e),
                    }
                },
            );
        }
        this
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(Dismissed);
    }

    /// Runs `work` on a background thread (it may block on the keyring, the
    /// disk or the network), then `then` on the UI thread.
    fn run<T: Send + 'static>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        work: impl FnOnce() -> T + Send + 'static,
        then: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let out = cx.background_executor().spawn(async move { work() }).await;
            this.update_in(cx, |this, window, cx| {
                then(this, out, window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn file(&self) -> Option<&AccountsFile> {
        match &self.loaded {
            Loaded::Ready(s) => Some(&s.file),
            _ => None,
        }
    }

    /// Escape and a click outside: from a form back to the list, from the
    /// list out of the sheet.
    fn back_or_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.screen {
            Screen::Form(_) => {
                self.screen = Screen::List;
                self.dropdown = None;
                window.focus(&self.focus, cx);
                cx.notify();
            }
            Screen::List => self.close(cx),
        }
    }

    /// Takes a finished change: the new file, a note, back to the list.
    fn finished(&mut self, done: Done, ok: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.loaded = Loaded::Ready(done.snapshot);
        self.notice = match done.problem {
            Some(p) => Note::bad(p),
            None => Note::good(ok),
        };
        self.screen = Screen::List;
        self.dropdown = None;
        self.confirm_remove = None;
        self.busy = false;
        window.focus(&self.focus, cx);
        accounts_changed(cx);
    }

    // ---- the list -------------------------------------------------------------

    fn remove(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let (Ok(path), false) = (self.path.clone(), self.busy) else { return };
        self.busy = true;
        let keys = self.keys.clone();
        self.run(
            window,
            cx,
            move || work::remove_account(&path, &keys, &id),
            |this, r, window, cx| match r {
                Ok(done) => this.finished(done, "Account removed.", window, cx),
                Err(e) => {
                    this.busy = false;
                    this.notice = Note::bad(e);
                }
            },
        );
    }

    fn make_default(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let (Ok(path), false) = (self.path.clone(), self.busy) else { return };
        self.busy = true;
        let keys = self.keys.clone();
        self.run(
            window,
            cx,
            move || work::make_default(&path, &keys, &id),
            |this, r, _, cx| {
                this.busy = false;
                match r {
                    Ok(s) => {
                        this.loaded = Loaded::Ready(s);
                        this.notice = Note::good("Default account changed.");
                        accounts_changed(cx);
                    }
                    Err(e) => this.notice = Note::bad(e),
                }
            },
        );
    }

    // ---- the form -------------------------------------------------------------

    fn open_form(&mut self, form: Form, base: Option<Account>, window: &mut Window, cx: &mut Context<Self>) {
        let editing = base.is_some();
        let field = |value: &str, placeholder: &str, masked: bool, window: &mut Window, cx: &mut Context<Self>| {
            let value = value.to_string();
            let placeholder = placeholder.to_string();
            cx.new(|cx| {
                let mut s = InputState::new(window, cx).placeholder(placeholder).masked(masked);
                s.set_value(value, window, cx);
                s
            })
        };
        let preset_name = form.kind.label();
        let name = field(&form.name, preset_name, false, window, cx);
        let base_url = field(&form.base_url, "http://localhost:11434/v1", false, window, cx);
        let model = field(&form.model, "model id", false, window, cx);
        let max_tokens = field(&form.max_tokens, "default", false, window, cx);
        let key = field(
            "",
            if editing { "leave empty to keep the stored key" } else { "paste the API key" },
            true,
            window,
            cx,
        );
        let mut subs = vec![];
        for input in [&name, &base_url, &model, &max_tokens, &key] {
            subs.push(cx.subscribe_in(input, window, |this, _, e: &InputEvent, window, cx| match e {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => this.save(window, cx),
                _ => {}
            }));
        }
        let focus = if editing { name.focus_handle(cx) } else { key.focus_handle(cx) };
        self.screen = Screen::Form(Box::new(FormView {
            form,
            base,
            name,
            base_url,
            model,
            max_tokens,
            key,
            status: None,
            busy: None,
            note: None,
            _subs: subs,
        }));
        self.dropdown = None;
        self.notice = None;
        window.focus(&focus, cx);
        self.lookup_status(window, cx);
        cx.notify();
    }

    /// Looks up the key's status for the form's account (the keyring may be slow).
    fn lookup_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(file) = self.file().cloned() else { return };
        let Screen::Form(f) = &mut self.screen else { return };
        f.status = None;
        let current = f.current(cx);
        let Ok(account) = current.account(&file, f.base.as_ref()).or_else(|_| {
            // Not complete yet (a custom account without its URL): look up
            // by the id it would get.
            Form { base_url: "http://x".into(), model: "x".into(), ..current }.account(&file, f.base.as_ref())
        }) else {
            return;
        };
        let keys = self.keys.clone();
        self.run(
            window,
            cx,
            move || keys.status(&account),
            move |this, status, _, _| {
                if let Screen::Form(f) = &mut this.screen {
                    f.status = Some(status);
                }
            },
        );
    }

    fn set_kind(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        let Screen::Form(f) = &mut self.screen else { return };
        if f.form.editing.is_some() || f.form.kind == kind {
            return;
        }
        f.form.kind = kind;
        let default = Form::new(kind).model;
        f.model.update(cx, |m, cx| m.set_value(default, window, cx));
        self.dropdown = None;
        self.lookup_status(window, cx);
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(file), Ok(path)) = (self.file().cloned(), self.path.clone()) else { return };
        let Screen::Form(f) = &mut self.screen else { return };
        if f.busy.is_some() {
            return;
        }
        let account = match f.current(cx).account(&file, f.base.as_ref()) {
            Ok(a) => a,
            Err(e) => {
                f.note = Note::bad(e);
                cx.notify();
                return;
            }
        };
        let key = f.key.read(cx).value().to_string();
        f.busy = Some("Saving…");
        f.note = None;
        let keys = self.keys.clone();
        self.run(
            window,
            cx,
            move || work::save_account(&path, &keys, account, Some(&key), false),
            |this, r, window, cx| match r {
                Ok(done) => this.finished(done, "Saved.", window, cx),
                Err(e) => {
                    if let Screen::Form(f) = &mut this.screen {
                        f.busy = None;
                        f.note = Note::bad(e);
                    }
                }
            },
        );
    }

    fn check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(file) = self.file().cloned() else { return };
        let Screen::Form(f) = &mut self.screen else { return };
        if f.busy.is_some() {
            return;
        }
        let account = match f.current(cx).account(&file, f.base.as_ref()) {
            Ok(a) => a,
            Err(e) => {
                f.note = Note::bad(e);
                cx.notify();
                return;
            }
        };
        let typed = f.key.read(cx).value().to_string();
        f.busy = Some("Checking…");
        f.note = None;
        let keys = self.keys.clone();
        self.run(
            window,
            cx,
            move || work::check(&keys, &account, Some(&typed)),
            |this, r, _, _| {
                if let Screen::Form(f) = &mut this.screen {
                    f.busy = None;
                    f.note = match r {
                        Ok(m) => Note::good(m),
                        Err(e) => Note::bad(e),
                    };
                }
            },
        );
    }

    fn remove_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Screen::Form(f) = &mut self.screen else { return };
        let (Some(base), None) = (f.base.clone(), f.busy) else { return };
        f.busy = Some("Removing the key…");
        f.note = None;
        let keys = self.keys.clone();
        self.run(
            window,
            cx,
            move || work::delete_key(&keys, &base),
            |this, r, _, cx| {
                if let Screen::Form(f) = &mut this.screen {
                    f.busy = None;
                    match r {
                        Ok(status) => {
                            f.status = Some(status);
                            f.note = Note::good("Key removed from the keychain.");
                            if let Loaded::Ready(s) = &mut this.loaded
                                && let Some(id) = &f.form.editing
                                && let Some(st) = f.status.clone()
                            {
                                s.statuses.insert(id.clone(), st);
                            }
                            accounts_changed(cx);
                        }
                        Err(e) => f.note = Note::bad(e),
                    }
                }
            },
        );
    }

    // ---- drop-downs -----------------------------------------------------------

    fn options(&self, dd: Dd, cx: &App) -> (Vec<String>, Option<usize>) {
        let Screen::Form(f) = &self.screen else { return (vec![], None) };
        match dd {
            Dd::Model => {
                let models: Vec<String> = f.form.kind.models().iter().map(|m| m.to_string()).collect();
                let now = f.model.read(cx).value();
                let at = models.iter().position(|m| *m == now.trim());
                (models, at)
            }
            Dd::Effort => (
                EFFORTS.iter().map(|e| form::effort_label(*e).to_string()).collect(),
                EFFORTS.iter().position(|e| *e == f.form.effort),
            ),
            Dd::Protocol => (
                PROTOCOLS.iter().map(|p| p.name().to_string()).collect(),
                PROTOCOLS.iter().position(|p| *p == f.form.protocol),
            ),
        }
    }

    fn set_dropdown(&mut self, dd: Dd, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if open {
            let (_, at) = self.options(dd, cx);
            self.highlight = at.unwrap_or(0);
            self.dropdown = Some(dd);
            // The list isn't drawn yet, so it can only take focus on the next turn.
            let list = self.lists[dd as usize].clone();
            window.defer(cx, move |window, cx| list.focus(window, cx));
        } else if self.dropdown == Some(dd) {
            self.dropdown = None;
        }
        cx.notify();
    }

    fn move_highlight(&mut self, dd: Dd, down: bool, cx: &mut Context<Self>) {
        let (options, _) = self.options(dd, cx);
        if options.is_empty() {
            return;
        }
        self.highlight =
            if down { (self.highlight + 1).min(options.len() - 1) } else { self.highlight.saturating_sub(1) };
        cx.notify();
    }

    /// Chooses option `ix`. The `Select` leaves focus on its list, which
    /// vanishes with the pick, so the trigger gets it back on a deferred
    /// call, after the `Select` has finished with it.
    fn pick(&mut self, dd: Dd, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (options, _) = self.options(dd, cx);
        self.dropdown = None;
        if let (Screen::Form(f), Some(choice)) = (&mut self.screen, options.get(ix)) {
            match dd {
                Dd::Model => f.model.update(cx, |m, cx| m.set_value(choice.clone(), window, cx)),
                Dd::Effort => f.form.effort = EFFORTS[ix],
                Dd::Protocol => f.form.protocol = PROTOCOLS[ix],
            }
        }
        let trigger = self.triggers[dd as usize].clone();
        window.defer(cx, move |window, cx| trigger.focus(window, cx));
        cx.notify();
    }

    fn dropdown_view(
        &self,
        dd: Dd,
        label: &'static str,
        current: String,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.dropdown == Some(dd);
        let (options, selected) = self.options(dd, cx);
        let me = cx.entity().downgrade();
        let (me_open, me_confirm) = (me.clone(), me);
        let ix = dd as usize;
        let focused = self.triggers[ix].is_focused(window);
        let popup = deferred(
            div()
                .id(("dd-list", ix))
                .track_focus(&self.lists[ix])
                .absolute()
                .top(px(30.))
                .right_0()
                .min_w(px(200.))
                .whitespace_nowrap()
                .max_h(px(220.))
                .overflow_y_scroll()
                .occlude()
                .py_1()
                .bg(PAPER)
                .border_1()
                .border_color(RULE)
                .rounded_md()
                .shadow_md()
                .on_action(cx.listener(move |this, _: &SelectDown, _, cx| this.move_highlight(dd, true, cx)))
                .on_action(cx.listener(move |this, _: &SelectUp, _, cx| this.move_highlight(dd, false, cx)))
                .children(options.iter().enumerate().map(|(i, text)| {
                    div()
                        .id(("dd-item", ix * 100 + i))
                        .px_2()
                        .py(px(3.))
                        .cursor_pointer()
                        .when(i == self.highlight, |d| d.bg(ACCENT_SOFT))
                        .when(Some(i) == selected, |d| d.font_weight(FontWeight::SEMIBOLD))
                        .hover(|d| d.bg(ACCENT_SOFT))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.pick(dd, i, window, cx)))
                        .child(text.clone())
                })),
        )
        .with_priority(1);
        let highlight = self.highlight;
        div()
            .relative()
            .child(
                Select::new(("dd", ix))
                    .open(open)
                    .focus_handle(&self.triggers[ix])
                    .content_focus_handle(&self.lists[ix])
                    .accessibility_label(label)
                    .accessibility_value(current.clone())
                    .on_open_change(move |open, window, cx| {
                        me_open.update(cx, |s, cx| s.set_dropdown(dd, open, window, cx)).ok();
                    })
                    .on_confirm(move |window, cx| {
                        me_confirm.update(cx, |s, cx| s.pick(dd, highlight, window, cx)).ok();
                    })
                    .child(
                        div()
                            .id(("dd-trigger", ix))
                            .h(px(28.))
                            .px_2()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .cursor_pointer()
                            .rounded_md()
                            .bg(PAPER)
                            .border_1()
                            .border_color(if open || focused { ACCENT } else { RULE })
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                let open = this.dropdown != Some(dd);
                                this.set_dropdown(dd, open, window, cx);
                            }))
                            .child(div().truncate().child(current))
                            .child(div().text_color(INK_3).child("▾")),
                    )
                    .when(open, |s| s.child(popup)),
            )
            .into_any_element()
    }

    // ---- drawing --------------------------------------------------------------

    fn input_box(&self, input: &Entity<InputState>, window: &Window, cx: &App) -> AnyElement {
        let focused = input.focus_handle(cx).contains_focused(window, cx);
        div()
            .h(px(28.))
            .px_2()
            .flex()
            .items_center()
            .rounded_md()
            .bg(PAPER)
            .border_1()
            .border_color(if focused { ACCENT } else { RULE })
            .child(div().w_full().child(Input::new(input)))
            .into_any_element()
    }

    fn list_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut col = div().flex().flex_col().gap_3();
        match &self.loaded {
            Loaded::Loading => col = col.child(muted("Loading accounts…")),
            Loaded::Failed(e) => {
                col = col.child(note_line(Tone::Bad, format!("Can't read the accounts: {e}")));
            }
            Loaded::Ready(snap) => {
                let file = &snap.file;
                if file.accounts.is_empty() {
                    col = col.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child("No accounts yet"))
                            .child(muted(
                                "Add an account so the agent can work on your script. It needs a provider and an \
                             API key; keys are kept in the system keychain, never in a file.",
                            )),
                    );
                }
                let default = file.default_account().map(|a| a.id.clone());
                for a in &file.accounts {
                    col = col.child(self.account_row(a, default.as_deref() == Some(&a.id), snap, cx));
                }
                if let Some(n) = self.notice.as_ref() {
                    col = col.child(note_line(n.tone, n.text.clone()));
                }
                col = col.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(label(if file.accounts.is_empty() {
                            "Add your first account"
                        } else {
                            "Add an account"
                        }))
                        .child(div().flex().flex_wrap().gap_2().children(Kind::ALL.iter().map(|&kind| {
                            button(("add", kind as usize), kind.label(), Btn::Plain).on_click(cx.listener(
                                move |this, _: &ClickEvent, window, cx| {
                                    this.open_form(Form::new(kind), None, window, cx);
                                },
                            ))
                        }))),
                );
                return col.into_any_element();
            }
        }
        if let Some(n) = self.notice.as_ref() {
            col = col.child(note_line(n.tone, n.text.clone()));
        }
        col.into_any_element()
    }

    fn account_row(&self, a: &Account, is_default: bool, snap: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let status = snap.statuses.get(&a.id);
        let (key_text, key_tone) = status
            .map(|s| form::key_short(s, a.provider.needs_key()))
            .unwrap_or_else(|| ("checking the key…".into(), Tone::Plain));
        let id = a.id.clone();
        let name = if a.name.trim().is_empty() { a.id.clone() } else { a.name.clone() };
        let confirming = self.confirm_remove.as_deref() == Some(a.id.as_str());
        let many = snap.file.accounts.len() > 1;
        let idx = snap.file.accounts.iter().position(|x| x.id == a.id).unwrap_or(0);
        let detail = format!("{} · {}", a.provider.name(), a.model.as_deref().unwrap_or("no model"));
        let (edit_acc, rm_id, def_id) = (a.clone(), id.clone(), id.clone());
        div()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(RULE)
            .bg(SIDE)
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(name))
                    .when(is_default, |d| {
                        d.child(
                            div()
                                .px_1()
                                .rounded_sm()
                                .bg(ACCENT_SOFT)
                                .text_color(ACCENT)
                                .text_size(px(11.))
                                .child("Default"),
                        )
                    }),
            )
            .child(div().text_color(INK_2).truncate().child(detail))
            .child(div().text_size(px(12.)).text_color(tone_color(key_tone)).child(format!("Key: {key_text}")))
            .children(a.warnings().into_iter().map(|w| div().text_size(px(12.)).text_color(WARN).child(w)))
            .child(if confirming {
                div()
                    .mt_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_color(WARN).child("Remove this account and its stored key?"))
                    .child(button(("rm-yes", idx), "Remove", Btn::Danger).on_click(
                        cx.listener(move |this, _: &ClickEvent, window, cx| this.remove(rm_id.clone(), window, cx)),
                    ))
                    .child(button(("rm-no", idx), "Keep", Btn::Plain).on_click(cx.listener(
                        |this, _: &ClickEvent, _, cx| {
                            this.confirm_remove = None;
                            cx.notify();
                        },
                    )))
            } else {
                div()
                    .mt_1()
                    .flex()
                    .gap_2()
                    .child(button(("edit", idx), "Edit", Btn::Plain).on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| {
                            this.open_form(Form::from_account(&edit_acc), Some(edit_acc.clone()), window, cx)
                        },
                    )))
                    .when(many && !is_default, |d| {
                        d.child(button(("default", idx), "Make default", Btn::Plain).on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| this.make_default(def_id.clone(), window, cx),
                        )))
                    })
                    .child(button(("remove", idx), "Remove", Btn::Plain).on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.confirm_remove = Some(id.clone());
                            cx.notify();
                        },
                    )))
            })
            .into_any_element()
    }

    fn form_view(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Screen::Form(f) = &self.screen else { return div().into_any_element() };
        let Some(file) = self.file() else { return div().into_any_element() };
        let current = f.current(cx);
        let kind = current.kind;
        let editing = current.editing.is_some();
        let needs_key = kind != Kind::Custom;
        let busy = f.busy;
        let warnings = current.warnings(file, f.base.as_ref());
        let key_status = f.status.as_ref().map(|s| form::key_long(s, needs_key));
        let keys_url = kind.keys_url();
        let has_stored_key = editing && matches!(f.status, Some(KeyStatus::Keyring));
        let effort_text = form::effort_label(current.effort).to_string();
        let protocol_text = current.protocol.name().to_string();

        let mut col = div().flex().flex_col().gap_3();
        col = col.child(div().font_weight(FontWeight::SEMIBOLD).child(if editing {
            format!("Edit {}", kind.label())
        } else {
            format!("Add {}", kind.label())
        }));
        if !editing {
            col = col.child(field(
                "Provider",
                div().flex().flex_wrap().gap_2().children(Kind::ALL.iter().map(|&k| {
                    button(("kind", k as usize), k.label(), if k == kind { Btn::Chosen } else { Btn::Plain })
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.set_kind(k, window, cx)))
                })),
            ));
        }
        col = col.child(field("Name", self.input_box(&f.name, window, cx)));
        if kind == Kind::Custom {
            col = col
                .child(field("Protocol", self.dropdown_view(Dd::Protocol, "Protocol", protocol_text, window, cx)))
                .child(field("Base URL", self.input_box(&f.base_url, window, cx)));
        }
        let mut model_row = div().flex().gap_2().child(div().flex_1().child(self.input_box(&f.model, window, cx)));
        if !kind.models().is_empty() {
            model_row = model_row.child(div().w(px(120.)).child(self.dropdown_view(
                Dd::Model,
                "Suggested models",
                "Presets".into(),
                window,
                cx,
            )));
        }
        col = col.child(field("Model", model_row)).child(
            div()
                .flex()
                .gap_3()
                .child(div().flex_1().child(field(
                    "Reasoning effort",
                    self.dropdown_view(Dd::Effort, "Reasoning effort", effort_text, window, cx),
                )))
                .child(div().w(px(120.)).child(field("Output limit", self.input_box(&f.max_tokens, window, cx)))),
        );
        let mut key_col = div().flex().flex_col().gap_1().child(self.input_box(&f.key, window, cx));
        key_col = key_col.child(match key_status {
            Some((text, tone)) => note_line(tone, text),
            None => muted("Looking up the key…"),
        });
        let mut key_actions = div().flex().gap_2();
        if let Some(url) = keys_url {
            key_actions = key_actions.child(
                button("get-key", "Get a key", Btn::Plain)
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.open_url(url))),
            );
        }
        if has_stored_key && busy.is_none() {
            key_actions = key_actions.child(
                button("remove-key", "Remove key", Btn::Plain)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.remove_key(window, cx))),
            );
        }
        col = col.child(field("API key", key_col.child(key_actions)));
        for w in warnings {
            col = col.child(note_line(Tone::Bad, w));
        }
        if let Some(n) = f.note.as_ref() {
            col = col.child(note_line(n.tone, n.text.clone()));
        }
        let idle = busy.is_none();
        col = col.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(if idle {
                    button("check", "Check", Btn::Plain)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.check(window, cx)))
                } else {
                    button("check", busy.unwrap_or(""), Btn::Off)
                })
                .child(div().flex_1())
                .child(
                    button("cancel", "Cancel", Btn::Plain)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.back_or_close(window, cx))),
                )
                .child(if idle {
                    button("save", "Save", Btn::Primary)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save(window, cx)))
                } else {
                    button("save", "Save", Btn::Off)
                }),
        );
        col.into_any_element()
    }
}

impl Focusable for AccountsSheet {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AccountsSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let body = match self.screen {
            Screen::List => self.list_view(cx),
            Screen::Form(_) => self.form_view(window, cx),
        };
        let path = match &self.path {
            Ok(p) => format!("Accounts file: {}", p.display()),
            Err(_) => String::new(),
        };
        Sheet::new(cx)
            .focus_handle(self.focus.clone())
            .request_close(move |window, cx| {
                this.update(cx, |s, cx| s.back_or_close(window, cx)).ok();
            })
            .overlay(div().absolute().inset_0().bg(rgba(0x1c1f2455)))
            .surface(
                div()
                    .absolute()
                    .right_0()
                    .top_0()
                    .h_full()
                    .w(px(440.))
                    .occlude()
                    .bg(PAPER)
                    .border_l_1()
                    .border_color(RULE)
                    .flex()
                    .flex_col()
                    .text_size(px(13.))
                    .text_color(INK)
                    .font_family(super::panel_font(cx))
                    .child(
                        div()
                            .flex_none()
                            .px_4()
                            .py_3()
                            .border_b_1()
                            .border_color(RULE)
                            .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child("AI accounts")),
                    )
                    .child(div().id("accounts-body").flex_1().min_h_0().overflow_y_scroll().p_4().child(body))
                    .child(
                        div()
                            .flex_none()
                            .px_4()
                            .py_2()
                            .border_t_1()
                            .border_color(RULE)
                            .text_size(px(11.))
                            .text_color(INK_3)
                            .child(div().truncate().child(path))
                            .child("Esc to close"),
                    ),
            )
    }
}

// ---- small pieces ---------------------------------------------------------------

fn tone_color(t: Tone) -> gpui::Rgba {
    match t {
        Tone::Good => OK,
        Tone::Plain => INK_2,
        Tone::Bad => WARN,
    }
}

fn muted(text: impl Into<SharedString>) -> gpui::Div {
    div().text_color(INK_2).child(text.into())
}

fn note_line(tone: Tone, text: String) -> gpui::Div {
    div().text_size(px(12.)).text_color(tone_color(tone)).child(text)
}

fn label(text: &'static str) -> gpui::Div {
    div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(INK_3).child(text.to_uppercase())
}

fn field(name: &'static str, content: impl IntoElement) -> gpui::Div {
    div().flex().flex_col().gap_1().child(label(name)).child(content)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Btn {
    Plain,
    Primary,
    Danger,
    /// The chosen one of a row of choices.
    Chosen,
    /// Not clickable right now.
    Off,
}

fn button(id: impl Into<gpui::ElementId>, text: &str, kind: Btn) -> gpui::Stateful<gpui::Div> {
    let (bg, fg, border) = match kind {
        Btn::Plain => (PAPER, INK, RULE),
        Btn::Primary => (ACCENT, PAPER, ACCENT),
        Btn::Danger => (WARN, PAPER, WARN),
        Btn::Chosen => (ACCENT_SOFT, ACCENT, ACCENT),
        Btn::Off => (SIDE, INK_3, RULE),
    };
    div()
        .id(id)
        .h(px(28.))
        .px_3()
        .flex()
        .items_center()
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_color(fg)
        .when(kind != Btn::Off, |d| d.cursor_pointer().hover(|h| h.opacity(0.85)))
        .child(text.to_string())
}
