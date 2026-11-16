//! The composer: the foot of the panel, where the writer types and sends.
//!
//! From the top: the run's notices and "files busy" line; one card at a time
//! (what the run asks — the whole-script read or a disclosure in the middle
//! of a run —, else the first-use disclosure, else "set up an AI account");
//! the text field with Send/Stop; the model chip and the usage line.
//!
//! The draft is never lost: it is cleared only once the driver has saved the
//! message ([`ChatEvent::MessageSaved`]), comes back when the model declines
//! it ([`ChatEvent::Withdrawn`]), and the disclosure card's Send sends the
//! draft as it is then.
//!
//! The text field's key context, `AgentComposer`, is what the panel's
//! composer key bindings match (`super::bindings`): Enter sends (the panel
//! forwards [`super::Send`] to [`Composer::send`]), Escape goes to
//! [`Composer::dismiss`] first. The free-text model field sits in the same
//! context, so Enter there chooses the model instead of sending.
//!
//! The logic the views show (the chip, the picker's entries, the cost) is in
//! the pure functions at the end, with their tests.

use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Rgba, Role,
    SharedString, Subscription, Window, deferred, div, prelude::*, px, rgba,
};
use gpui_base::actions::{SelectDown, SelectUp};
use gpui_base::input::{InputState, TextareaState};
use gpui_base::{Input, Select, Textarea};
use scriptorium_agent::Usage;
use scriptorium_agent::file::ThreadFile;
use scriptorium_chat::host;
use scriptorium_chat::labels;
use scriptorium_providers::shell::config;
use scriptorium_providers::{AccountsFile, PriceBook, model_name, reply_cost, thread_cost};

use super::accounts::AccountsChanged;
use super::driver::{Chat, ChatEvent, Question, READ_ONLY_NOTE, SendError};
use super::{OpenAccounts, Send};
use crate::theme::*;

const PLACEHOLDER: &str = "Ask about the script, or ask for changes…";
/// The placeholder of a conversation another program has open (the full
/// note shows under the field).
const READ_ONLY_PLACEHOLDER: &str = "Read only: open in another Scriptorium window or an AI agent.";

/// What the composer tells the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerEvent {
    /// The writer chose a model (its name for the writer): an error row
    /// asking to choose one can offer to try again with it.
    ModelChosen(String),
}

/// The accounts file as last read (on start, and when the accounts sheet
/// changes it).
type Loaded = Result<AccountsFile, String>;

