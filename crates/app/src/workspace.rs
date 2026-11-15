//! The window: a toolbar (with the File dropdown on Linux), the open script
//! (a `ScriptView`) or the empty state, notices, and dropping files.
//!
//! There's one window, holding at most one script. File › Open saves the
//! current script's session and replaces it; File › Add review attaches review
//! files to it. Menu and toolbar actions are handled here at the root, so they
//! work whatever has focus (macOS menu items fire with nothing focused).

use std::path::PathBuf;

use gpui::{
    App, Context, Div, Entity, ExternalPaths, FocusHandle, Focusable, FontWeight, KeyDownEvent, MouseButton,
    PathPromptOptions, SharedString, Stateful, Subscription, Window, actions, deferred, div, prelude::*, px,
};

use crate::agent_panel::accounts::{AccountsSheet, Dismissed};
use crate::agent_panel::{OpenAccounts, ToggleAgentPanel};
use crate::menus::{self, Item};
use crate::script_view::{
    ApplyToFdx, NextView, Opened, PrevView, RedoAny, SaveAndQuit, ScriptView, ShowReview1, ShowReview2, ShowReview3,
    ShowScript, UndoAny, file_name, has_extension,
};
use crate::theme::*;

actions!(scriptorium, [Open, AddReview, About]);

/// Test hook: when set, file pickers don't open; they return these paths
/// (separated by `:`) instead. For driving the app headless, where the
/// desktop portal can't show a dialog.
const PICK_ENV: &str = "SCRIPTORIUM_TEST_PICK";

pub struct Workspace {
    view: Option<Entity<ScriptView>>,
    focus: FocusHandle,
    /// The Linux File dropdown, and what had focus before it opened.
    menu: Option<Option<FocusHandle>>,
    menu_focus: FocusHandle,
    /// Shown in the status bar when no script is open (otherwise the script
    /// view's status bar shows notices).
    notice: SharedString,
    /// The AI accounts sheet while it's open, and what had focus before.
    accounts: Option<(Entity<AccountsSheet>, Option<FocusHandle>, Subscription)>,
    _observe: Option<Subscription>,
}

