//! Opening apps in the writer's own browser (plan "apps", A8).
//!
//! - [`open`] hands a URL to the system's opener (`open` on macOS,
//!   `xdg-open` elsewhere), detached and never through a shell. With
//!   [`TEST_OPEN_URL_ENV`] set it appends the URL to that file instead, so
//!   tests and scenarios never open a browser on anyone's screen. The
//!   crate's own tests never launch an opener at all.
//! - [`open_app`] opens one of the open folder's apps: the Apps menu and the
//!   `open_app` tool both go through it, so they check and say the same.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use scriptorium_apps::list::Found;

use crate::apps_host::Apps;

/// When set (and not empty), [`open`] appends each URL, one per line, to
/// this file instead of opening a browser.
pub(crate) const TEST_OPEN_URL_ENV: &str = "SCRIPTORIUM_TEST_OPEN_URL";

/// Open `url` in the writer's default browser (or note it in the test
/// hook's file). Doesn't wait for the browser.
pub(crate) fn open(url: &str) -> io::Result<()> {
    match hook() {
        Some(file) => append_url(&file, url),
        None => launch(url),
    }
}

/// The test hook's file, if one is set.
fn hook() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(file) = test_hook::get() {
        return Some(file);
    }
    std::env::var_os(TEST_OPEN_URL_ENV).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Append `url` and a newline to `file` (made if missing).
fn append_url(file: &Path, url: &str) -> io::Result<()> {
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(file)?;
    f.write_all(format!("{url}\n").as_bytes())
}

/// The system's opener for URLs.
#[cfg(not(test))]
const OPENER: &str = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };

/// Run the opener on `url`, detached: no shell, no inherited input or
/// output, reaped on a thread of its own.
#[cfg(not(test))]
fn launch(url: &str) -> io::Result<()> {
    use std::process::{Command, Stdio};
    // Only ever a web address we built: never something an opener could
    // read as an option.
    if !url.starts_with("http://") {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a web address"));
    }
    let mut child =
        Command::new(OPENER).arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Tests never open a browser: without the hook, opening fails.
#[cfg(test)]
fn launch(_: &str) -> io::Result<()> {
    Err(io::Error::other("tests never open a browser (no hook set)"))
}

/// Why an app didn't open. `text` is plain sentences for the writer (the
/// Apps menu shows it alone); `check_apps`: the app's name or folder is
/// the trouble, so an agent is told to check it with `list_apps`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refused {
    pub text: String,
    pub check_apps: bool,
}

impl Refused {
    fn because(text: impl Into<String>) -> Self {
        Refused { text: text.into(), check_apps: false }
    }

    fn check(text: impl Into<String>) -> Self {
        Refused { text: text.into(), check_apps: true }
    }
}

/// Apps aren't served from this window (another serves them, or none yet).
pub(crate) const APPS_OFF: &str = "Apps open from the other Scriptorium window.";

/// The opening link for app `name` of the folder with project key `key`:
/// `/open` exchanges the token for the cookie and moves on to the app.
pub(crate) fn app_url(port: u16, token: &str, key: &str, name: &str) -> String {
    format!("http://127.0.0.1:{port}/open?t={token}&to=/p/{key}/{name}/")
}

/// Open app `name` of the script folder `dir` in the writer's browser,
/// with apps as served this run. Ok: the app's title. Err: why not, in
/// words for the writer and the agent (never the token).
pub(crate) fn open_app(apps: &Apps, dir: &Path, name: &str) -> Result<String, Refused> {
    if !scriptorium_apps::manifest::name_ok(name) {
        return Err(Refused::check(format!(
            "{name:?} isn't an app name (lower-case letters, digits and -, at most 40 characters)."
        )));
    }
    let (port, token) = match apps {
        Apps::Serving { running, token } => (running.port(), token.as_str()),
        Apps::Unavailable(why) => return Err(Refused::because(why.clone())),
        Apps::Off => return Err(Refused::because(APPS_OFF)),
    };
    let found = scriptorium_apps::shell::scan(dir);
    let title = app_title(&found, &dir.join("apps"), name)?;
    let url = app_url(port, token, &crate::agent_link::project_key(dir), name);
    match open(&url) {
        Ok(()) => Ok(title),
        Err(e) => {
            crate::log::warn(&format!("couldn't open the browser for app {name}: {e}"));
            Err(Refused::because(format!("Scriptorium couldn't open the browser ({e}).")))
        }
    }
}

