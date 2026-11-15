//! U0 spike (docs/plans/chat-panel.md): gpui-base's Textarea, streamed
//! markdown TextView, Select and Sheet inside our own window, without Root
//! (or with it under `SCRIPTORIUM_U0_ROOT=1`). Opened instead of the
//! workspace when `SCRIPTORIUM_U0=1`. Throwaway: U1 builds the real panel.
//!
//! The window nests the real app's key contexts (`Workspace > ScriptView`)
//! around a stand-in script and an `AgentPanel > AgentComposer` column, so the
//! app's own bindings take part in key dispatch exactly as they will in U1.
//! Everything the spike observes (focus, actions, sends, the keymap table) is
//! appended to `$SCRIPTORIUM_U0_OUT` (and stderr) for the test script to read.

use std::io::Write as _;
use std::time::Duration;

use gpui::{
    App, Bounds, Context, Entity, FocusHandle, Focusable, KeyBinding, KeyContext, Keystroke, SharedString, Window,
    WindowBounds, WindowOptions, actions, anchored, deferred, div, prelude::*, px, size,
};
use gpui_base::input::{InputEvent, InputState, TextareaState};
use gpui_base::{Input, Sheet, TextView, TextViewState, Textarea};

use crate::theme;

actions!(u0, [FocusComposer, Send, ComposerEscape, FocusNext, Stop, NewThread, OpenSheet, FocusModel]);

/// Same convention as main.rs: Cmd on macOS, Ctrl elsewhere.
macro_rules! sec {
    ($k:literal) => {
        if cfg!(target_os = "macos") { concat!("cmd-", $k) } else { concat!("ctrl-", $k) }
    };
}

/// The panel's bindings as the plan specifies them, registered after both
/// `gpui_base::init` and the app's own `bindings()`.
pub fn bindings() -> Vec<KeyBinding> {
    let composer = Some("AgentComposer > Input");
    vec![
        KeyBinding::new(sec!("l"), FocusComposer, Some("Workspace")),
        KeyBinding::new(sec!("shift-a"), OpenSheet, Some("Workspace")),
        KeyBinding::new(sec!("m"), FocusModel, Some("Workspace")),
        KeyBinding::new(sec!("."), Stop, Some("AgentPanel")),
        KeyBinding::new(sec!("n"), NewThread, Some("AgentPanel")),
        // Overrides of gpui-base's `Input` keys, only inside the composer.
        KeyBinding::new("enter", Send, composer),
        KeyBinding::new(sec!("enter"), Send, composer),
        KeyBinding::new("escape", ComposerEscape, composer),
        KeyBinding::new("tab", FocusNext, composer),
        // gpui-base binds Ctrl+. to ToggleCodeActions inside `Input` on Linux.
        KeyBinding::new(sec!("."), Stop, composer),
        // gpui-base has only Ctrl+Y for redo on Linux; the plan promises Shift+Z.
        KeyBinding::new(sec!("shift-z"), gpui_base::input::Redo, composer),
    ]
}