impl Workspace {
    pub fn new(script: Option<PathBuf>, reviews: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            view: None,
            focus: cx.focus_handle(),
            menu: None,
            menu_focus: cx.focus_handle(),
            notice: "".into(),
            accounts: None,
            _observe: None,
        };
        // Closing the window saves, like Quit.
        let ws = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            ws.update(cx, |w, cx| w.with_view(cx, |v, cx| _ = v.save_now(cx))).ok();
            true
        });
        match script {
            Some(p) => this.open_script(p, reviews, window, cx),
            None => window.focus(&this.focus, cx),
        }
        this
    }

    fn set_notice(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let text = text.into();
        match &self.view {
            Some(v) => v.update(cx, |v, cx| v.set_notice(text, cx)),
            None => self.notice = text,
        }
        cx.notify();
    }

    // ---- opening -------------------------------------------------------------

    /// Ask for files (or take them from the test hook), then run `then`.
    fn pick(
        &mut self,
        multiple: bool,
        prompt: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, Vec<PathBuf>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        if let Some(paths) = std::env::var_os(PICK_ENV) {
            let paths: Vec<PathBuf> = std::env::split_paths(&paths).collect();
            then(self, paths, window, cx);
            return;
        }
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple,
            prompt: Some(prompt.to_string().into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let picked = rx.await;
            this.update_in(cx, |this, window, cx| match picked {
                Ok(Ok(Some(paths))) if !paths.is_empty() => then(this, paths, window, cx),
                Ok(Ok(_)) | Err(_) => {} // cancelled
                Ok(Err(e)) => {
                    crate::log::warn(&format!("file picker: {e:#}"));
                    this.set_notice(
                        "Couldn't open a file picker (no desktop portal?). Pass the path on the command line \
                         instead, or drop the file on the window.",
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    fn on_open(&mut self, _: &Open, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        self.pick(false, "Open", window, cx, |this, paths, window, cx| {
            this.open_script(paths[0].clone(), vec![], window, cx);
        });
    }

    fn on_add_review(&mut self, _: &AddReview, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        if self.view.is_none() {
            self.set_notice("Open a script first, then add reviews to it.", cx);
            return;
        }
        self.pick(true, "Add review", window, cx, |this, paths, window, cx| {
            this.add_reviews(paths, window, cx);
        });
    }

    /// Open `path` in place of the current script (whose session is saved first).
    fn open_script(&mut self, path: PathBuf, reviews: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if !has_extension(&path, "fdx") {
            self.set_notice(format!("{} isn't a Final Draft script (they end in .fdx).", file_name(&path)), cx);
            return;
        }
        // Save first: reopening the same script must read what's on screen
        // (a pending autosave or a rewrite in progress), not an older session.
        let set_aside = self.view.clone().and_then(|old| old.update(cx, |v, cx| v.save_now(cx)));
        let opened = match Opened::load(&path, &reviews) {
            Ok(o) => o,
            Err(e) => {
                // The current script stays open.
                crate::log::warn(&format!("opening {}: {e}", path.display()));
                self.set_notice(set_aside.map_or(e.clone(), |a| format!("{e}  {a}")), cx);
                return;
            }
        };
        self.view = None;
        crate::log::info(&format!("opened {}", opened.script_path.display()));
        window.set_window_title(&format!("{} — Scriptorium", file_name(&opened.script_path)));
        let view = cx.new(|cx| ScriptView::new(opened, window, cx));
        // The toolbar shows the view's state, so redraw when it changes.
        self._observe = Some(cx.observe(&view, |_, _, cx| cx.notify()));
        self.view = Some(view);
        self.notice = "".into();
        if let Some(a) = set_aside {
            self.set_notice(a, cx);
        }
        cx.notify();
    }

    fn add_reviews(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(v) = &self.view {
            v.update(cx, |v, cx| v.add_reviews(&paths, window, cx));
        }
    }

    /// Dropped files: a `.fdx` opens (one), `.txt` files are added as reviews.
    fn dropped(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        let paths = paths.paths();
        let script = paths.iter().find(|p| has_extension(p, "fdx")).cloned();
        let reviews: Vec<PathBuf> = paths.iter().filter(|p| has_extension(p, "txt")).cloned().collect();
        let others: Vec<String> = paths
            .iter()
            .filter(|p| !has_extension(p, "fdx") && !has_extension(p, "txt"))
            .map(|p| file_name(p))
            .collect();
        match script {
            Some(s) => self.open_script(s, reviews, window, cx),
            None if !reviews.is_empty() && self.view.is_some() => self.add_reviews(reviews, window, cx),
            None if !reviews.is_empty() => self.set_notice("Open a script first, then drop reviews on it.", cx),
            None => {}
        }
        if !others.is_empty() {
            self.set_notice(
                format!("Not opened: {}. Drop a script (.fdx) or review files (.txt).", others.join(", ")),
                cx,
            );
        }
    }

    // ---- actions forwarded to the script -------------------------------------

    fn with_view(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut ScriptView, &mut Context<ScriptView>)) {
        if let Some(v) = self.view.clone() {
            v.update(cx, f);
        }
    }

    fn on_apply(&mut self, a: &ApplyToFdx, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        self.with_view(cx, |v, cx| v.apply_to_fdx(a, window, cx));
    }

    fn on_quit(&mut self, _: &SaveAndQuit, _: &mut Window, cx: &mut Context<Self>) {
        self.with_view(cx, |v, cx| _ = v.save_now(cx));
        cx.quit();
    }

    fn on_about(&mut self, _: &About, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        let log = crate::log::path().map_or("none".into(), |p| p.display().to_string());
        self.set_notice(format!("Scriptorium {} · log file: {log}", env!("CARGO_PKG_VERSION")), cx);
    }

    // ---- the agent panel and AI accounts ---------------------------------------

    fn on_toggle_agent(&mut self, _: &ToggleAgentPanel, window: &mut Window, cx: &mut Context<Self>) {
        // Under the accounts sheet, keys stay with the sheet.
        if self.accounts.is_some() {
            return;
        }
        self.close_menu(window, cx);
        if self.view.is_none() {
            self.set_notice("Open a script to chat about it.", cx);
            return;
        }
        self.with_view(cx, |v, cx| v.toggle_agent(window, cx));
    }

    /// The accounts are the user's, not the script's, so the sheet is drawn
    /// here over the whole window and opens with no script too.
    fn on_open_accounts(&mut self, _: &OpenAccounts, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        if self.accounts.is_some() {
            return;
        }
        let back = window.focused(cx);
        let sheet = cx.new(|cx| AccountsSheet::new(window, cx));
        let sub = cx.subscribe_in(&sheet, window, |this, _, _: &Dismissed, window, cx| {
            if let Some((_, back, _)) = this.accounts.take() {
                window.focus(&back.unwrap_or_else(|| this.focus.clone()), cx);
            }
            cx.notify();
        });
        let into = sheet.focus_handle(cx);
        window.focus(&into, cx);
        self.accounts = Some((sheet, back, sub));
        cx.notify();
    }

    // ---- the Linux File dropdown ---------------------------------------------

    fn toggle_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.is_some() {
            self.close_menu(window, cx);
        } else {
            self.menu = Some(window.focused(cx));
            window.focus(&self.menu_focus, cx);
            cx.notify();
        }
    }

    /// Gives focus back to what had it when the menu opened. Menu items call
    /// this before dispatching their action, and `dispatch_action` goes to the
    /// element focused when it is called, so an item acts where the writer
    /// was (Edit › Undo with the composer focused undoes the composer).
    fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(prev) = self.menu.take() {
            match prev {
                Some(f) => window.focus(&f, cx),
                None => window.focus(&self.focus, cx),
            }
            cx.notify();
        }
    }

    fn render_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let has_script = self.view.is_some();
        let items: Vec<gpui::AnyElement> = menus::file_dropdown()
            .into_iter()
            .enumerate()
            .map(|(ix, item)| match item {
                Item::Separator => div().h(px(1.)).my_1().bg(RULE).into_any_element(),
                Item::Action(e) => {
                    let enabled = has_script || !e.needs_script;
                    let action = e.action;
                    div()
                        .id(("menu-item", ix))
                        .flex()
                        .items_center()
                        .gap_4()
                        .px_3()
                        .py_1()
                        .rounded_sm()
                        .text_color(if enabled { INK } else { INK_3 })
                        .when(enabled, |d| {
                            d.cursor_pointer().hover(|s| s.bg(ACCENT_SOFT)).on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.close_menu(window, cx);
                                    window.dispatch_action(action(), cx);
                                },
                            ))
                        })
                        .child(div().flex_1().child(e.name))
                        .when_some(e.key, |d, k| d.child(div().text_color(INK_3).child(menus::shortcut(k))))
                        .into_any_element()
                }
            })
            .collect();
        // A full-window layer: clicking outside the menu closes it.
        deferred(
            div()
                .id("menu-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close_menu(window, cx)))
                .child(
                    div()
                        .id("file-menu")
                        .track_focus(&self.menu_focus)
                        .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                            if e.keystroke.key == "escape" {
                                this.close_menu(window, cx);
                            }
                        }))
                        // Clicks inside the menu don't reach the backdrop.
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .absolute()
                        .top(px(34.))
                        .left(px(8.))
                        .w(px(260.))
                        .p_1()
                        .flex()
                        .flex_col()
                        .bg(PAPER)
                        .border_1()
                        .border_color(RULE)
                        .rounded_md()
                        .shadow_md()
                        .children(items),
                ),
        )
        .with_priority(1)
    }

    // ---- rendering -----------------------------------------------------------

    fn render_toolbar(&self, cx: &mut Context<Self>) -> Div {
        let show_file_button = !cfg!(target_os = "macos");
        let (name, state) = match &self.view {
            Some(v) => {
                let v = v.read(cx);
                (file_name(v.script_path()), v.save_state(cx))
            }
            None => ("Scriptorium".to_string(), String::new()),
        };
        div()
            .flex_none()
            .h(px(38.))
            .px_2()
            .flex()
            .items_center()
            .gap_3()
            .bg(CHROME)
            .border_b_1()
            .border_color(RULE)
            .when(show_file_button, |d| {
                d.child(
                    toolbar_button("file-button", "File", self.menu.is_some())
                        // Pressing here would focus the workspace (it tracks
                        // focus) before the menu opens; keep the focus where
                        // the writer was, so the menu can give it back and its
                        // items act there (Edit › Undo in the composer).
                        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                        .on_click(cx.listener(|this, _, window, cx| this.toggle_menu(window, cx))),
                )
            })
            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(INK).child(name))
            .child(div().flex_1())
            .child(div().text_color(INK_2).child(state))
            .when(self.view.is_some(), |d| {
                d.child(
                    crate::script_view::button("apply", "Write .fdx", Some(&menus::shortcut("s")), true)
                        .on_click(cx.listener(|this, _, window, cx| this.on_apply(&ApplyToFdx, window, cx))),
                )
            })
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> Div {
        div().flex_1().min_h_0().flex().items_center().justify_center().bg(SIDE).child(
            div()
                .w(px(460.))
                .p_6()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .bg(PAPER)
                .border_1()
                .border_color(RULE)
                .rounded_lg()
                .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).child("Scriptorium"))
                .child(
                    div()
                        .text_color(INK_2)
                        .text_center()
                        .child("Work through proposed changes to a Final Draft script, one at a time."),
                )
                .child(
                    crate::script_view::button("open-script", "Open a script…", Some(&menus::shortcut("o")), true)
                        .on_click(cx.listener(|this, _, window, cx| this.on_open(&Open, window, cx))),
                )
                .child(div().text_size(px(12.)).text_color(INK_3).child("or drop a .fdx file on this window")),
        )
    }

    fn render_status(&self) -> Div {
        div()
            .flex_none()
            .h(px(28.))
            .px_3()
            .flex()
            .items_center()
            .bg(CHROME)
            .border_t_1()
            .border_color(RULE)
            .text_size(px(12.))
            .text_color(INK_2)
            .child(div().flex_1())
            .child(if self.notice.is_empty() {
                div().child(format!("{} open a script", menus::shortcut("o")))
            } else {
                div().text_color(INK).font_weight(FontWeight::SEMIBOLD).child(self.notice.clone())
            })
    }
}