/// App `name`'s title among `found` (in `apps_dir`), or why it can't open.
fn app_title(found: &[Found], apps_dir: &Path, name: &str) -> Result<String, Refused> {
    let Some(app) = found.iter().find(|a| a.name == name) else {
        return Err(Refused::check(format!("There's no app {name} in {}/.", apps_dir.display())));
    };
    match &app.manifest {
        Some(m) if app.problems.is_empty() => Ok(m.title.clone()),
        _ => {
            let problems: Vec<String> = app.problems.iter().map(|p| p.to_string()).collect();
            Err(Refused::check(format!("The app {name} can't open: {}.", problems.join("; "))))
        }
    }
}

/// A hook file for this test thread only (the env var is the whole
/// process's): the link's tests run the UI task's step on the test thread.
#[cfg(test)]
pub(crate) mod test_hook {
    use std::cell::RefCell;
    use std::path::PathBuf;

    thread_local! {
        static FILE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    pub(crate) fn set(file: Option<PathBuf>) {
        FILE.with(|f| *f.borrow_mut() = file);
    }

    pub(crate) fn get() -> Option<PathBuf> {
        FILE.with(|f| f.borrow().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("scriptorium-browser-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_hook_appends_one_url_per_line() {
        let dir = folder("hook");
        let file = dir.join("opened");
        test_hook::set(Some(file.clone()));
        open("http://127.0.0.1:1/a").unwrap();
        open("http://127.0.0.1:1/b").unwrap();
        test_hook::set(None);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "http://127.0.0.1:1/a\nhttp://127.0.0.1:1/b\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_hook_tests_never_launch_a_browser() {
        test_hook::set(None);
        if std::env::var_os(TEST_OPEN_URL_ENV).is_none() {
            assert!(open("http://127.0.0.1:1/").is_err());
        }
    }

    #[test]
    fn the_opening_link_exchanges_the_token_then_goes_to_the_app() {
        assert_eq!(
            app_url(28417, "abc", "0123456789abcdef", "scene-list"),
            "http://127.0.0.1:28417/open?t=abc&to=/p/0123456789abcdef/scene-list/"
        );
    }

    #[test]
    fn opening_needs_a_good_name_a_server_and_a_good_app() {
        let dir = folder("open");
        let app = dir.join("apps/scene-list");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("app.json"), r#"{"scriptorium": 1, "title": "Scene list", "description": "d"}"#)
            .unwrap();
        let broken = dir.join("apps/broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("app.json"), "{").unwrap();

        let bad = open_app(&Apps::Off, &dir, "../x").unwrap_err();
        assert!(bad.check_apps && bad.text.ends_with("at most 40 characters)."), "{bad:?}");
        let why = "Apps can't open: another program is using port 28417.";
        let taken = Apps::Unavailable(why.into());
        assert_eq!(open_app(&taken, &dir, "scene-list").unwrap_err(), Refused::because(why));
        assert_eq!(open_app(&Apps::Off, &dir, "scene-list").unwrap_err(), Refused::because(APPS_OFF));

        let found = scriptorium_apps::shell::scan(&dir);
        let apps_dir = dir.join("apps");
        let none = app_title(&found, &apps_dir, "nope").unwrap_err();
        assert_eq!(none, Refused::check(format!("There's no app nope in {}/.", apps_dir.display())));
        // No `index.html` yet: a problem. Plain sentences: the menu shows them.
        let no_index = app_title(&found, &apps_dir, "scene-list").unwrap_err();
        assert!(no_index.check_apps && no_index.text.starts_with("The app scene-list can't open: "), "{no_index:?}");
        assert!(no_index.text.ends_with('.') && !no_index.text.contains("list_apps"), "{no_index:?}");
        let broken = app_title(&found, &apps_dir, "broken").unwrap_err();
        assert!(broken.check_apps && broken.text.starts_with("The app broken can't open: "), "{broken:?}");

        std::fs::write(app.join("index.html"), "<p>hi</p>").unwrap();
        let found = scriptorium_apps::shell::scan(&dir);
        assert_eq!(app_title(&found, &apps_dir, "scene-list").unwrap(), "Scene list");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