pub struct Composer {
    input: Entity<TextareaState>,
    chat: Entity<Chat>,
    accounts: Loaded,
    /// The first-use disclosure shown in place of sending: (key, text).
    disclosure: Option<(String, String)>,
    /// The message last handed to the driver, until it is saved: what
    /// [`ChatEvent::MessageSaved`] takes out of the draft.
    sent: Option<String>,
    /// The model picker is open, with this entry highlighted.
    picker: bool,
    highlight: usize,
    /// The free-text model field, while open: for this account.
    other: Option<(String, Entity<InputState>)>,
    /// The field is off: the conversation is another program's.
    read_only: bool,
    focus: Focus,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ComposerEvent> for Composer {}

/// The composer's own focus stops.
struct Focus {
    send: FocusHandle,
    chip: FocusHandle,
    list: FocusHandle,
    yes: FocusHandle,
    no: FocusHandle,
    setup: FocusHandle,
}

impl Composer {
    pub fn new(chat: Entity<Chat>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 10).placeholder(PLACEHOLDER));
        let subs = vec![
            cx.observe_global::<AccountsChanged>(|this, cx| {
                this.accounts = load_accounts();
                cx.notify();
            }),
            cx.observe_in(&chat, window, |this, _, window, cx| {
                this.sync_read_only(window, cx);
                cx.notify();
            }),
            cx.subscribe_in(&chat, window, Self::on_chat_event),
        ];
        Self {
            input,
            chat,
            accounts: load_accounts(),
            disclosure: None,
            sent: None,
            picker: false,
            highlight: 0,
            other: None,
            read_only: false,
            focus: Focus {
                send: cx.focus_handle(),
                chip: cx.focus_handle(),
                list: cx.focus_handle(),
                yes: cx.focus_handle(),
                no: cx.focus_handle(),
                setup: cx.focus_handle(),
            },
            _subs: subs,
        }
    }

    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |c, cx| c.focus(window, cx));
    }

    pub fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.focus_handle(cx).contains_focused(window, cx)
    }

    /// The draft as typed.
    pub fn text(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    /// The composer's Tab stops, in order: the field, the card's buttons,
    /// Send/Stop, the model chip.
    pub fn tab_stops(&self, cx: &App) -> Vec<FocusHandle> {
        let mut stops = vec![self.focus_handle(cx)];
        match self.card(cx) {
            Card::Question(_) | Card::Disclose(_) | Card::ToolsChanged => {
                stops.extend([self.focus.no.clone(), self.focus.yes.clone()])
            }
            Card::Setup => stops.push(self.focus.setup.clone()),
            Card::None => {}
        }
        stops.push(self.focus.send.clone());
        match &self.other {
            Some((_, field)) => stops.push(field.focus_handle(cx)),
            None => stops.push(self.focus.chip.clone()),
        }
        stops
    }

    // ---- what the writer does -------------------------------------------------

    /// Enter or the Send button. While the agent works the writer can type
    /// but not send (Send is Stop). A card is answered with its own buttons
    /// only — the disclosure's "Agree and send", a question's — so a
    /// reflexive second Enter never agrees to anything.
    pub fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chat = self.chat.read(cx);
        if chat.is_running() || chat.question().is_some() {
            crate::log::info("agent composer: Enter while the agent works; not sent");
            return;
        }
        if self.disclosure.is_some() {
            crate::log::info("agent composer: Enter with the disclosure showing; not sent");
            return;
        }
        if self.read_only {
            // The field already says why; the notice is for a send that
            // finds the conversation taken (SendError::ReadOnly).
            return;
        }
        self.send_draft(window, cx);
    }

    /// Whether Send can send now: no card waits for an answer, there's an
    /// account to send with, and the conversation isn't another program's.
    fn can_send(&self, cx: &App) -> bool {
        self.disclosure.is_none()
            && !matches!(self.chip(cx), Chip::Setup)
            && self.accounts.is_ok()
            && !self.chat.read(cx).read_only()
    }

    /// A conversation another program has open can be read, not added to:
    /// the field is off and says why in place of the placeholder.
    fn sync_read_only(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let read_only = self.chat.read(cx).read_only();
        if read_only == self.read_only {
            return;
        }
        self.read_only = read_only;
        self.input.update(cx, |s, cx| {
            s.set_disabled(read_only, cx);
            s.set_placeholder(if read_only { READ_ONLY_PLACEHOLDER } else { PLACEHOLDER }, window, cx);
        });
    }

    /// Hands the draft to the driver; shows why it wasn't sent, if it wasn't.
    fn send_draft(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let text = self.text(cx);
        let sent = self.chat.update(cx, |c, cx| c.send(&text, cx));
        match sent {
            Ok(()) => {
                crate::log::info("agent composer: sent");
                self.disclosure = None;
                self.sent = Some(text);
            }
            Err(SendError::Blank) => {}
            Err(SendError::ReadOnly) => {
                crate::log::info("agent composer: not sent: read-only conversation");
                // Once: Enter again doesn't pile up the same line.
                if !self.chat.read(cx).notices().iter().any(|n| n == READ_ONLY_NOTE) {
                    self.chat.update(cx, |c, cx| c.notice(READ_ONLY_NOTE, cx));
                }
            }
            Err(SendError::Running) => {}
            Err(SendError::Disclose { key, text }) => {
                crate::log::info("agent composer: disclosure first");
                self.disclosure = Some((key, text));
            }
            // No account at all: the setup card already says what to do.
            Err(SendError::Setup(_)) if matches!(self.chip(cx), Chip::Setup) => {}
            Err(e) => {
                crate::log::info(&format!("agent composer: not sent: {e}"));
                self.chat.update(cx, |c, cx| c.notice(e.to_string(), cx));
            }
        }
        cx.notify();
    }

    /// The disclosure card's Send: remember it, then send the draft.
    fn accept_disclosure(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((key, _)) = self.disclosure.take() else { return };
        if let Err(e) = self.chat.update(cx, |c, _| c.accept_disclosure(&key)) {
            crate::log::warn(&format!("couldn't remember the disclosure: {e}"));
        }
        self.send_draft(window, cx);
    }

    /// Stop (the button, Cmd/Ctrl+.): answers a pending question "no" and
    /// stops the run.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.chat.update(cx, |c, cx| {
            if c.question().is_some() && !c.is_running() {
                c.answer(false, cx);
            }
            c.stop(cx);
        });
    }

    /// Escape: closes the model field, the picker or the disclosure card, in
    /// that order. `false`: nothing was open (focus goes back to the script).
    pub fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.other.take().is_some() {
            self.refocus_chip(window, cx);
        } else if self.picker {
            self.picker = false;
            self.refocus_chip(window, cx);
        } else if self.disclosure.take().is_none() {
            return false;
        }
        cx.notify();
        true
    }

    /// The panel closed, or another thread opened: the disclosure card,
    /// the picker and the model field go (the draft stays).
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.disclosure = None;
        self.picker = false;
        self.other = None;
        cx.notify();
    }

    /// Opens the accounts sheet from the chip or the picker. Deferred, with
    /// the chip focused first, so the sheet remembers the chip (not the
    /// picker's list, gone with the pick) as what to give focus back to.
    fn open_accounts_from_chip(&self, window: &mut Window, cx: &mut Context<Self>) {
        let chip = self.focus.chip.clone();
        window.defer(cx, move |window, cx| {
            chip.focus(window, cx);
            window.dispatch_action(Box::new(OpenAccounts), cx);
        });
    }

    /// Opens the model picker (also an error row's [Choose model]).
    pub fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.chip(cx), Chip::Setup) || self.accounts.is_err() {
            self.open_accounts_from_chip(window, cx);
            return;
        }
        self.other = None;
        self.set_picker(true, window, cx);
    }

    fn set_picker(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if open {
            let picks = self.picks(cx);
            let current = self.current(cx);
            self.highlight = picks.iter().position(|p| p.is(current.as_ref())).unwrap_or(0);
            self.picker = true;
            // The list isn't drawn yet, so it can only take focus on the next turn.
            let list = self.focus.list.clone();
            window.defer(cx, move |window, cx| list.focus(window, cx));
        } else {
            self.picker = false;
        }
        cx.notify();
    }

    fn move_highlight(&mut self, down: bool, cx: &mut Context<Self>) {
        let n = self.picks(cx).len();
        if n == 0 {
            return;
        }
        self.highlight = if down { (self.highlight + 1).min(n - 1) } else { self.highlight.saturating_sub(1) };
        cx.notify();
    }

    /// Chooses entry `ix`. The `Select` leaves focus on its list, which
    /// vanishes with the pick, so focus goes back on a deferred call, after
    /// the `Select` has finished with it.
    fn pick(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = false;
        match self.picks(cx).into_iter().nth(ix) {
            Some(Pick::Model { account, model, .. }) => {
                crate::log::info(&format!("agent composer: model {account} · {model}"));
                cx.emit(ComposerEvent::ModelChosen(friendly(&model)));
                self.chat.update(cx, |c, cx| c.choose(account, Some(model), cx));
                self.refocus_chip(window, cx);
            }
            Some(Pick::Other { account }) => {
                let now = self.current(cx).filter(|(a, _)| *a == account).map(|(_, m)| m).unwrap_or_default();
                let field = cx.new(|cx| {
                    let mut s = InputState::new(window, cx).placeholder("Model name, as the provider writes it");
                    s.set_value(now, window, cx);
                    s
                });
                let fh = field.focus_handle(cx);
                self.other = Some((account, field));
                window.defer(cx, move |window, cx| fh.focus(window, cx));
            }
            Some(Pick::Manage) => self.open_accounts_from_chip(window, cx),
            None => self.refocus_chip(window, cx),
        }
        cx.notify();
    }

    /// Enter in the free-text model field.
    fn choose_other(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((account, field)) = self.other.take() else { return };
        let model = field.read(cx).value().trim().to_string();
        if !model.is_empty() {
            crate::log::info(&format!("agent composer: model {account} · {model} (typed)"));
            cx.emit(ComposerEvent::ModelChosen(friendly(&model)));
            self.chat.update(cx, |c, cx| c.choose(account, Some(model), cx));
        }
        self.refocus_chip(window, cx);
        cx.notify();
    }

    fn refocus_chip(&self, window: &mut Window, cx: &mut Context<Self>) {
        let chip = self.focus.chip.clone();
        window.defer(cx, move |window, cx| chip.focus(window, cx));
    }

    fn on_chat_event(&mut self, _: &Entity<Chat>, e: &ChatEvent, window: &mut Window, cx: &mut Context<Self>) {
        match e {
            // Edits, not set_value, so the cursor stays where the writer
            // left it and composer undo still works.
            ChatEvent::MessageSaved => {
                let now = self.text(cx);
                if let Some(n) = self.sent.take().and_then(|sent| sent_prefix(&now, &sent)) {
                    self.input.update(cx, |s, cx| {
                        let at = s.selected_range();
                        s.set_selected_range(0..n, cx);
                        s.replace("", window, cx);
                        s.set_selected_range(at.start.saturating_sub(n)..at.end.saturating_sub(n), cx);
                    });
                }
            }
            ChatEvent::Withdrawn(text) => {
                self.sent = None;
                let back = if self.text(cx).trim().is_empty() { text.clone() } else { format!("{text}\n") };
                self.input.update(cx, |s, cx| {
                    s.set_selected_range(0..0, cx);
                    s.insert(back, window, cx);
                });
            }
            ChatEvent::ThreadChanged => {
                self.sent = None;
                self.reset(cx);
            }
            ChatEvent::Ended { .. } => self.sent = None,
            ChatEvent::Asking => {}
        }
        cx.notify();
    }

    // ---- what the views show --------------------------------------------------

    fn chip(&self, cx: &App) -> Chip {
        let chat = self.chat.read(cx);
        chip(&self.accounts, chat.thread(), chat.choice())
    }

    /// The account and model the next run uses, if known.
    fn current(&self, cx: &App) -> Option<(String, String)> {
        match self.chip(cx) {
            Chip::Ready { account, model, .. } => Some((account, model)),
            _ => None,
        }
    }

    fn picks(&self, cx: &App) -> Vec<Pick> {
        let current = self.current(cx);
        match &self.accounts {
            Ok(file) => picks(file, current.as_ref().map(|(a, m)| (a.as_str(), m.as_str()))),
            Err(_) => vec![Pick::Manage],
        }
    }

    fn card(&self, cx: &App) -> Card {
        let chat = self.chat.read(cx);
        if let Some(q) = chat.question() {
            Card::Question(q.clone())
        } else if let Some((_, text)) = &self.disclosure {
            Card::Disclose(text.clone())
        } else if chat.tools_changed() && !chat.is_running() {
            Card::ToolsChanged
        } else if (matches!(self.chip(cx), Chip::Setup) || self.accounts.is_err()) && !chat.is_running() {
            Card::Setup
        } else {
            Card::None
        }
    }

    /// The usage line, and whether it's a floor (a stopped reply's counts
    /// aren't reported).
    fn usage_line(&self, cx: &App) -> Option<(String, bool)> {
        let chat = self.chat.read(cx);
        let total = chat.usage();
        let thread = chat.thread();
        if thread.is_none() && total == Usage::default() {
            return None;
        }
        let floor = !total.known;
        let Ok(file) = &self.accounts else { return Some((labels::usage_text(&total, None), floor)) };
        let book = PriceBook::from_accounts(file);
        let price = host::settle_account(file, thread, chat.choice().map(|c| c.0), chat.choice().and_then(|c| c.1))
            .ok()
            .and_then(|r| book.price(&r.origin()));
        Some((labels::usage_text(&total, priced(&book, thread, &total, price.as_ref())), floor))
    }

    // ---- drawing --------------------------------------------------------------

    fn render_lines(&self, cx: &App) -> Option<AnyElement> {
        let chat = self.chat.read(cx);
        let mut lines: Vec<(String, bool)> = chat.notices().iter().map(|n| (n.clone(), false)).collect();
        if chat.waiting_for_files() {
            lines.push((FILES_BUSY.to_string(), true));
        }
        if lines.is_empty() {
            return None;
        }
        Some(
            div()
                .id("agent-notices")
                .flex()
                .flex_col()
                .gap_1()
                .text_size(px(12.))
                .children(lines.into_iter().map(|(text, busy)| {
                    div()
                        .flex()
                        .gap_1()
                        .text_color(if busy { OWN } else { INK_2 })
                        .when(busy, |d| d.child("⏳"))
                        .child(div().flex_1().min_w_0().child(text))
                }))
                .into_any_element(),
        )
    }

    fn render_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (title, body, yes, no, tone): (Option<String>, String, &str, &str, Rgba) = match self.card(cx) {
            Card::None => return None,
            Card::Setup => return Some(self.render_setup(cx)),
            Card::Disclose(text) => {
                let (title, body) = split_title(&text);
                (title, body, "Agree and send", "Not now", ACCENT)
            }
            Card::Question(q @ Question::Disclose(_)) => {
                let (title, body) = split_title(q.text());
                (title, body, "Agree and send", "Not now", ACCENT)
            }
            Card::ToolsChanged => (None, host::TOOLS_CHANGED.to_string(), "Continue", "New conversation", OWN),
            Card::Question(q @ Question::WholeScript(_)) => (
                None,
                format!("{} Reading scene by scene costs less, but the agent may miss things.", q.text()),
                "Let it read",
                "Scene by scene",
                OWN,
            ),
        };
        // A question arrives while the writer may be looking elsewhere:
        // assistive technology announces an alert.
        let alert = matches!(self.card(cx), Card::Question(_));
        let yes_btn = self.button("agent-card-yes", yes, true, &self.focus.yes, cx, |this, window, cx| {
            this.card_answer(true, window, cx)
        });
        let no_btn = self.button("agent-card-no", no, false, &self.focus.no, cx, |this, window, cx| {
            this.card_answer(false, window, cx)
        });
        Some(
            div()
                .id("agent-card")
                .p_3()
                .rounded_md()
                .bg(PAPER)
                .border_1()
                .border_color(tone)
                .flex()
                .flex_col()
                .gap_2()
                .text_size(px(12.5))
                .text_color(INK)
                .when(alert, |d| d.role(Role::Alert))
                .when_some(title, |d, t| d.child(div().font_weight(FontWeight::SEMIBOLD).child(t)))
                .child(div().text_color(INK_2).child(body))
                .child(div().flex().gap_2().justify_end().child(no_btn).child(yes_btn))
                .into_any_element(),
        )
    }

    /// No AI account yet, or the accounts file can't be read.
    fn render_setup(&self, cx: &mut Context<Self>) -> AnyElement {
        let problem = self.accounts.as_ref().err().cloned();
        let (title, body, label) = match &problem {
            None => (
                "Connect an AI to start chatting",
                "The agent runs on your own account with an AI company such as Anthropic or OpenAI. \
                 Add one with its API key, and you can ask about the script or for changes.",
                "Set up an AI account…",
            ),
            Some(_) => (
                "Your AI accounts couldn't be read",
                "Nothing has been changed. Open AI accounts to see what's wrong.",
                "Open AI accounts…",
            ),
        };
        let open = self.button("agent-setup", label, true, &self.focus.setup, cx, |this, window, cx| {
            // The sheet gives focus back to what had it: the text field,
            // as this button goes once an account is added.
            this.focus(window, cx);
            window.dispatch_action(Box::new(OpenAccounts), cx)
        });
        div()
            .id("agent-setup-card")
            .p_3()
            .rounded_md()
            .bg(PAPER)
            .border_1()
            .border_color(ACCENT)
            .flex()
            .flex_col()
            .gap_2()
            .text_size(px(12.5))
            .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
            .child(div().text_color(INK_2).child(body))
            .when_some(problem, |d, p| d.child(div().text_color(WARN).child(p)))
            .child(div().flex().justify_end().child(open))
            .into_any_element()
    }

    fn card_answer(&mut self, yes: bool, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.card(cx), Card::ToolsChanged) {
            crate::log::info(&format!("agent composer: older tools: {}", if yes { "continue" } else { "new" }));
            if let Err(e) = self.chat.update(cx, |c, cx| c.older_tools(yes, cx)) {
                self.chat.update(cx, |c, cx| c.notice(e, cx));
            }
        } else if self.chat.read(cx).question().is_some() {
            crate::log::info(&format!("agent composer: answered {}", if yes { "yes" } else { "no" }));
            self.chat.update(cx, |c, cx| c.answer(yes, cx));
        } else if yes {
            self.accept_disclosure(window, cx);
        } else {
            crate::log::info("agent composer: disclosure not now");
            self.disclosure = None;
        }
        self.focus(window, cx);
        cx.notify();
    }

    /// A button reached with Tab and pressed with Enter, Space or a click.
    fn button(
        &self,
        id: &'static str,
        text: &str,
        primary: bool,
        focus: &FocusHandle,
        cx: &mut Context<Self>,
        f: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        let (bg, fg, border) = if primary { (ACCENT, PAPER, ACCENT) } else { (PAPER, INK, RULE) };
        div()
            .id(id)
            .track_focus(&focus.clone().tab_stop(true))
            .h(px(26.))
            .px(px(12.))
            .flex()
            .items_center()
            .rounded_md()
            .border_1()
            .border_color(border)
            .bg(bg)
            .text_color(fg)
            .text_size(px(12.5))
            .cursor_pointer()
            .hover(|h| h.opacity(0.85))
            .focus_visible(|s| s.border_color(INK).border_2())
            .child(text.to_string())
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| f(this, window, cx)))
    }

    fn render_field(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let focused = self.is_focused(window, cx);
        let chat = self.chat.read(cx);
        let (running, stopping) = (chat.is_running(), chat.is_stopping());
        let action = if stopping {
            div()
                .id("agent-stopping")
                .track_focus(&self.focus.send.clone().tab_stop(true))
                .h(px(26.))
                .px(px(12.))
                .flex()
                .items_center()
                .text_size(px(12.5))
                .text_color(INK_3)
                .child("Stopping…")
        } else if running {
            self.button("agent-stop", "Stop", false, &self.focus.send, cx, |this, _, cx| this.stop(cx))
                .border_color(WARN)
                .text_color(WARN)
        } else if !self.can_send(cx) {
            // Answer the card above (or add an account) first.
            div()
                .id("agent-send-off")
                .track_focus(&self.focus.send.clone().tab_stop(true))
                .h(px(26.))
                .px(px(12.))
                .flex()
                .items_center()
                .rounded_md()
                .border_1()
                .border_color(RULE)
                .bg(SIDE)
                .text_color(INK_3)
                .text_size(px(12.5))
                .child("Send")
        } else {
            self.button("agent-send", "Send", true, &self.focus.send, cx, |this, window, cx| {
                this.send(window, cx);
                this.focus(window, cx);
            })
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .bg(PAPER)
            .border_1()
            .border_color(if focused { ACCENT } else { RULE })
            .child(div().id("agent-composer").key_context("AgentComposer").child(Textarea::new(&self.input)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.))
                            .text_color(INK_3)
                            // Read only: the notice above the field says why, once.
                            .truncate()
                            .when(!self.read_only, |d| {
                                d.child(if running {
                                    "You can type while the agent works."
                                } else {
                                    "Enter to send · Shift+Enter for a new line"
                                })
                            }),
                    )
                    .child(action),
            )
            .into_any_element()
    }

    fn render_footer(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let usage = self.usage_line(cx);
        let left = match &self.other {
            Some((_, field)) => self.render_other(field, window, cx),
            None => self.render_chip(window, cx),
        };
        // The model on its own line, so a long name isn't cut short; the
        // usage under it.
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .text_size(px(11.5))
            .child(div().flex().child(left))
            .when_some(usage, |d, (u, floor)| {
                d.child(
                    div()
                        .id("agent-usage")
                        .px_2()
                        .text_color(INK_3)
                        .whitespace_nowrap()
                        .truncate()
                        .child(u)
                        .when(floor, |d| d.tooltip(|_, cx| tip(cx, "A stopped reply's cost isn't reported."))),
                )
            })
            .into_any_element()
    }

    fn render_other(&self, field: &Entity<InputState>, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let focused = field.focus_handle(cx).contains_focused(window, cx);
        let input = div()
            .id("agent-model-other")
            .key_context("AgentComposer")
            .flex_1()
            .min_w_0()
            .h(px(26.))
            .px_2()
            .flex()
            .items_center()
            .rounded_md()
            .bg(PAPER)
            .border_1()
            .border_color(if focused { ACCENT } else { RULE })
            // Enter here chooses the typed model; it never sends the draft.
            .on_action(cx.listener(|this, _: &Send, window, cx| this.choose_other(window, cx)))
            .child(Input::new(field));
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(input)
            .child(div().px_2().text_color(INK_3).child("Enter to use it · Esc to cancel"))
            .into_any_element()
    }

    fn render_chip(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let chip = self.chip(cx);
        let several = self.accounts.as_ref().is_ok_and(|f| f.accounts.len() > 1);
        let text = chip_label(&chip, several);
        let warn = !matches!(chip, Chip::Ready { .. });
        let tooltip: Option<SharedString> = match &chip {
            Chip::Ready { model, .. } => Some(format!("Model: {model}").into()),
            Chip::Problem(p) => Some(p.clone().into()),
            Chip::Setup => None,
        };
        let open = self.picker;
        let focused = self.focus.chip.is_focused(window);
        let picks = self.picks(cx);
        let current = self.current(cx);
        let me = cx.entity().downgrade();
        let (me_open, me_confirm) = (me.clone(), me);
        let highlight = self.highlight;
        let popup = deferred(
            div()
                .id("agent-model-list")
                .track_focus(&self.focus.list)
                .absolute()
                .bottom(px(30.))
                .left_0()
                .min_w(px(240.))
                .max_w(px(330.))
                .max_h(px(320.))
                .overflow_y_scroll()
                .occlude()
                .py_1()
                .bg(PAPER)
                .border_1()
                .border_color(RULE)
                .rounded_md()
                .shadow_md()
                .text_size(px(12.5))
                .on_action(cx.listener(|this, _: &SelectDown, _, cx| this.move_highlight(true, cx)))
                .on_action(cx.listener(|this, _: &SelectUp, _, cx| this.move_highlight(false, cx)))
                .children(picks.iter().enumerate().map(|(i, p)| {
                    let chosen = p.is(current.as_ref());
                    div()
                        .id(("agent-model-item", i))
                        .px_2()
                        .py(px(3.))
                        .cursor_pointer()
                        .truncate()
                        .when(matches!(p, Pick::Other { .. }), |d| d.border_t_1().border_color(RULE).mt_1().pt_1())
                        .when(matches!(p, Pick::Manage), |d| d.text_color(ACCENT))
                        .when(i == highlight, |d| d.bg(ACCENT_SOFT))
                        .when(chosen, |d| d.font_weight(FontWeight::SEMIBOLD))
                        .hover(|d| d.bg(ACCENT_SOFT))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.pick(i, window, cx)))
                        .child(if chosen { format!("✓ {}", p.label()) } else { p.label() })
                })),
        )
        .with_priority(1);
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .child(
                Select::new("agent-model")
                    .open(open)
                    .focus_handle(&self.focus.chip)
                    .content_focus_handle(&self.focus.list)
                    .accessibility_label("Model")
                    .accessibility_value(text.clone())
                    .on_open_change(move |open, window, cx| {
                        me_open
                            .update(cx, |s, cx| {
                                if open && (matches!(s.chip(cx), Chip::Setup) || s.accounts.is_err()) {
                                    s.open_accounts_from_chip(window, cx);
                                } else {
                                    s.set_picker(open, window, cx)
                                }
                            })
                            .ok();
                    })
                    // The highlight as it is now, not as it was drawn.
                    .on_confirm(move |window, cx| {
                        me_confirm.update(cx, |s, cx| s.pick(s.highlight, window, cx)).ok();
                    })
                    .child(
                        div()
                            .id("agent-model-chip")
                            .h(px(24.))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_1()
                            .max_w_full()
                            .cursor_pointer()
                            .rounded_md()
                            .border_1()
                            .border_color(if open || focused { ACCENT } else { rgba(0x00000000) })
                            .text_color(if warn { WARN } else { INK_2 })
                            .hover(|d| d.bg(ACCENT_SOFT))
                            .when_some(tooltip, |d, t| d.tooltip(move |_, cx| tip(cx, t.clone())))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                if this.picker {
                                    this.set_picker(false, window, cx);
                                } else {
                                    this.open_picker(window, cx);
                                }
                            }))
                            .child(div().truncate().child(text))
                            .child(div().flex_none().text_color(INK_3).child("▾")),
                    )
                    .when(open, |s| s.child(popup)),
            )
            .into_any_element()
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let lines = self.render_lines(cx);
        let card = self.render_card(cx);
        let field = self.render_field(window, cx);
        let footer = self.render_footer(window, cx);
        div()
            .flex_none()
            .p_2()
            .border_t_1()
            .border_color(RULE)
            .flex()
            .flex_col()
            .gap_2()
            .children(lines)
            .children(card)
            .child(field)
            .child(footer)
    }
}