fn toolbar_button(id: &'static str, text: &'static str, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded_md()
        .cursor_pointer()
        .text_color(INK)
        .hover(|s| s.bg(ACCENT_SOFT))
        .when(on, |d| d.bg(ACCENT_SOFT))
        .child(text)
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let menu = self.menu.is_some().then(|| self.render_menu(cx));
        let accounts = self.accounts.as_ref().map(|(sheet, _, _)| sheet.clone());
        let body = match &self.view {
            Some(v) => div().flex_1().min_h_0().flex().flex_col().child(v.clone()),
            None => div().flex_1().min_h_0().flex().flex_col().child(self.render_empty(cx)).child(self.render_status()),
        };
        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(CHROME)
            .text_color(INK)
            .text_size(px(13.))
            .on_action(cx.listener(Self::on_open))
            .on_action(cx.listener(Self::on_add_review))
            .on_action(cx.listener(Self::on_apply))
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(Self::on_about))
            .on_action(cx.listener(Self::on_toggle_agent))
            .on_action(cx.listener(Self::on_open_accounts))
            .on_action(
                cx.listener(|this, _: &UndoAny, window, cx| this.with_view(cx, |v, cx| v.undo_any(false, window, cx))),
            )
            .on_action(
                cx.listener(|this, _: &RedoAny, window, cx| this.with_view(cx, |v, cx| v.undo_any(true, window, cx))),
            )
            .on_action(
                cx.listener(|this, _: &ShowScript, window, cx| this.with_view(cx, |v, cx| v.show_nth(0, window, cx))),
            )
            .on_action(
                cx.listener(|this, _: &ShowReview1, window, cx| this.with_view(cx, |v, cx| v.show_nth(1, window, cx))),
            )
            .on_action(
                cx.listener(|this, _: &ShowReview2, window, cx| this.with_view(cx, |v, cx| v.show_nth(2, window, cx))),
            )
            .on_action(
                cx.listener(|this, _: &ShowReview3, window, cx| this.with_view(cx, |v, cx| v.show_nth(3, window, cx))),
            )
            .on_action(
                cx.listener(|this, _: &NextView, window, cx| {
                    this.with_view(cx, |v, cx| v.cycle_view(true, window, cx))
                }),
            )
            .on_action(
                cx.listener(|this, _: &PrevView, window, cx| {
                    this.with_view(cx, |v, cx| v.cycle_view(false, window, cx))
                }),
            )
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| this.dropped(paths, window, cx)))
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(ACCENT_SOFT))
            .child(self.render_toolbar(cx))
            .child(body)
            .children(accounts)
            .children(menu)
    }
}
