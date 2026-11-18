//! The menus, defined once. On macOS they become the menu bar
//! (`cx.set_menus`); on Linux GPUI has no menu bar, so the workspace draws a
//! File button with a dropdown from the same definition.
//!
//! Menu items dispatch actions to the focused window; the workspace handles
//! them at its root, so they work whatever has focus (or nothing).
//!
//! The Apps menu (plan "apps", A9) is the one part that changes while the
//! app runs: the workspace keeps an [`AppsMenu`] (the open folder's apps,
//! scanned when a script opens, the window gains focus or the Linux
//! dropdown opens) and builds the menus from it; on macOS it sets the menu
//! bar again when the list changes.

use gpui::{Action, Menu, MenuItem, NoAction, SharedString};
use scriptorium_apps::list::Found;

use crate::agent_panel::{OpenAccounts, ToggleAgentPanel};
use crate::connect::{ConnectAgent, InstallCli};
use crate::new_version::{MarkInSync, MarkNotInSync, NewVersion};
use crate::script_view::{ApplyToFdx, NextView, PrevView, RedoAny, SaveAndQuit, ShowScript, UndoAny};
use crate::workspace::{About, AddReview, Open, OpenApp};

pub struct Entry {
    pub name: SharedString,
    /// What choosing it does; none for a line that only says something
    /// (always disabled).
    pub action: Option<Box<dyn Action>>,
    /// Shortcut key after Cmd/Ctrl (`"shift-o"`), for display.
    pub key: Option<&'static str>,
    /// Only makes sense with a script open.
    pub needs_script: bool,
}

pub enum Item {
    Action(Entry),
    /// A section's name in the Linux dropdown ("Apps").
    Heading(&'static str),
    Separator,
}

pub struct MenuDef {
    pub name: &'static str,
    pub items: Vec<Item>,
}

fn entry(name: &'static str, action: fn() -> Box<dyn Action>, key: Option<&'static str>, needs_script: bool) -> Item {
    Item::Action(Entry { name: name.into(), action: Some(action()), key, needs_script })
}

/// A disabled line that says something.
fn line(text: impl Into<SharedString>) -> Item {
    Item::Action(Entry { name: text.into(), action: None, key: None, needs_script: false })
}

/// What the Apps menu shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AppsMenu {
    /// No script open: apps belong to a script's folder.
    #[default]
    NoScript,
    /// Apps don't run this time, in the writer's words (port taken).
    Unavailable(String),
    /// The open folder's apps, in name order (none: says so).
    Listed(Vec<AppLine>),
}

/// One app in the Apps menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppLine {
    /// The folder name ([`OpenApp`]'s `name`).
    pub name: String,
    /// The manifest's title, or the folder name when it has none.
    pub title: String,
    /// Why it can't open (its first problem, shortened); none when it can.
    pub problem: Option<String>,
}

/// The longest problem shown in a menu item, in characters.
const PROBLEM_MAX: usize = 60;

impl AppsMenu {
    /// The menu for a folder's apps as [`scriptorium_apps::shell::scan`]
    /// found them.
    pub fn listed(found: &[Found]) -> Self {
        AppsMenu::Listed(
            found
                .iter()
                .map(|f| AppLine {
                    name: f.name.clone(),
                    title: f.manifest.as_ref().map_or_else(|| f.name.clone(), |m| m.title.clone()),
                    problem: f.problems.first().map(|p| shorten(&p.to_string())),
                })
                .collect(),
        )
    }

    fn items(&self) -> Vec<Item> {
        match self {
            AppsMenu::NoScript => vec![line("Open a script to see its apps")],
            AppsMenu::Unavailable(why) => vec![line(why.clone())],
            AppsMenu::Listed(apps) if apps.is_empty() => vec![line("No apps in this script's folder")],
            AppsMenu::Listed(apps) => apps
                .iter()
                .map(|a| match &a.problem {
                    None => Item::Action(Entry {
                        name: a.title.clone().into(),
                        action: Some(Box::new(OpenApp { name: a.name.clone() })),
                        key: None,
                        needs_script: true,
                    }),
                    Some(p) => line(format!("{} ({p})", a.title)),
                })
                .collect(),
        }
    }
}