/// Shown while a tool call waits for the script's files.
const FILES_BUSY: &str = "Another program is saving the script's files; the agent will go on when it's done.";

fn load_accounts() -> Loaded {
    let path = config::default_path().ok_or_else(|| "There's no settings folder for AI accounts.".to_string())?;
    config::load(&path).map_err(|e| capitalized(&e.to_string()))
}

/// A small tooltip.
struct Tip(SharedString);

impl Render for Tip {
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

fn tip(cx: &mut App, text: impl Into<SharedString>) -> gpui::AnyView {
    let text = text.into();
    cx.new(|_| Tip(text)).into()
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| f.to_uppercase().collect::<String>() + c.as_str())
}

// ---- pure -------------------------------------------------------------------

/// The one card above the field.
#[derive(Debug, Clone, PartialEq)]
enum Card {
    None,
    /// The run asks (whole-script read, or a disclosure mid-run).
    Question(Question),
    /// The first-use disclosure, before sending.
    Disclose(String),
    /// The conversation was started with other tools ([`host::TOOLS_CHANGED`]).
    ToolsChanged,
    /// No AI account yet.
    Setup,
}

/// What the model chip shows.
#[derive(Debug, Clone, PartialEq)]
enum Chip {
    /// No accounts at all: the setup card shows.
    Setup,
    /// The accounts file can't be read, or the thread's account is gone…
    Problem(String),
    Ready {
        account: String,
        name: String,
        model: String,
    },
}