fn out(line: &str) {
    eprintln!("u0: {line}");
    if let Some(path) = std::env::var_os("SCRIPTORIUM_U0_OUT")
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// What `key` resolves to with focus at the end of `stack`, through the live
/// keymap (gpui-base's bindings, then the app's, then the panel's).
fn resolve(cx: &App, key: &'static str, stack: &[&str]) -> String {
    let stack: Vec<KeyContext> = stack.iter().map(|c| KeyContext::parse(c).unwrap()).collect();
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let (found, _) = keymap.bindings_for_input(&[Keystroke::parse(key).unwrap()], &stack);
    found.first().map(|b| b.action().name().to_string()).unwrap_or_else(|| "-".into())
}

fn keymap_report(cx: &App) {
    let composer = ["Workspace", "ScriptView", "AgentPanel", "AgentComposer", "Input"];
    let key_field = ["Workspace", "ScriptView", "AgentPanel", "Sheet", "Input"];
    let editor = ["Workspace", "ScriptView", "ScriptEditor"];
    let review = ["Workspace", "ScriptView", "Review"];
    let mut keys = vec![
        "enter",
        "shift-enter",
        "escape",
        "tab",
        "shift-tab",
        "up",
        "backspace",
        "a",
        "1",
        "ctrl-tab",
        "ctrl-0",
        "ctrl-1",
    ];
    keys.extend([
        sec!("enter"),
        sec!("."),
        sec!("z"),
        sec!("shift-z"),
        sec!("y"),
        sec!("a"),
        sec!("c"),
        sec!("v"),
        sec!("f"),
        sec!("h"),
        sec!("l"),
        sec!("n"),
        sec!("o"),
        sec!("s"),
        sec!("q"),
    ]);
    out("keymap: key | composer | key field in sheet | script editor | review card");
    for k in keys {
        out(&format!(
            "keymap: {k} | {} | {} | {} | {}",
            resolve(cx, k, &composer),
            resolve(cx, k, &key_field),
            resolve(cx, k, &editor),
            resolve(cx, k, &review)
        ));
    }
}

const MODELS: [&str; 3] = ["claude-opus-4", "claude-sonnet-4", "gpt-5"];

/// Exercises headings, emphasis split across chunks, lists, code and a link.
const MARKDOWN: &str = "## Dialogue pass\n\nI read **scene 12** (INT. KITCHEN – NIGHT) and proposed *eight* changes to \
MARTA's lines. The main ones:\n\n1. Cut the repeated `\"I know\"` in her second speech.\n2. Move the beat about the \
letter **before** the kettle boils.\n3. Give TOMAS a line to answer her, so the scene turns.\n\n- Kept her last line \
as is.\n- Two proposals were refused: those lines changed while I worked.\n\n> Open the review card to decide them.\n\n\
```\nMARTA\nYou kept it. All this time.\n```\n\nSee [the review](https://example.com) when you're ready.";

struct Spike {
    script: FocusHandle,
    composer: Entity<TextareaState>,
    reply: Entity<TextViewState>,
    model_trigger: FocusHandle,
    model_list: FocusHandle,
    model_open: bool,
    model: usize,
    highlighted: usize,
    sheet_open: bool,
    sheet_focus: FocusHandle,
    key_field: Entity<InputState>,
    sent: Vec<String>,
    last: SharedString,
    streamed: usize,
}

impl Spike {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx).auto_grow(1, 10).placeholder("Ask about the script, or ask for changes…")
        });
        cx.subscribe(&composer, |this, _, e: &InputEvent, cx| {
            let name = match e {
                InputEvent::Change => return cx.notify(),
                // Reaching this means our Enter override lost to gpui-base's.
                InputEvent::PressEnter { secondary, shift } => {
                    format!("gpui-base PressEnter secondary={secondary} shift={shift}")
                }
                InputEvent::Focus => "composer Focus event".into(),
                InputEvent::Blur => "composer Blur event".into(),
            };
            this.note(name, cx);
        })
        .detach();
        let key_field = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("API key"));
        let reply = cx.new(|cx| TextViewState::markdown("", cx).selectable(true));
        let script = cx.focus_handle();
        script.focus(window, cx);
        let this = Self {
            script,
            composer,
            reply,
            model_trigger: cx.focus_handle(),
            model_list: cx.focus_handle(),
            model_open: false,
            model: 0,
            highlighted: 0,
            sheet_open: false,
            sheet_focus: cx.focus_handle(),
            key_field,
            sent: Vec::new(),
            last: "".into(),
            streamed: 0,
        };
        this.start_stream(cx);
        this
    }

    fn note(&mut self, what: String, cx: &mut Context<Self>) {
        out(&format!("action: {what}"));
        self.last = what.into();
        cx.notify();
    }

    /// Streams MARKDOWN in 9-byte chunks every 80 ms (splitting `**`, words
    /// and code fences mid-token), then hands over the full text with
    /// `set_text`, as the stored message replaces the streaming one.
    fn start_stream(&self, cx: &mut Context<Self>) {
        let delay: u64 =
            std::env::var("SCRIPTORIUM_U0_STREAM_DELAY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(80);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            out("stream: start");
            let mut i = 0;
            while i < MARKDOWN.len() {
                let mut j = (i + 9).min(MARKDOWN.len());
                while !MARKDOWN.is_char_boundary(j) {
                    j += 1;
                }
                let chunk = &MARKDOWN[i..j];
                let ok = this.update(cx, |this, cx| {
                    this.reply.update(cx, |r, cx| r.push_str(chunk, cx));
                    this.streamed = j;
                    cx.notify();
                });
                if ok.is_err() {
                    return;
                }
                i = j;
                cx.background_executor().timer(Duration::from_millis(delay)).await;
            }
            let _ = this.update(cx, |this, cx| {
                this.reply.update(cx, |r, cx| r.set_text(MARKDOWN, cx));
                out(&format!("stream: done, {} bytes", MARKDOWN.len()));
                cx.notify();
            });
        })
        .detach();
    }

    fn focus_name(&self, window: &Window, cx: &App) -> &'static str {
        if self.composer.focus_handle(cx).is_focused(window) {
            "composer"
        } else if self.key_field.focus_handle(cx).is_focused(window) {
            "key field"
        } else if self.script.is_focused(window) {
            "script"
        } else if self.model_trigger.is_focused(window) {
            "model trigger"
        } else if self.model_list.is_focused(window) {
            "model list"
        } else {
            "other"
        }
    }

    fn focus_composer(&mut self, _: &FocusComposer, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |c, cx| c.focus(window, cx));
        self.note("FocusComposer".into(), cx);
    }

    fn send(&mut self, _: &Send, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        if text.trim().is_empty() {
            self.note("Send (blank: nothing sent)".into(), cx);
            return;
        }
        self.composer.update(cx, |c, cx| c.set_value("", window, cx));
        out(&format!("sent: {text:?}"));
        self.sent.push(text);
        self.note("Send".into(), cx);
    }

    fn escape(&mut self, _: &ComposerEscape, window: &mut Window, cx: &mut Context<Self>) {
        self.script.focus(window, cx);
        self.note("ComposerEscape".into(), cx);
    }

    fn focus_next(&mut self, _: &FocusNext, window: &mut Window, cx: &mut Context<Self>) {
        self.model_trigger.focus(window, cx);
        self.note("FocusNext".into(), cx);
    }

    fn stop(&mut self, _: &Stop, _: &mut Window, cx: &mut Context<Self>) {
        self.note("Stop".into(), cx);
    }

    fn new_thread(&mut self, _: &NewThread, _: &mut Window, cx: &mut Context<Self>) {
        self.note("NewThread".into(), cx);
    }

    fn open_sheet(&mut self, _: &OpenSheet, window: &mut Window, cx: &mut Context<Self>) {
        self.sheet_open = true;
        self.key_field.update(cx, |k, cx| k.focus(window, cx));
        self.note("OpenSheet".into(), cx);
    }

    fn focus_model(&mut self, _: &FocusModel, window: &mut Window, cx: &mut Context<Self>) {
        self.model_trigger.focus(window, cx);
        self.note("FocusModel".into(), cx);
    }

    fn render_select(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let confirm = weak.clone();
        let mut select = gpui_base::Select::new("model")
            .focus_handle(&self.model_trigger)
            .content_focus_handle(&self.model_list)
            .open(self.model_open)
            .on_open_change(move |open, _, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.model_open = open;
                    this.highlighted = this.model;
                    this.note(format!("Select open={open}"), cx);
                });
            })
            .on_confirm(move |window, cx| {
                let _ = confirm.update(cx, |this, cx| {
                    this.model = this.highlighted;
                    this.model_open = false;
                    // Select focuses its content handle *after* on_confirm; the
                    // list is gone by then, and focus on an unrendered handle
                    // swallows every key in the window. Refocus afterwards.
                    let trigger = this.model_trigger.clone();
                    window.defer(cx, move |window, cx| trigger.focus(window, cx));
                    this.note(format!("Select picked {}", MODELS[this.model]), cx);
                });
            })
            .relative()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme::RULE)
            .child(format!("Anthropic · {} ▾", MODELS[self.model]));
        if self.model_open {
            let rows = MODELS
                .iter()
                .enumerate()
                .map(|(i, m)| div().px_2().py_1().when(i == self.highlighted, |d| d.bg(theme::ACCENT_SOFT)).child(*m));
            select = select.child(deferred(
                anchored().child(
                    div()
                        .track_focus(&self.model_list)
                        .on_action(cx.listener(|this, _: &gpui_base::actions::SelectDown, _, cx| {
                            this.highlighted = (this.highlighted + 1) % MODELS.len();
                            cx.notify();
                        }))
                        .on_action(cx.listener(|this, _: &gpui_base::actions::SelectUp, _, cx| {
                            this.highlighted = (this.highlighted + MODELS.len() - 1) % MODELS.len();
                            cx.notify();
                        }))
                        .mt_1()
                        .w(px(220.))
                        .bg(theme::PAPER)
                        .border_1()
                        .border_color(theme::RULE)
                        .rounded_md()
                        .shadow_md()
                        .children(rows),
                ),
            ));
        }
        select
    }
}

