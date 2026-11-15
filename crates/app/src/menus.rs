//! The menus, defined once. On macOS they become the menu bar
//! (`cx.set_menus`); on Linux GPUI has no menu bar, so the workspace draws a
//! File button with a dropdown from the same definition.
//!
//! Menu items dispatch actions to the focused window; the workspace handles
//! them at its root, so they work whatever has focus (or nothing).

use gpui::{Action, Menu, MenuItem};

use crate::agent_panel::{OpenAccounts, ToggleAgentPanel};
use crate::script_view::{ApplyToFdx, NextView, PrevView, RedoAny, SaveAndQuit, ShowScript, UndoAny};
use crate::workspace::{About, AddReview, Open};

pub struct Entry {
    pub name: &'static str,
    pub action: fn() -> Box<dyn Action>,
    /// Shortcut key after Cmd/Ctrl (`"shift-o"`), for display.
    pub key: Option<&'static str>,
    /// Only makes sense with a script open.
    pub needs_script: bool,
}

pub enum Item {
    Action(Entry),
    Separator,
}

pub struct MenuDef {
    pub name: &'static str,
    pub items: Vec<Item>,
}

fn entry(name: &'static str, action: fn() -> Box<dyn Action>, key: Option<&'static str>, needs_script: bool) -> Item {
    Item::Action(Entry { name, action, key, needs_script })
}

pub fn definition() -> Vec<MenuDef> {
    vec![
        MenuDef {
            name: "Scriptorium",
            items: vec![
                entry("About Scriptorium", || Box::new(About), None, false),
                Item::Separator,
                entry("AI accounts…", || Box::new(OpenAccounts), None, false),
                Item::Separator,
                entry("Quit Scriptorium", || Box::new(SaveAndQuit), Some("q"), false),
            ],
        },
        MenuDef {
            name: "File",
            items: vec![
                entry("Open…", || Box::new(Open), Some("o"), false),
                entry("Add review…", || Box::new(AddReview), Some("shift-o"), true),
                Item::Separator,
                entry("Write .fdx", || Box::new(ApplyToFdx), Some("s"), true),
            ],
        },
        MenuDef {
            name: "Edit",
            items: vec![
                entry("Undo", || Box::new(UndoAny), Some("z"), true),
                entry("Redo", || Box::new(RedoAny), Some("shift-z"), true),
            ],
        },
        MenuDef {
            name: "View",
            items: vec![
                entry("Script", || Box::new(ShowScript), Some("0"), true),
                entry("Next view", || Box::new(NextView), None, true),
                entry("Previous view", || Box::new(PrevView), None, true),
                Item::Separator,
                entry("Agent panel", || Box::new(ToggleAgentPanel), Some("l"), true),
            ],
        },
    ]
}

/// The File dropdown drawn on Linux: the File menu, Edit's Undo and Redo,
/// the agent panel, then the application menu's items (About, AI accounts,
/// Quit, which live in the application menu on macOS).
pub fn file_dropdown() -> Vec<Item> {
    let mut defs = definition();
    let mut file = std::mem::take(&mut defs[1].items);
    let app = std::mem::take(&mut defs[0].items);
    let edit = std::mem::take(&mut defs[2].items);
    let panel = std::mem::take(&mut defs[3].items).into_iter().filter(|i| match i {
        Item::Action(e) => e.name == "Agent panel",
        Item::Separator => false,
    });
    file.push(Item::Separator);
    file.extend(edit);
    file.extend(panel);
    file.push(Item::Separator);
    file.extend(app.into_iter().filter(|i| matches!(i, Item::Action(_))));
    file
}

/// The macOS menu bar.
pub fn app_menus() -> Vec<Menu> {
    definition()
        .into_iter()
        .map(|m| Menu {
            name: m.name.into(),
            disabled: false,
            items: m
                .items
                .into_iter()
                .map(|i| match i {
                    Item::Separator => MenuItem::separator(),
                    Item::Action(e) => MenuItem::Action {
                        name: e.name.into(),
                        action: (e.action)(),
                        os_action: None,
                        checked: false,
                        disabled: false,
                    },
                })
                .collect(),
        })
        .collect()
}

/// A shortcut as the platform writes it: `"shift-o"` → "⌘⇧O" on macOS,
/// "Ctrl+Shift+O" elsewhere.
pub fn shortcut(key: &str) -> String {
    let (shift, key) = match key.strip_prefix("shift-") {
        Some(k) => (true, k),
        None => (false, key),
    };
    let key = key.to_uppercase();
    if cfg!(target_os = "macos") {
        format!("⌘{}{key}", if shift { "⇧" } else { "" })
    } else {
        format!("Ctrl+{}{key}", if shift { "Shift+" } else { "" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_read_naturally() {
        if !cfg!(target_os = "macos") {
            assert_eq!(shortcut("o"), "Ctrl+O");
            assert_eq!(shortcut("shift-o"), "Ctrl+Shift+O");
        }
    }

    #[test]
    fn dropdown_has_file_edit_and_panel_items_then_quit() {
        let names: Vec<&str> = file_dropdown()
            .iter()
            .map(|i| match i {
                Item::Action(e) => e.name,
                Item::Separator => "-",
            })
            .collect();
        assert_eq!(
            names,
            [
                "Open…",
                "Add review…",
                "-",
                "Write .fdx",
                "-",
                "Undo",
                "Redo",
                "Agent panel",
                "-",
                "About Scriptorium",
                "AI accounts…",
                "Quit Scriptorium"
            ]
        );
    }
}