/// The chip for the next run: the chosen account and model, else the
/// thread's, else the default account's (as the driver settles them).
fn chip(accounts: &Loaded, thread: Option<&ThreadFile>, choice: Option<(&str, Option<&str>)>) -> Chip {
    let file = match accounts {
        Ok(f) => f,
        Err(e) => return Chip::Problem(e.clone()),
    };
    if file.accounts.is_empty() {
        return Chip::Setup;
    }
    match host::settle_account(file, thread, choice.map(|c| c.0), choice.and_then(|c| c.1)) {
        Ok(r) => {
            let name = file.get(&r.account).map_or_else(|| r.account.clone(), |a| a.name.clone());
            Chip::Ready { account: r.account, name, model: r.model }
        }
        Err(e) => Chip::Problem(capitalized(&e.to_string())),
    }
}

/// A model's name for the writer ("Claude Opus 5.5"), else its id.
fn friendly(model: &str) -> String {
    model_name(model).map_or_else(|| model.to_string(), String::from)
}

/// The chip's text: the model's name, after the account's when there are
/// several.
fn chip_label(chip: &Chip, several: bool) -> String {
    match chip {
        Chip::Setup => "No AI account".into(),
        Chip::Problem(_) => "Choose a model".into(),
        Chip::Ready { name, model, .. } if several => format!("{name} · {}", friendly(model)),
        Chip::Ready { model, .. } => friendly(model),
    }
}