impl Render for Spike {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus = self.focus_name(window, cx);
        let value = self.composer.read(cx).value();
        let key_len = self.key_field.read(cx).value().chars().count();
        let status = format!(
            "focus: {focus} · last: {} · sent: {} · composer: {:?} · key chars: {key_len} · streamed: {}/{}",
            self.last,
            self.sent.len(),
            value.as_ref(),
            self.streamed,
            MARKDOWN.len()
        );
        let composer_focused = focus == "composer";
        div()
            .key_context("Workspace")
            .on_action(cx.listener(Self::focus_composer))
            .on_action(cx.listener(Self::open_sheet))
            .on_action(cx.listener(Self::focus_model))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::DESK)
            .text_color(theme::INK)
            .text_size(px(14.))
            .child(
                div()
                    .key_context("ScriptView")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(
                        div()
                            .id("script")
                            .key_context("ScriptEditor")
                            .track_focus(&self.script)
                            .flex_1()
                            .m_4()
                            .p_4()
                            .bg(theme::PAPER)
                            .when(focus == "script", |d| d.border_2().border_color(theme::ACCENT))
                            .child("Script stand-in. Ctrl+L focuses the composer; Escape there comes back here.")
                            .child(div().font_weight(gpui::FontWeight::BOLD).child("Bold check (div)"))
                            .child(div().italic().child("Italic check (div)")),
                    )
                    .child(
                        div()
                            .key_context("AgentPanel")
                            .on_action(cx.listener(Self::stop))
                            .on_action(cx.listener(Self::new_thread))
                            .w(px(360.))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .bg(theme::CHROME)
                            .border_l_1()
                            .border_color(theme::RULE)
                            .child(div().p_2().border_b_1().border_color(theme::RULE).child("Dialogue pass"))
                            .child(
                                div()
                                    .id("messages")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .p_3()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .children(self.sent.iter().map(|s| {
                                        div().p_2().rounded_md().bg(theme::ACCENT_SOFT).child(format!("You: {s}"))
                                    }))
                                    .child(TextView::new(&self.reply)),
                            )
                            .child(
                                div()
                                    .p_2()
                                    .border_t_1()
                                    .border_color(theme::RULE)
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(
                                        div()
                                            .id("composer")
                                            .key_context("AgentComposer")
                                            .on_action(cx.listener(Self::send))
                                            .on_action(cx.listener(Self::escape))
                                            .on_action(cx.listener(Self::focus_next))
                                            .p_2()
                                            .rounded_md()
                                            .bg(theme::PAPER)
                                            .border_1()
                                            .border_color(if composer_focused { theme::ACCENT } else { theme::RULE })
                                            .child(Textarea::new(&self.composer)),
                                    )
                                    .child(self.render_select(cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .py_1()
                    .bg(theme::SIDE)
                    .border_t_1()
                    .border_color(theme::RULE)
                    .text_color(theme::INK_2)
                    .text_size(px(12.))
                    .child(status),
            )
            .when(self.sheet_open, |d| {
                let weak = cx.entity().downgrade();
                d.child(
                    Sheet::new(cx)
                        .focus_handle(self.sheet_focus.clone())
                        .request_close(move |window, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.sheet_open = false;
                                this.script.focus(window, cx);
                                this.note("Sheet closed".into(), cx);
                            });
                        })
                        .overlay(div().absolute().inset_0().bg(gpui::rgba(0x1c1f2455)))
                        .surface(
                            div()
                                .absolute()
                                .right_0()
                                .top_0()
                                .h_full()
                                .w(px(420.))
                                .occlude()
                                .bg(theme::PAPER)
                                .p_4()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child("AI accounts")
                                .child(
                                    div()
                                        .p_2()
                                        .border_1()
                                        .border_color(theme::RULE)
                                        .rounded_md()
                                        .child(Input::new(&self.key_field)),
                                ),
                        ),
                )
            })
    }
}

/// Opens the spike window. Call after `gpui_base::init` and all `bind_keys`.
pub fn open(cx: &mut App) {
    keymap_report(cx);
    let with_root = std::env::var_os("SCRIPTORIUM_U0_ROOT").is_some();
    out(&format!("window: root={with_root}"));
    let bounds = Bounds::centered(None, size(px(1500.), px(1000.)), cx);
    let options = WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() };
    if with_root {
        cx.open_window(options, |window, cx| {
            let spike = cx.new(|cx| Spike::new(window, cx));
            cx.new(|cx| gpui_base::Root::new(spike, window, cx))
        })
        .map(|_| ())
    } else {
        cx.open_window(options, |window, cx| cx.new(|cx| Spike::new(window, cx))).map(|_| ())
    }
    .expect("spike window");
}
