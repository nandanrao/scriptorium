//! Scriptorium desktop app: review proposed changes to a script, and edit its text.
//!
//! Usage: `scriptorium-app [SCRIPT.fdx [REVIEW.txt ...]]`. See docs/PLAN.md.

mod editor;
mod elements;
mod layout;
mod log;
mod menus;
mod script_view;
mod theme;
mod workspace;

use std::borrow::Cow;
use std::path::PathBuf;

use gpui::{App, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size};

use editor::*;
use script_view::*;
use workspace::*;

const USAGE: &str = "usage: scriptorium-app [SCRIPT.fdx [REVIEW.txt ...]]

Opens the script with the review files attached to it last time, plus any
given here. With no arguments, opens an empty window (File › Open).";

/// Cmd on macOS, Ctrl elsewhere.
macro_rules! sec {
    ($k:literal) => {
        if cfg!(target_os = "macos") { concat!("cmd-", $k) } else { concat!("ctrl-", $k) }
    };
}

fn bindings() -> Vec<KeyBinding> {
    let ctx = Some("ScriptEditor");
    let mut b = vec![
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("shift-backspace", Backspace, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("up", Up, ctx),
        KeyBinding::new("down", Down, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("shift-up", SelectUp, ctx),
        KeyBinding::new("shift-down", SelectDown, ctx),
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("end", End, ctx),
        KeyBinding::new("shift-home", SelectHome, ctx),
        KeyBinding::new("shift-end", SelectEnd, ctx),
        KeyBinding::new("pageup", PageUp, ctx),
        KeyBinding::new("pagedown", PageDown, ctx),
        KeyBinding::new("enter", Enter, ctx),
        KeyBinding::new("tab", Tab, ctx),
        KeyBinding::new("shift-tab", ShiftTab, ctx),
        KeyBinding::new(sec!("a"), SelectAll, ctx),
        KeyBinding::new(sec!("c"), Copy, ctx),
        KeyBinding::new(sec!("x"), Cut, ctx),
        KeyBinding::new(sec!("v"), Paste, ctx),
        KeyBinding::new(sec!("z"), Undo, ctx),
        KeyBinding::new(sec!("shift-z"), Redo, ctx),
        KeyBinding::new("escape", Escape, ctx),
        KeyBinding::new(sec!("o"), Open, Some("Workspace")),
        KeyBinding::new(sec!("shift-o"), AddReview, Some("Workspace")),
        KeyBinding::new(sec!("s"), ApplyToFdx, Some("Workspace")),
        KeyBinding::new(sec!("q"), SaveAndQuit, Some("Workspace")),
        KeyBinding::new(sec!("0"), ShowScript, Some("Workspace")),
        KeyBinding::new(sec!("1"), ShowReview1, Some("Workspace")),
        KeyBinding::new(sec!("2"), ShowReview2, Some("Workspace")),
        KeyBinding::new(sec!("3"), ShowReview3, Some("Workspace")),
        KeyBinding::new("ctrl-tab", NextView, Some("Workspace")),
        KeyBinding::new("ctrl-shift-tab", PrevView, Some("Workspace")),
    ];
    if cfg!(target_os = "macos") {
        b.extend([
            KeyBinding::new("cmd-shift-]", NextView, Some("Workspace")),
            KeyBinding::new("cmd-shift-[", PrevView, Some("Workspace")),
        ]);
    }
    // Single keys on the review card, as in script-review.
    let review = Some("Review");
    b.extend([
        KeyBinding::new("a", Accept, review),
        KeyBinding::new("enter", Accept, review),
        KeyBinding::new("1", Accept1, review),
        KeyBinding::new("2", Accept2, review),
        KeyBinding::new("3", Accept3, review),
        KeyBinding::new("r", KeepMine, review),
        KeyBinding::new("e", Rewrite, review),
        KeyBinding::new("j", NextChange, review),
        KeyBinding::new("down", NextChange, review),
        KeyBinding::new("k", PrevChange, review),
        KeyBinding::new("up", PrevChange, review),
        KeyBinding::new("tab", NextOption, review),
        KeyBinding::new("right", NextOption, review),
        KeyBinding::new("shift-tab", PrevOption, review),
        KeyBinding::new("left", PrevOption, review),
        KeyBinding::new("u", UndoDecision, review),
        KeyBinding::new("shift-u", RedoDecision, review),
        KeyBinding::new(sec!("z"), UndoDecision, review),
        KeyBinding::new(sec!("shift-z"), RedoDecision, review),
    ]);
    if cfg!(target_os = "macos") {
        b.extend([
            KeyBinding::new("alt-left", WordLeft, ctx),
            KeyBinding::new("alt-right", WordRight, ctx),
            KeyBinding::new("alt-shift-left", SelectWordLeft, ctx),
            KeyBinding::new("alt-shift-right", SelectWordRight, ctx),
            KeyBinding::new("alt-backspace", DeleteWordLeft, ctx),
            KeyBinding::new("alt-delete", DeleteWordRight, ctx),
            KeyBinding::new("cmd-left", Home, ctx),
            KeyBinding::new("cmd-right", End, ctx),
            KeyBinding::new("cmd-up", DocStart, ctx),
            KeyBinding::new("cmd-down", DocEnd, ctx),
        ]);
    } else {
        b.extend([
            KeyBinding::new("ctrl-left", WordLeft, ctx),
            KeyBinding::new("ctrl-right", WordRight, ctx),
            KeyBinding::new("ctrl-shift-left", SelectWordLeft, ctx),
            KeyBinding::new("ctrl-shift-right", SelectWordRight, ctx),
            KeyBinding::new("ctrl-backspace", DeleteWordLeft, ctx),
            KeyBinding::new("ctrl-delete", DeleteWordRight, ctx),
            KeyBinding::new("ctrl-home", DocStart, ctx),
            KeyBinding::new("ctrl-end", DocEnd, ctx),
            KeyBinding::new("ctrl-y", Redo, ctx),
        ]);
    }
    b
}

fn main() {
    log::install_panic_hook();
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    let script = args.first().cloned();
    let reviews = args.get(1..).unwrap_or_default().to_vec();
    gpui_platform::application().run(move |cx: &mut App| {
        let fonts: Vec<Cow<'static, [u8]>> = vec![
            Cow::Borrowed(include_bytes!("../assets/fonts/CourierPrime-Regular.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/CourierPrime-Bold.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/CourierPrime-Italic.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/CourierPrime-BoldItalic.ttf")),
        ];
        if let Err(e) = cx.text_system().add_fonts(fonts) {
            log::warn(&format!("could not load Courier Prime: {e}"));
        }
        cx.bind_keys(bindings());
        // With no window to handle it (all closed), Quit still quits.
        cx.on_action(|_: &SaveAndQuit, cx| cx.quit());
        cx.set_menus(menus::app_menus());
        let bounds = Bounds::centered(None, size(px(1500.), px(1000.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions { title: Some("Scriptorium".into()), ..Default::default() }),
                    ..Default::default()
                },
                move |window, cx| cx.new(|cx| Workspace::new(script, reviews, window, cx)),
            )
            .expect("open window");
        window.update(cx, |_, _, cx| cx.activate(true)).ok();
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
    });
}
