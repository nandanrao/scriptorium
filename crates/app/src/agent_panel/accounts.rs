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

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Global,
    SharedString, Subscription, Window, deferred, div, prelude::*, px, rgba,
};
use gpui_base::actions::{SelectDown, SelectUp};

use super::{FocusNext, FocusPrev};
use gpui_base::input::{InputEvent, InputState};
use gpui_base::{Input, Select, Sheet};
use scriptorium_providers::shell::{CancelToken, KeyStatus, Keys};
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
    /// The form as it was opened, to tell whether anything was changed.
    initial: Form,
    /// The account being edited, whose other settings carry over.
    base: Option<Account>,
    name: Entity<InputState>,
    base_url: Entity<InputState>,
    model: Entity<InputState>,
    max_tokens: Entity<InputState>,
    key: Entity<InputState>,
    /// The key's status; `None` while it is being looked up.
    status: Option<KeyStatus>,
    /// A Check in flight; cancelled when the form is left.
    check: Option<CancelToken>,
    note: Option<Note>,
    /// "More options" and "Advanced" are open.
    more: bool,
    advanced: bool,
    /// Asking whether to throw away unsaved changes.
    confirm_discard: bool,
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

    /// Something was entered or changed since the form was opened.
    fn dirty(&self, cx: &App) -> bool {
        self.current(cx) != self.initial || !self.key.read(cx).value().is_empty()
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
    /// Counts forms: a result that arrives for an earlier one is dropped.
    fid: u64,
    /// Counts key-status lookups, so only the latest one counts.
    status_seq: u64,
    /// The account whose "Remove" is waiting for a second click.
    confirm_remove: Option<String>,
    /// How the last change to the list went.
    notice: Option<Note>,
    /// A write is in flight (save, remove, default, key): only one at a
    /// time, so no two read-modify-write cycles overlap.
    busy: Option<&'static str>,
    dropdown: Option<Dd>,
    /// The highlighted option of the open drop-down.
    highlight: usize,
    triggers: [FocusHandle; 3],
    lists: [FocusHandle; 3],
    /// One focus handle per button, so Tab reaches them all.
    buttons: RefCell<HashMap<String, FocusHandle>>,
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
            fid: 0,
            status_seq: 0,
            confirm_remove: None,
            notice: None,
            busy: None,
            dropdown: None,
            highlight: 0,
            triggers: [cx.focus_handle(), cx.focus_handle(), cx.focus_handle()],
            lists: [cx.focus_handle(), cx.focus_handle(), cx.focus_handle()],
            buttons: RefCell::new(HashMap::new()),
        };
        if let Ok(path) = this.path.clone() {
            let keys = this.keys.clone();
            this.run(
                window,
                cx,
                move || work::snapshot(&path, &keys),
                |_| false,
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
    /// disk or the network), then `then` on the UI thread. When `changed`
    /// says the accounts were changed, [`accounts_changed`] fires first, even
    /// if the sheet was closed meanwhile.
    fn run<T: Send + 'static>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        work: impl FnOnce() -> T + Send + 'static,
        changed: fn(&T) -> bool,
        then: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let out = cx.background_executor().spawn(async move { work() }).await;
            if changed(&out) {
                cx.update(|_, cx| accounts_changed(cx)).ok();
            }
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

    /// The focus handle of the button called `key`.
    fn focus_for(&self, key: &str, cx: &App) -> FocusHandle {
        self.buttons.borrow_mut().entry(key.to_string()).or_insert_with(|| cx.focus_handle()).clone()
    }

    /// Focus the button called `key` once it has been drawn.
    fn focus_soon(&self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let fh = self.focus_for(key, cx);
        window.defer(cx, move |window, _| {
            window.on_next_frame(move |window, cx| fh.focus(window, cx));
        });
    }

    /// Leaves the form (so results still on their way to it are dropped, and
    /// a Check in flight is cancelled).
    fn leave_form(&mut self) {
        if let Screen::Form(f) = &self.screen
            && let Some(c) = &f.check
        {
            c.cancel();
        }
        self.fid += 1;
        self.status_seq += 1;
        self.screen = Screen::List;
        self.dropdown = None;
    }

    /// Escape and a click outside. From the list: closes the sheet (a
    /// waiting "Remove?" is cancelled first). From a form: back to the list,
    /// asking first when there are unsaved changes.
    fn back_or_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &mut self.screen {
            Screen::Form(f) => {
                if f.confirm_discard {
                    f.confirm_discard = false;
                } else if f.dirty(cx) {
                    f.confirm_discard = true;
                    self.focus_soon("keep", window, cx);
                } else {
                    self.leave_form();
                    window.focus(&self.focus, cx);
                }
                cx.notify();
            }
            Screen::List => {
                if self.confirm_remove.take().is_some() {
                    window.focus(&self.focus, cx);
                    cx.notify();
                } else {
                    self.close(cx);
                }
            }
        }
    }

    /// Takes a finished write: the new file, a note, and (for a write made
    /// from the form that is still open) back to the list.
    fn finished(&mut self, done: Done, ok: &str, form_gen: Option<u64>, window: &mut Window, cx: &mut Context<Self>) {
        self.busy = None;
        self.loaded = match done.snapshot {
            Ok(s) => Loaded::Ready(s),
            Err(e) => Loaded::Failed(format!("The change was saved, but the accounts couldn't be read back: {e}")),
        };
        self.notice = match done.problem {
            Some(p) => Note::bad(p),
            None => Note::good(ok),
        };
        self.confirm_remove = None;
        if form_gen == Some(self.fid) && matches!(self.screen, Screen::Form(_)) {
            self.leave_form();
            window.focus(&self.focus, cx);
        }
    }

    /// Why a write can't start now, if so.
    fn blocked(&mut self) -> bool {
        self.busy.is_some() || !matches!(self.loaded, Loaded::Ready(_))
    }

    // ---- the list -------------------------------------------------------------

    fn remove(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(path) = self.path.clone() else { return };
        if self.blocked() {
            return;
        }
        self.busy = Some("Removing…");
        let keys = self.keys.clone();
        self.run(
            window,
            cx,
            move || work::remove_account(&path, &keys, &id),
            |r| r.is_ok(),
            |this, r, window, cx| match r {
                Ok(done) => this.finished(done, "Account removed.", None, window, cx),
                Err(e) => {
                    this.busy = None;
                    this.notice = Note::bad(e);
                }
            },
        );
    }

    fn make_default(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(path) = self.path.clone() else { return };
        if self.blocked() {
            return;
        }
        self.busy = Some("Saving…");
        let keys = self.keys.clone();
        self.run(
            window,
            cx,
            move || work::make_default(&path, &keys, &id),
            |r| r.is_ok(),
            |this, r, window, cx| match r {
                Ok(done) => this.finished(done, "Default account changed.", None, window, cx),
                Err(e) => {
                    this.busy = None;
                    this.notice = Note::bad(e);
                }
            },
        );
    }

    // ---- the form -------------------------------------------------------------

    fn open_form(&mut self, form: Form, base: Option<Account>, window: &mut Window, cx: &mut Context<Self>) {
        let editing = base.is_some();
        let kind = form.kind;
        let field = |value: &str, placeholder: &str, masked: bool, window: &mut Window, cx: &mut Context<Self>| {
            let value = value.to_string();
            let placeholder = placeholder.to_string();
            cx.new(|cx| {
                let mut s = InputState::new(window, cx).placeholder(placeholder).masked(masked);
                s.set_value(value, window, cx);
                s
            })
        };
        let name = field(&form.name, kind.label(), false, window, cx);
        let base_url = field(&form.base_url, "http://localhost:11434/v1", false, window, cx);
        let model = field(&form.model, "model id", false, window, cx);
        let max_tokens = field(&form.max_tokens, "default", false, window, cx);
        // Masked only once there is something to hide, so the placeholder
        // never shows as dots.
        let key = field(
            "",
            if editing { "leave empty to keep the stored key" } else { "paste the API key" },
            false,
            window,
            cx,
        );
        let mut subs = vec![];
        for input in [&name, &base_url, &model, &max_tokens, &key] {
            subs.push(cx.subscribe_in(input, window, |this, input, e: &InputEvent, window, cx| match e {
                InputEvent::Change => {
                    if let Screen::Form(f) = &this.screen
                        && *input == f.key
                    {
                        let masked = !input.read(cx).value().is_empty();
                        input.update(cx, |s, cx| s.set_masked(masked, window, cx));
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.save(window, cx),
                _ => {}
            }));
        }
        let focus = if editing { name.focus_handle(cx) } else { key.focus_handle(cx) };
        self.leave_form();
        self.screen = Screen::Form(Box::new(FormView {
            initial: form.clone(),
            form,
            base,
            name,
            base_url,
            model,
            max_tokens,
            key,
            status: None,
            check: None,
            note: None,
            more: editing,
            advanced: false,
            confirm_discard: false,
            _subs: subs,
        }));
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
        self.status_seq += 1;
        let (fid, seq) = (self.fid, self.status_seq);
        let keys = self.keys.clone();
        self.run(
            window,
            cx,
            move || keys.status(&account),
            |_| false,
            move |this, status, _, _| {
                if let Screen::Form(f) = &mut this.screen
                    && this.fid == fid
                    && this.status_seq == seq
                {
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
        if self.busy.is_some() {
            if let Screen::Form(f) = &mut self.screen {
                f.note = Note::bad("Another change is still being saved. Try again in a moment.");
            }
            return;
        }
        let Screen::Form(f) = &mut self.screen else { return };
        let current = f.current(cx);
        let account = match current.account(&file, f.base.as_ref()) {
            Ok(a) => a,
            Err(e) => {
                f.note = Note::bad(e);
                cx.notify();
                return;
            }
        };
        let create = current.create_base();
        let key = f.key.read(cx).value().to_string();
        f.note = None;
        self.busy = Some("Saving…");
        let (keys, fid) = (self.keys.clone(), self.fid);
        self.run(
            window,
            cx,
            move || work::save_account(&path, &keys, account, create.as_deref(), Some(&key)),
            |r| r.is_ok(),
            move |this, r, window, cx| match r {
                Ok(done) => this.finished(done, "Saved.", Some(fid), window, cx),
                Err(e) => {
                    this.busy = None;
                    if let Screen::Form(f) = &mut this.screen
                        && this.fid == fid
                    {
                        f.note = Note::bad(e);
                    }
                }
            },
        );
    }

    fn check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(file) = self.file().cloned() else { return };
        let Screen::Form(f) = &mut self.screen else { return };
        if f.check.is_some() {
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
        let token = CancelToken::new();
        f.check = Some(token.clone());
        f.note = None;
        let (keys, fid) = (self.keys.clone(), self.fid);
        self.run(
            window,
            cx,
            move || work::check(&keys, &account, Some(&typed), &token),
            |_| false,
            move |this, r, _, _| {
                if let Screen::Form(f) = &mut this.screen
                    && this.fid == fid
                {
                    f.check = None;
                    f.note = match r {
                        Ok(m) => Note::good(m),
                        Err(e) => Note::bad(e),
                    };
                }
            },
        );
    }

    fn remove_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked() {
            return;
        }
        let Screen::Form(f) = &mut self.screen else { return };
        let Some(base) = f.base.clone() else { return };
        f.note = None;
        self.busy = Some("Removing the key…");
        let (keys, fid) = (self.keys.clone(), self.fid);
        self.run(
            window,
            cx,
            {
                let base = base.clone();
                move || work::delete_key(&keys, &base)
            },
            |r| r.is_ok(),
            move |this, r, _, _| {
                this.busy = None;
                let ok = match r {
                    Ok(status) => {
                        if let Loaded::Ready(s) = &mut this.loaded {
                            s.statuses.insert(base.id.clone(), status.clone());
                        }
                        Ok(status)
                    }
                    Err(e) => Err(e),
                };
                if let Screen::Form(f) = &mut this.screen
                    && this.fid == fid
                {
                    match ok {
                        Ok(status) => {
                            f.status = Some(status);
                            f.note = Note::good("Key removed from the keychain.");
                        }
                        Err(e) => f.note = Note::bad(e),
                    }
                }
            },
        );
    }

    // ---- drop-downs -----------------------------------------------------------

    /// The labels of a drop-down's options, and which one is current.
    fn options(&self, dd: Dd, cx: &App) -> (Vec<String>, Option<usize>) {
        let Screen::Form(f) = &self.screen else { return (vec![], None) };
        match dd {
            Dd::Model => {
                let ids = f.form.kind.models();
                let now = f.model.read(cx).value();
                (ids.iter().map(|m| form::model_label(m)).collect(), ids.iter().position(|m| *m == now.trim()))
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
        self.dropdown = None;
        if let Screen::Form(f) = &mut self.screen {
            match dd {
                Dd::Model => {
                    if let Some(id) = f.form.kind.models().get(ix) {
                        f.model.update(cx, |m, cx| m.set_value(id.to_string(), window, cx));
                    }
                }
                Dd::Effort => f.form.effort = EFFORTS.get(ix).copied().flatten(),
                Dd::Protocol => f.form.protocol = PROTOCOLS[ix.min(PROTOCOLS.len() - 1)],
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

    /// A button that can be reached with Tab and pressed with Enter or Space.
    fn kb(
        &self,
        key: &str,
        text: &str,
        kind: Btn,
        cx: &mut Context<Self>,
        f: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        let fh = self.focus_for(key, cx);
        let f = Rc::new(f);
        button(SharedString::from(key.to_string()), text, kind)
            .track_focus(&fh.tab_stop(true))
            .focus_visible(|s| s.border_color(ACCENT).border_2())
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                f(this, window, cx);
                cx.notify();
            }))
    }

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
                if let Some(b) = self.busy {
                    col = col.child(muted(b));
                } else if let Some(n) = self.notice.as_ref() {
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
                            self.kb(
                                &format!("add-{}", kind.id()),
                                kind.label(),
                                Btn::Plain,
                                cx,
                                move |this, window, cx| this.open_form(Form::new(kind), None, window, cx),
                            )
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
        let model = a.model.as_deref().map(form::model_label).unwrap_or_else(|| "no model".into());
        let detail = format!("{} · {}", a.provider.name(), model);
        let (edit_acc, rm_id, def_id, ask_id) = (a.clone(), id.clone(), id.clone(), id.clone());
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
            .children(form::plain_warnings(a).into_iter().map(|w| div().text_size(px(12.)).text_color(WARN).child(w)))
            .child(if confirming {
                div()
                    .mt_1()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(div().text_color(WARN).child("Remove this account and its stored key?"))
                    .child(self.kb(&format!("rm-yes-{id}"), "Remove", Btn::Danger, cx, move |this, window, cx| {
                        this.remove(rm_id.clone(), window, cx)
                    }))
                    .child(self.kb("rm-keep", "Keep", Btn::Plain, cx, |this, window, cx| {
                        this.confirm_remove = None;
                        window.focus(&this.focus, cx);
                    }))
            } else {
                div()
                    .mt_1()
                    .flex()
                    .gap_2()
                    .child(self.kb(&format!("edit-{id}"), "Edit", Btn::Plain, cx, move |this, window, cx| {
                        this.open_form(Form::from_account(&edit_acc), Some(edit_acc.clone()), window, cx)
                    }))
                    .when(many && !is_default, |d| {
                        d.child(self.kb(
                            &format!("default-{id}"),
                            "Make default",
                            Btn::Plain,
                            cx,
                            move |this, window, cx| this.make_default(def_id.clone(), window, cx),
                        ))
                    })
                    .child(self.kb(&format!("remove-{id}"), "Remove", Btn::Plain, cx, move |this, window, cx| {
                        this.confirm_remove = Some(ask_id.clone());
                        this.focus_soon("rm-keep", window, cx);
                    }))
            })
            .into_any_element()
    }

    fn form_view(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Screen::Form(f) = &self.screen else { return div().into_any_element() };
        let Some(file) = self.file() else { return div().into_any_element() };
        let current = f.current(cx);
        let kind = current.kind;
        let editing = current.editing.is_some();
        let custom = kind == Kind::Custom;
        let provider = kind.label();
        let warnings = current.warnings(file, f.base.as_ref());
        let has_stored_key = editing && matches!(f.status, Some(KeyStatus::Keyring));
        let no_key = matches!(f.status, Some(KeyStatus::Missing { keyring: None, .. })) && !custom;
        let effort_text = form::effort_label(current.effort).to_string();
        let protocol_text = current.protocol.name().to_string();
        let checking = f.check.is_some();
        let writing = self.busy;

        let mut col = div().flex().flex_col().gap_3();
        let title = if editing { format!("Edit {}", f.initial.name) } else { format!("Add {provider}") };
        col = col.child(div().font_weight(FontWeight::SEMIBOLD).child(title));

        if f.confirm_discard {
            col = col.child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(WARN_SOFT)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Discard your changes?"))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(self.kb("keep", "Keep editing", Btn::Primary, cx, |this, _, cx| {
                                if let Screen::Form(f) = &mut this.screen {
                                    f.confirm_discard = false;
                                }
                                cx.notify();
                            }))
                            .child(self.kb("discard", "Discard", Btn::Danger, cx, |this, window, cx| {
                                this.leave_form();
                                window.focus(&this.focus, cx);
                            })),
                    ),
            );
        }
        if !editing {
            col = col.child(field(
                "Provider",
                div().flex().flex_wrap().gap_2().children(Kind::ALL.iter().map(|&k| {
                    self.kb(
                        &format!("kind-{}", k.id()),
                        k.label(),
                        if k == kind { Btn::Chosen } else { Btn::Plain },
                        cx,
                        move |this, window, cx| this.set_kind(k, window, cx),
                    )
                })),
            ));
        }
        if custom {
            col = col
                .child(field("Protocol", self.dropdown_view(Dd::Protocol, "Protocol", protocol_text, window, cx)))
                .child(field("Base URL", self.input_box(&f.base_url, window, cx)))
                .child(field("Model", self.input_box(&f.model, window, cx)));
        }

        // The key.
        let (help, tone) = match &f.status {
            None => ("Looking up the key…".to_string(), Tone::Plain),
            Some(s) => form::key_long(s, !custom, provider),
        };
        let mut key_col = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(self.input_box(&f.key, window, cx))
            .child(note_line(if tone == Tone::Bad { Tone::Bad } else { Tone::Plain }, help));
        if no_key && let Some(url) = kind.keys_url() {
            key_col = key_col
                .child(div().mt_1().flex().child(self.kb(
                    "get-key",
                    "Get a key →",
                    Btn::Primary,
                    cx,
                    move |_, _, cx| cx.open_url(url),
                )))
                .child(note_line(
                    Tone::Plain,
                    format!(
                        "Create a key, copy it, paste it here. You'll need a paid account or credits with {provider}."
                    ),
                ));
        }
        if has_stored_key && writing.is_none() {
            key_col = key_col.child(div().flex().child(self.kb(
                "remove-key",
                "Remove key",
                Btn::Plain,
                cx,
                |this, window, cx| this.remove_key(window, cx),
            )));
        }
        col = col.child(field("API key", key_col));

        // More options.
        col = col.child(self.kb(
            "more",
            if f.more { "▾ More options" } else { "▸ More options" },
            Btn::Link,
            cx,
            |this, _, _| {
                if let Screen::Form(f) = &mut this.screen {
                    f.more = !f.more;
                }
            },
        ));
        if f.more {
            col = col.child(field("Name", self.input_box(&f.name, window, cx)));
            if !custom {
                let mut model_row =
                    div().flex().gap_2().child(div().flex_1().child(self.input_box(&f.model, window, cx)));
                model_row = model_row.child(div().w(px(120.)).child(self.dropdown_view(
                    Dd::Model,
                    "Suggested models",
                    "Presets".into(),
                    window,
                    cx,
                )));
                col = col.child(field("Model", model_row));
            }
            col = col.child(
                div()
                    .flex()
                    .gap_3()
                    .child(div().flex_1().child(field(
                        "Reasoning effort",
                        self.dropdown_view(Dd::Effort, "Reasoning effort", effort_text, window, cx),
                    )))
                    .child(div().w(px(120.)).child(field("Output limit", self.input_box(&f.max_tokens, window, cx)))),
            );
            col = col.child(self.kb(
                "advanced",
                if f.advanced { "▾ Advanced" } else { "▸ Advanced" },
                Btn::Link,
                cx,
                |this, _, _| {
                    if let Screen::Form(f) = &mut this.screen {
                        f.advanced = !f.advanced;
                    }
                },
            ));
            if f.advanced {
                col = col.child(note_line(
                    Tone::Plain,
                    form::key_advanced(&current.env_vars(file, f.base.as_ref()), f.status.as_ref()),
                ));
            }
        }
        for w in warnings {
            col = col.child(note_line(Tone::Bad, w));
        }
        if let Some(n) = f.note.as_ref() {
            col = col.child(note_line(n.tone, n.text.clone()));
        }
        col = col.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(if checking {
                    button("check", "Checking…", Btn::Off)
                } else {
                    self.kb("check", "Check", Btn::Plain, cx, |this, window, cx| this.check(window, cx))
                })
                .child(div().flex_1())
                .child(self.kb("cancel", "Cancel", Btn::Plain, cx, |this, window, cx| this.back_or_close(window, cx)))
                .child(match writing {
                    Some(label) => button("save", label, Btn::Off),
                    None => self.kb("save", "Save", Btn::Primary, cx, |this, window, cx| this.save(window, cx)),
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
        let sheet = Sheet::new(cx)
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
                            .child("Esc to close"),
                    ),
            );
        // The key context is on an ancestor of the sheet's own focus handle too,
        // so Tab moves on from there as well.
        div()
            .key_context("AccountsSheet")
            .on_action(cx.listener(|_, _: &FocusNext, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &FocusPrev, window, cx| window.focus_prev(cx)))
            .child(sheet)
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
    /// Text only: a disclosure.
    Link,
    /// Not clickable right now.
    Off,
}

fn button(id: impl Into<gpui::ElementId>, text: &str, kind: Btn) -> gpui::Stateful<gpui::Div> {
    let (bg, fg, border) = match kind {
        Btn::Plain => (PAPER, INK, RULE),
        Btn::Primary => (ACCENT, PAPER, ACCENT),
        Btn::Danger => (WARN, PAPER, WARN),
        Btn::Chosen => (ACCENT_SOFT, ACCENT, ACCENT),
        Btn::Link => (rgba(0x00000000), ACCENT, rgba(0x00000000)),
        Btn::Off => (SIDE, INK_3, RULE),
    };
    div()
        .id(id)
        .h(px(28.))
        .px(px(if kind == Btn::Link { 0. } else { 12. }))
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
