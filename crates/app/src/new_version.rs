//! File › New version… (Cmd/Ctrl+T, plan step B3): a sheet over the window
//! asking for the version's language, the original's language and the name
//! it is saved as; "Create and open" makes the version as `scriptorium
//! version new` does (`ScriptView::make_version`) and the workspace opens it
//! with the original lined up on the left.
//!
//! The version starts with blank lines (scene headings and character names
//! copied): that is what `version new` makes. Starting from a copy of the
//! text (a new draft in the same language) is Phase F and not offered yet.
//! A script that already has a version is refused before the sheet opens
//! (one version per script for now). Problems are said in the status bar (and on the sheet's last
//! line, so they are seen), never in a dialog.
//!
//! Also the window's actions for the pair: [`MarkInSync`], [`MarkNotInSync`]
//! (the Edit menu) and [`Swap`] (the toolbar).

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, SharedString, Subscription, Window,
    actions, div, prelude::*, px, rgba,
};
use gpui_base::input::{InputEvent, InputState};
use gpui_base::{Input, Sheet};
use scriptorium_fsio::versions::default_version_name;
use scriptorium_versions::language_name;

use crate::theme::*;

actions!(versions, [NewVersion, MarkInSync, MarkNotInSync, Swap]);

/// What the sheet asks the workspace for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SheetEvent {
    Cancel,
    /// Make the version: its language code, the original's (if given), and
    /// its file name (if not the default).
    Create {
        lang: String,
        from: Option<String>,
        name: Option<String>,
    },
}

pub struct NewVersionSheet {
    focus: FocusHandle,
    /// The script the version is made of (its file name).
    script: String,
    lang: Entity<InputState>,
    from: Entity<InputState>,
    name: Entity<InputState>,
    /// Why the last Create failed (also said in the status bar).
    error: Option<SharedString>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SheetEvent> for NewVersionSheet {}

impl NewVersionSheet {
    /// For the script `script` (its file name).
    pub fn new(script: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let field = |value: &str, placeholder: String, window: &mut Window, cx: &mut Context<Self>| {
            let value = value.to_string();
            cx.new(|cx| {
                let mut s = InputState::new(window, cx).placeholder(placeholder);
                s.set_value(value, window, cx);
                s
            })
        };
        let lang = field("", "es, fr, pt-BR…".into(), window, cx);
        let from = field("", "en".into(), window, cx);
        let name = field("", default_version_name(&script, "es"), window, cx);
        let mut subs = vec![];
        for input in [&lang, &from, &name] {
            subs.push(cx.subscribe_in(input, window, |this, input, e: &InputEvent, window, cx| match e {
                InputEvent::Change => {
                    // The default name follows the language as it is typed.
                    if *input == this.lang {
                        let code = this.code(cx);
                        let default = default_version_name(&this.script, if code.is_empty() { "es" } else { &code });
                        this.name.update(cx, |s, cx| s.set_placeholder(default, window, cx));
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.create(cx),
                _ => {}
            }));
        }
        let into = lang.focus_handle(cx);
        window.focus(&into, cx);
        Self { focus: cx.focus_handle(), script, lang, from, name, error: None, _subs: subs }
    }

    fn code(&self, cx: &App) -> String {
        self.lang.read(cx).value().trim().to_string()
    }

    /// Say why Create failed, on the sheet (the workspace also says it in
    /// the status bar).
    pub fn set_error(&mut self, error: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.error = Some(error.into());
        cx.notify();
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        let text = |e: &Entity<InputState>| Some(e.read(cx).value().trim().to_string()).filter(|s| !s.is_empty());
        let (from, name) = (text(&self.from), text(&self.name));
        let lang = self.code(cx);
        if lang.is_empty() {
            self.set_error("Give the version's language: a code like es, fr or pt-BR.", cx);
            return;
        }
        self.error = None;
        cx.emit(SheetEvent::Create { lang, from, name });
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(SheetEvent::Cancel);
    }

    fn input_box(&self, input: &Entity<InputState>, window: &Window, cx: &App) -> gpui::Div {
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
    }
}

impl Focusable for NewVersionSheet {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// A field's label and what goes under it.
fn field(name: &'static str, input: impl IntoElement, hint: Option<String>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(INK_3).child(name.to_uppercase()))
        .child(input)
        .children(hint.map(|h| div().text_size(px(12.)).text_color(INK_2).child(h)))
}

impl Render for NewVersionSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let code = self.code(cx);
        let lang_hint = (!code.is_empty()).then(|| {
            let name = language_name(&code);
            if name == code {
                format!("{code}: not a language Scriptorium names; it is shown as typed")
            } else {
                name.to_string()
            }
        });
        let from_code = self.from.read(cx).value().trim().to_string();
        let from_hint = Some(if from_code.is_empty() {
            "Optional. Naming it labels the original's lines (\u{201c}English\u{201d}) everywhere.".to_string()
        } else {
            language_name(&from_code).to_string()
        });
        let body = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().text_color(INK_2).child(format!(
                "A new script lined up with {} row by row, saved beside it: scene headings and character \
                         names copied, every other line empty, of the same type, ready to translate.",
                self.script
            )))
            .child(field("Language", self.input_box(&self.lang, window, cx), lang_hint))
            .child(field("Original's language", self.input_box(&self.from, window, cx), from_hint))
            .child(field(
                "Saved as",
                self.input_box(&self.name, window, cx),
                Some("In the script's folder, with its links file beside it.".into()),
            ))
            .children(
                self.error
                    .clone()
                    .map(|e| div().text_size(px(12.)).text_color(WARN).font_weight(FontWeight::SEMIBOLD).child(e)),
            );
        let buttons = div()
            .flex()
            .justify_end()
            .gap_2()
            .child(
                crate::script_view::button("nv-cancel", "Cancel", Some("Esc"), false)
                    .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
            )
            .child(
                crate::script_view::button("nv-create", "Create and open", Some("\u{21b5}"), true)
                    .on_click(cx.listener(|this, _, _, cx| this.create(cx))),
            );
        Sheet::new(cx)
            .focus_handle(self.focus.clone())
            .request_close(move |_, cx| {
                this.update(cx, |s, cx| s.cancel(cx)).ok();
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
                    .child(
                        div()
                            .flex_none()
                            .px_4()
                            .py_3()
                            .border_b_1()
                            .border_color(RULE)
                            .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child("New version")),
                    )
                    .child(div().id("new-version-body").flex_1().min_h_0().overflow_y_scroll().p_4().child(body))
                    .child(div().flex_none().px_4().py_3().border_t_1().border_color(RULE).child(buttons)),
            )
    }
}