/// `text` cut to [`PROBLEM_MAX`] characters, with "…" when cut.
fn shorten(text: &str) -> String {
    if text.chars().count() <= PROBLEM_MAX {
        return text.to_string();
    }
    let cut: String = text.chars().take(PROBLEM_MAX - 1).collect();
    format!("{}…", cut.trim_end())
}

pub fn definition(apps: &AppsMenu) -> Vec<MenuDef> {
    vec![
        MenuDef {
            name: "Scriptorium",
            items: vec![
                entry("About Scriptorium", || Box::new(About), None, false),
                Item::Separator,
                entry("AI accounts…", || Box::new(OpenAccounts), None, false),
                entry("Connect an AI agent…", || Box::new(ConnectAgent), None, false),
                entry("Install command-line tool…", || Box::new(InstallCli), None, false),
                Item::Separator,
                entry("Quit Scriptorium", || Box::new(SaveAndQuit), Some("q"), false),
            ],
        },
        MenuDef {
            name: "File",
            items: vec![
                entry("Open…", || Box::new(Open), Some("o"), false),
                entry("Add review…", || Box::new(AddReview), Some("shift-o"), true),
                entry("New version…", || Box::new(NewVersion), Some("t"), true),
                Item::Separator,
                entry("Write .fdx", || Box::new(ApplyToFdx), Some("s"), true),
            ],
        },
        MenuDef {
            name: "Edit",
            items: vec![
                entry("Undo", || Box::new(UndoAny), Some("z"), true),
                entry("Redo", || Box::new(RedoAny), Some("shift-z"), true),
                Item::Separator,
                entry("Mark in sync", || Box::new(MarkInSync), None, true),
                entry("Mark not in sync", || Box::new(MarkNotInSync), None, true),
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
        MenuDef { name: "Apps", items: apps.items() },
    ]
}

/// The File dropdown drawn on Linux: the File menu, Edit's Undo and Redo,
/// the agent panel, the Apps menu under an "Apps" heading, then the
/// application menu's items (About, AI accounts, Connect an AI agent,
/// Install command-line tool, Quit, which live in the application menu on
/// macOS).
pub fn file_dropdown(apps: &AppsMenu) -> Vec<Item> {
    let mut defs = definition(apps);
    let mut file = std::mem::take(&mut defs[1].items);
    let app = std::mem::take(&mut defs[0].items);
    let edit = std::mem::take(&mut defs[2].items);
    let panel = std::mem::take(&mut defs[3].items).into_iter().filter(|i| match i {
        Item::Action(e) => e.name == "Agent panel",
        _ => false,
    });
    let apps = std::mem::take(&mut defs[4].items);
    file.push(Item::Separator);
    file.extend(edit);
    file.extend(panel);
    file.push(Item::Separator);
    file.push(Item::Heading("Apps"));
    file.extend(apps);
    file.push(Item::Separator);
    file.extend(app.into_iter().filter(|i| matches!(i, Item::Action(_))));
    file
}

/// The macOS menu bar.
pub fn app_menus(apps: &AppsMenu) -> Vec<Menu> {
    definition(apps)
        .into_iter()
        .map(|m| Menu {
            name: m.name.into(),
            disabled: false,
            items: m
                .items
                .into_iter()
                .filter_map(|i| match i {
                    Item::Separator => Some(MenuItem::separator()),
                    // The menu's own name says it on macOS.
                    Item::Heading(_) => None,
                    Item::Action(e) => Some(MenuItem::Action {
                        name: e.name,
                        disabled: e.action.is_none(),
                        action: e.action.unwrap_or_else(|| Box::new(NoAction)),
                        os_action: None,
                        checked: false,
                    }),
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
    use scriptorium_apps::manifest::{Manifest, Problem};

    #[test]
    fn shortcuts_read_naturally() {
        if !cfg!(target_os = "macos") {
            assert_eq!(shortcut("o"), "Ctrl+O");
            assert_eq!(shortcut("shift-o"), "Ctrl+Shift+O");
        }
    }

    /// Each item as text: a name ("!" before it when it only says
    /// something), "-" for a separator, "# Apps" for a heading.
    fn names(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|i| match i {
                Item::Action(e) if e.action.is_none() => format!("!{}", e.name),
                Item::Action(e) => e.name.to_string(),
                Item::Heading(h) => format!("# {h}"),
                Item::Separator => "-".into(),
            })
            .collect()
    }

    fn apps_of(menu: &AppsMenu) -> Vec<String> {
        let defs = definition(menu);
        let apps = defs.iter().find(|m| m.name == "Apps").expect("an Apps menu");
        names(&apps.items)
    }

    fn found(name: &str, title: Option<&str>, problems: Vec<Problem>) -> Found {
        Found {
            name: name.into(),
            folder: format!("/s/apps/{name}").into(),
            manifest: title.map(|t| Manifest { title: t.into(), description: "d".into() }),
            problems,
        }
    }

    #[test]
    fn dropdown_has_file_edit_panel_and_apps_items_then_quit() {
        assert_eq!(
            names(&file_dropdown(&AppsMenu::NoScript)),
            [
                "Open…",
                "Add review…",
                "New version…",
                "-",
                "Write .fdx",
                "-",
                "Undo",
                "Redo",
                "-",
                "Mark in sync",
                "Mark not in sync",
                "Agent panel",
                "-",
                "# Apps",
                "!Open a script to see its apps",
                "-",
                "About Scriptorium",
                "AI accounts…",
                "Connect an AI agent…",
                "Install command-line tool…",
                "Quit Scriptorium"
            ]
        );
    }

    #[test]
    fn apps_menu_lists_good_apps_and_disables_ones_with_problems() {
        let menu = AppsMenu::listed(&[
            found("bad-json", None, vec![Problem::BadJson("expected value at line 1 column 1".into())]),
            found("scene-list", Some("Scene list"), vec![]),
            found("storyboard", Some("Storyboard"), vec![Problem::NoIndex, Problem::Missing("description")]),
        ]);
        assert_eq!(
            apps_of(&menu),
            [
                "!bad-json (app.json isn't valid JSON: expected value at line 1 column 1)",
                "Scene list",
                "!Storyboard (index.html is missing)"
            ]
        );
        let defs = definition(&menu);
        let Item::Action(e) = &defs[4].items[1] else { panic!("an item") };
        let open = e.action.as_ref().unwrap().as_any().downcast_ref::<OpenApp>().expect("OpenApp");
        assert_eq!(open.name, "scene-list");
        assert!(e.needs_script);
    }

    #[test]
    fn a_long_problem_is_shortened() {
        let long = Problem::BadJson("x".repeat(200));
        let menu = AppsMenu::listed(&[found("a", None, vec![long])]);
        let AppsMenu::Listed(apps) = &menu else { panic!() };
        let p = apps[0].problem.as_ref().unwrap();
        assert_eq!(p.chars().count(), PROBLEM_MAX);
        assert!(p.ends_with('…'));
    }

    #[test]
    fn apps_menu_with_none_says_so() {
        assert_eq!(apps_of(&AppsMenu::listed(&[])), ["!No apps in this script's folder"]);
    }

    #[test]
    fn apps_menu_with_no_server_says_why() {
        let why = "Apps can't open: another program is using port 28417. Quit it and restart Scriptorium.";
        assert_eq!(apps_of(&AppsMenu::Unavailable(why.into())), [format!("!{why}")]);
    }

    #[test]
    fn apps_menu_with_no_script_is_disabled() {
        assert_eq!(apps_of(&AppsMenu::NoScript), ["!Open a script to see its apps"]);
    }

    #[test]
    fn mac_menu_bar_has_apps_with_lines_disabled() {
        let menu = AppsMenu::listed(&[
            found("scene-list", Some("Scene list"), vec![]),
            found("x", None, vec![Problem::NoManifest]),
        ]);
        let bar = app_menus(&menu);
        let apps = bar.iter().find(|m| m.name == "Apps").expect("Apps");
        let states: Vec<(String, bool)> = apps
            .items
            .iter()
            .map(|i| match i {
                MenuItem::Action { name, disabled, .. } => (name.to_string(), *disabled),
                _ => panic!("only actions"),
            })
            .collect();
        assert_eq!(states, [("Scene list".to_string(), false), ("x (app.json is missing)".to_string(), true)]);
    }
}
