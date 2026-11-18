//! The apps list, for `list_apps`, `scriptorium apps` and the Apps menu.
//!
//! Per app: its title, its folder, its problems, and, when the server has
//! seen its page, whether the page loaded and its last error. That last
//! part lets an agent with no shell (Claude Desktop) see that the app it
//! built failed, and why.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::manifest::{Manifest, Problem};

/// An app folder found under `apps/` (by the shell's scan).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The folder name.
    pub name: String,
    /// `<script folder>/apps/<name>/`.
    pub folder: PathBuf,
    /// The manifest, when it parsed.
    pub manifest: Option<Manifest>,
    /// Every reason it can't open; empty when it can.
    pub problems: Vec<Problem>,
}

impl Found {
    /// Whether the app can be opened.
    pub fn ok(&self) -> bool {
        self.manifest.is_some() && self.problems.is_empty()
    }
}

/// What the server saw of an app's pages this run (in memory only).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeenApp {
    /// A page of it loaded the library and said so.
    pub loaded: bool,
    /// The last error a page of it reported.
    pub error: Option<String>,
}

/// Per app name, what the server saw. `None` where there is no server
/// (the CLI, a host working on the files): the list then says nothing
/// about pages.
pub type Seen = BTreeMap<String, SeenApp>;

/// The list as text. `apps_dir` is `<script folder>/apps/`, named when
/// there are none.
pub fn text(apps_dir: &Path, found: &[Found], seen: Option<&Seen>) -> String {
    let dir = folder_text(apps_dir);
    if found.is_empty() {
        return format!("No apps in {dir}.");
    }
    let count = if found.len() == 1 { "1 app".to_string() } else { format!("{} apps", found.len()) };
    let mut out = format!("{count} in {dir}:\n");
    for app in found {
        match &app.manifest {
            Some(m) => out.push_str(&format!("- {}: {} ({})\n", app.name, m.title, m.description)),
            None => out.push_str(&format!("- {}\n", app.name)),
        }
        out.push_str(&format!("  folder {}\n", folder_text(&app.folder)));
        for problem in &app.problems {
            out.push_str(&format!("  problem: {problem}\n"));
        }
        if let Some(seen) = seen {
            let app_seen = seen.get(&app.name).cloned().unwrap_or_default();
            out.push_str(if app_seen.loaded { "  page: loaded\n" } else { "  page: not loaded this run\n" });
            if let Some(error) = &app_seen.error {
                out.push_str(&format!("  last error: {error}\n"));
            }
        }
    }
    out.pop();
    out
}

/// The list as JSON: `[{"name", "title", "description", "folder", "ok",
/// "problems": [..], "loaded", "error"}]`; `loaded` and `error` are null
/// without `seen`.
pub fn json(found: &[Found], seen: Option<&Seen>) -> Value {
    Value::Array(
        found
            .iter()
            .map(|app| {
                let app_seen = seen.map(|s| s.get(&app.name).cloned().unwrap_or_default());
                json!({
                    "name": app.name,
                    "title": app.manifest.as_ref().map(|m| &m.title),
                    "description": app.manifest.as_ref().map(|m| &m.description),
                    "folder": folder_text(&app.folder),
                    "ok": app.ok(),
                    "problems": app.problems.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
                    "loaded": app_seen.as_ref().map(|s| s.loaded),
                    "error": app_seen.and_then(|s| s.error),
                })
            })
            .collect(),
    )
}

/// A folder as shown: always ending in the separator, so it reads as a
/// folder and an agent can append a file name.
fn folder_text(dir: &Path) -> String {
    let s = dir.display().to_string();
    if s.ends_with(std::path::MAIN_SEPARATOR) || s.ends_with('/') {
        s
    } else {
        format!("{s}{}", std::path::MAIN_SEPARATOR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> Found {
        Found {
            name: "scene-list".into(),
            folder: PathBuf::from("/w/film/apps/scene-list"),
            manifest: Some(Manifest { title: "Scene list".into(), description: "Every scene.".into() }),
            problems: vec![],
        }
    }

    fn broken() -> Found {
        Found {
            name: "broken".into(),
            folder: PathBuf::from("/w/film/apps/broken/"),
            manifest: None,
            problems: vec![Problem::Missing("title"), Problem::NoIndex],
        }
    }

    #[test]
    fn none() {
        assert_eq!(text(Path::new("/w/film/apps"), &[], None), "No apps in /w/film/apps/.");
    }

    #[test]
    fn on_the_files() {
        let t = text(Path::new("/w/film/apps"), &[good(), broken()], None);
        assert_eq!(
            t,
            "2 apps in /w/film/apps/:\n\
             - scene-list: Scene list (Every scene.)\n  folder /w/film/apps/scene-list/\n\
             - broken\n  folder /w/film/apps/broken/\n  problem: app.json needs \"title\"\n  problem: index.html is missing"
        );
    }

    #[test]
    fn with_what_the_server_saw() {
        let seen: Seen = [("broken".to_string(), SeenApp { loaded: false, error: Some("x is not defined".into()) })]
            .into_iter()
            .collect();
        let t = text(Path::new("/w/film/apps/"), &[good(), broken()], Some(&seen));
        assert!(t.contains("scene-list/\n  page: not loaded this run\n"), "{t}");
        assert!(t.ends_with("  page: not loaded this run\n  last error: x is not defined"), "{t}");
        let j = json(&[good(), broken()], Some(&seen));
        assert_eq!(j[0]["ok"], true);
        assert_eq!(j[0]["loaded"], false);
        assert_eq!(j[1]["ok"], false);
        assert_eq!(j[1]["title"], Value::Null);
        assert_eq!(j[1]["problems"][1], "index.html is missing");
        assert_eq!(j[1]["error"], "x is not defined");
        assert_eq!(json(&[good()], None)[0]["loaded"], Value::Null);
    }
}