/// An entry of the model picker.
#[derive(Debug, Clone, PartialEq)]
enum Pick {
    Model {
        account: String,
        name: String,
        model: String,
        several: bool,
    },
    /// "Other model…": type a model name for this account.
    Other {
        account: String,
    },
    Manage,
}

impl Pick {
    fn label(&self) -> String {
        match self {
            Pick::Model { name, model, several: true, .. } => format!("{name} · {}", friendly(model)),
            Pick::Model { model, .. } => friendly(model),
            Pick::Other { .. } => "Other model…".into(),
            Pick::Manage => "Manage accounts…".into(),
        }
    }

    fn is(&self, current: Option<&(String, String)>) -> bool {
        matches!((self, current), (Pick::Model { account, model, .. }, Some((a, m))) if account == a && model == m)
    }
}

/// Each account's preset models (its own default and the current model
/// first if the presets lack them), "Other model…" for the current account
/// (else the first), and "Manage accounts…".
fn picks(file: &AccountsFile, current: Option<(&str, &str)>) -> Vec<Pick> {
    let several = file.accounts.len() > 1;
    let mut out = vec![];
    for a in &file.accounts {
        let presets = a.provider.models();
        let mut models: Vec<String> = vec![];
        let extra = current.filter(|(acc, _)| *acc == a.id).map(|(_, m)| m).into_iter().chain(a.model.as_deref());
        for m in extra.filter(|m| !presets.contains(m)).chain(presets.iter().copied()) {
            if !m.is_empty() && !models.iter().any(|x| x == m) {
                models.push(m.to_string());
            }
        }
        out.extend(models.into_iter().map(|model| Pick::Model {
            account: a.id.clone(),
            name: a.name.clone(),
            model,
            several,
        }));
    }
    let other = current.map(|(a, _)| a.to_string()).or_else(|| file.accounts.first().map(|a| a.id.clone()));
    out.extend(other.map(|account| Pick::Other { account }));
    out.push(Pick::Manage);
    out
}

/// How many bytes at the start of the draft go once `sent` is saved: the
/// sent text and the blank after it, if the draft still starts with it
/// (the writer may have typed on); `None` if they changed it.
fn sent_prefix(now: &str, sent: &str) -> Option<usize> {
    let rest = now.strip_prefix(sent)?;
    Some(now.len() - rest.trim_start().len())
}

/// "What gets sent. Your messages…" → ("What gets sent.", "Your messages…").
fn split_title(text: &str) -> (Option<String>, String) {
    match text.split_once(". ") {
        Some((head, rest)) if head.split_whitespace().count() <= 4 => {
            (Some(format!("{head}.")), rest.trim().to_string())
        }
        _ => (None, text.to_string()),
    }
}

/// The price-table cost of `total`: the saved replies, each at its own
/// model's price, plus the request in flight (`total` less the thread's) at
/// `price`, the next run's. `None` when any part's price isn't known —
/// except after a stopped reply (`total` not known), where the parts that
/// are known make a floor.
fn priced(
    book: &PriceBook,
    thread: Option<&ThreadFile>,
    total: &Usage,
    price: Option<&scriptorium_providers::Price>,
) -> Option<f64> {
    let floor = !total.known;
    let (saved, base) = match thread {
        Some(t) => {
            let c = thread_cost(book, &t.thread);
            ((c.complete || floor).then_some(c.usd)?, t.thread.usage())
        }
        None => (0.0, Usage::default()),
    };
    let in_flight = Usage {
        input: total.input.saturating_sub(base.input),
        output: total.output.saturating_sub(base.output),
        cached: total.cached.saturating_sub(base.cached),
        cache_write: total.cache_write.saturating_sub(base.cache_write),
        reasoning: None,
        known: true,
        cost: None,
    };
    if in_flight.input + in_flight.output == 0 {
        return Some(saved);
    }
    match reply_cost(price, &in_flight) {
        Some(c) => Some(saved + c),
        None => floor.then_some(saved),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scriptorium_providers::{Account, Provider};

    fn file(accounts: Vec<Account>, default: Option<&str>) -> AccountsFile {
        let mut f = AccountsFile::default();
        for a in accounts {
            f.upsert(a).unwrap();
        }
        if let Some(d) = default {
            f.set_default(d).unwrap();
        }
        f
    }

    fn anthropic() -> Account {
        Account::new("anthropic", "Anthropic", Provider::Anthropic)
    }

    fn openai() -> Account {
        Account::new("openai", "OpenAI", Provider::Openai)
    }

    #[test]
    fn no_accounts_is_the_setup_state() {
        assert_eq!(chip(&Ok(AccountsFile::default()), None, None), Chip::Setup);
        assert!(matches!(chip(&Err("bad file".into()), None, None), Chip::Problem(e) if e == "bad file"));
    }

    #[test]
    fn chip_follows_choice_then_default() {
        let f = file(vec![anthropic(), openai()], Some("anthropic"));
        let default_model = Provider::Anthropic.default_model().unwrap().to_string();
        assert_eq!(
            chip(&Ok(f.clone()), None, None),
            Chip::Ready { account: "anthropic".into(), name: "Anthropic".into(), model: default_model }
        );
        let Chip::Ready { account, model, .. } = chip(&Ok(f.clone()), None, Some(("openai", Some("gpt-6-luna"))))
        else {
            panic!("not ready")
        };
        assert_eq!((account.as_str(), model.as_str()), ("openai", "gpt-6-luna"));
        // A removed account the writer chose reads as a problem, not a crash.
        assert!(matches!(chip(&Ok(f), None, Some(("gone", None))), Chip::Problem(_)));
    }

    #[test]
    fn picker_lists_presets_other_and_manage() {
        let f = file(vec![anthropic()], Some("anthropic"));
        let p = picks(&f, Some(("anthropic", "my-own-model")));
        // The current model leads when the presets lack it, then the presets.
        assert_eq!(p[0].label(), "my-own-model");
        for m in Provider::Anthropic.models() {
            assert!(p.iter().any(|x| matches!(x, Pick::Model { model, .. } if model == m)), "{m} missing");
        }
        // Names for the writer; one account: no account names in the labels.
        assert!(p.iter().any(|x| x.label() == "Claude Sonnet 5.5"));
        assert!(p.iter().all(|x| !x.label().contains(" · ")));
        assert_eq!(p[p.len() - 2], Pick::Other { account: "anthropic".into() });
        assert_eq!(p[p.len() - 1], Pick::Manage);
        // No duplicates.
        let models: Vec<_> = p.iter().map(Pick::label).collect();
        let mut dedup = models.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(models.len(), dedup.len());
    }

    #[test]
    fn picker_names_accounts_when_there_are_several() {
        let f = file(vec![anthropic(), openai()], Some("anthropic"));
        let p = picks(&f, Some(("openai", "gpt-6-luna")));
        assert!(p.iter().any(|x| x.label() == "OpenAI · GPT-6 Luna"));
        assert!(p.iter().any(|x| x.label().starts_with("Anthropic · ")));
        assert!(p.iter().any(|x| x.is(Some(&("openai".into(), "gpt-6-luna".into())))));
        // "Other model…" is for the current account.
        assert!(p.contains(&Pick::Other { account: "openai".into() }));
    }

    #[test]
    fn chip_names_the_model_for_the_writer() {
        let ready = |m: &str| Chip::Ready { account: "a".into(), name: "Anthropic".into(), model: m.into() };
        assert_eq!(chip_label(&ready("claude-opus-5-5"), false), "Claude Opus 5.5");
        assert_eq!(chip_label(&ready("claude-opus-5-5"), true), "Anthropic · Claude Opus 5.5");
        assert_eq!(chip_label(&ready("my-own-model"), false), "my-own-model");
        assert_eq!(chip_label(&Chip::Setup, false), "No AI account");
    }

    #[test]
    fn saved_message_leaves_only_what_was_typed_after() {
        assert_eq!(sent_prefix("Tighten scene 12", "Tighten scene 12"), Some(16));
        assert_eq!(sent_prefix("Tighten scene 12\nand 13", "Tighten scene 12"), Some(17));
        assert_eq!(sent_prefix("something else", "Tighten scene 12"), None);
        assert_eq!(sent_prefix("Café ☕ x", "Café ☕"), Some("Café ☕ ".len()));
    }

    #[test]
    fn disclosure_title_splits_off() {
        let (t, b) = split_title("What gets sent. Your messages go to api.anthropic.com.");
        assert_eq!(t.as_deref(), Some("What gets sent."));
        assert_eq!(b, "Your messages go to api.anthropic.com.");
        let (t, b) = split_title("The agent wants to read the whole script and then some more. Really.");
        assert_eq!(t, None);
        assert!(b.starts_with("The agent wants"));
    }

    #[test]
    fn cost_of_a_new_conversation_in_flight() {
        let book = PriceBook::builtin();
        let price = book.price(&scriptorium_agent::Origin::new("https://api.anthropic.com/v1", "claude-opus-5-5"));
        let total = Usage { input: 1_000_000, output: 0, known: true, ..Usage::default() };
        let c = priced(&book, None, &total, price.as_ref()).unwrap();
        assert!(c > 0.0);
        // No price for the model: unknown, never guessed.
        assert_eq!(priced(&book, None, &total, None), None);
        // Nothing in flight, nothing saved: free.
        assert_eq!(priced(&book, None, &Usage::default(), None), Some(0.0));
        // A stopped reply: what is known is a floor, never "unknown".
        let stopped = Usage { input: 1_000_000, known: false, ..Usage::default() };
        assert!(priced(&book, None, &stopped, price.as_ref()).unwrap() > 0.0);
        assert_eq!(priced(&book, None, &stopped, None), Some(0.0));
    }
}
