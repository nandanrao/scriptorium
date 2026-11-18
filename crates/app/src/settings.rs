//! The few things the app remembers between runs, per user: which one-time
//! notices the writer has already seen (`notices-seen`, one notice name per
//! line), and whether the agent panel was open (`agent-panel`: `open` or
//! `closed`), and which AI agents Connect set up (`agents-connected`, for
//! the launch check), and the apps token (`apps-token`: the per-install
//! secret apps' pages are opened with; plan "apps"). Plain files, so
//! there's nothing to migrate.
//!
//! macOS: `~/Library/Application Support/Scriptorium/`.
//! Elsewhere: `$XDG_CONFIG_HOME/scriptorium/` (default `~/.config/scriptorium/`).

use std::path::PathBuf;

/// "Enter now starts a new line" (the line-rewrite key moved to Cmd/Ctrl+Enter).
pub const ENTER_SPLITS: &str = "enter-splits";

fn dir() -> Option<PathBuf> {
    // Tests never read or write the user's settings.
    if cfg!(test) {
        return None;
    }
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
    let dir = if cfg!(target_os = "macos") {
        home?.join("Library/Application Support/Scriptorium")
    } else {
        match std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
            Some(d) => PathBuf::from(d).join("scriptorium"),
            None => home?.join(".config/scriptorium"),
        }
    };
    Some(dir)
}

fn path() -> Option<PathBuf> {
    Some(dir()?.join("notices-seen"))
}

/// Whether the agent panel was open when the writer last opened or closed it.
pub fn agent_panel_open() -> bool {
    dir().and_then(|d| std::fs::read_to_string(d.join("agent-panel")).ok()).is_some_and(|s| s.trim() == "open")
}

/// Remember whether the agent panel is open. Never fails (logged).
pub fn set_agent_panel_open(open: bool) {
    let Some(dir) = dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("agent-panel");
    if scriptorium_fsio::write_atomic(&path, if open { "open\n" } else { "closed\n" }.as_bytes()).is_err() {
        crate::log::warn(&format!("couldn't remember the agent panel in {}", path.display()));
    }
}

/// The agents Connect set up (`agents-connected`; the format is
/// `connect::Remembered`'s), or "" when there is none or it can't be read.
pub fn agents_connected() -> String {
    dir().and_then(|d| std::fs::read_to_string(d.join("agents-connected")).ok()).unwrap_or_default()
}

/// Remember the agents Connect set up. Never fails (logged).
pub fn set_agents_connected(text: &str) {
    let Some(dir) = dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("agents-connected");
    if scriptorium_fsio::write_atomic(&path, text.as_bytes()).is_err() {
        crate::log::warn(&format!("couldn't remember the connected agents in {}", path.display()));
    }
}

/// The apps token's file, readable by its owner only.
pub const APPS_TOKEN: &str = "apps-token";

/// The per-install token apps' pages are opened with (64 hex digits): read
/// from `apps-token`, or made on first use (32 random bytes) and saved
/// there, readable by its owner only. If it can't be saved, it serves this
/// run only (logged): pages need the Apps menu again after a restart.
/// `None` only when no random bytes could be had.
pub fn apps_token() -> Option<String> {
    token_in(dir().as_deref())
}

fn token_in(dir: Option<&std::path::Path>) -> Option<String> {
    let path = dir.map(|d| d.join(APPS_TOKEN));
    if let Some(saved) = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok())
        && token_ok(saved.trim())
    {
        return Some(saved.trim().to_string());
    }
    let mut bytes = [0u8; 32];
    if let Err(e) = getrandom::fill(&mut bytes) {
        crate::log::warn(&format!("apps: no random token: {e}"));
        return None;
    }
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let saved = path.as_ref().is_some_and(|p| {
        p.parent().is_some_and(|d| std::fs::create_dir_all(d).is_ok())
            && scriptorium_fsio::write_atomic_private(p, format!("{token}\n").as_bytes()).is_ok()
            && private(p)
    });
    if !saved {
        let at = path.map_or("(no settings folder)".into(), |p| p.display().to_string());
        crate::log::warn(&format!("apps: the token couldn't be saved in {at}; it serves this run only"));
    }
    Some(token)
}

/// Make `path` readable by its owner only (an existing file keeps its
/// mode through an atomic write).
fn private(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        true
    }
}

/// Whether `s` is a token [`apps_token`] makes: 64 lower-case hex digits.
fn token_ok(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn contains(file: &str, name: &str) -> bool {
    file.lines().any(|l| l.trim() == name)
}

/// Whether the writer has seen notice `name`; if not, remember that they
/// have now. Never fails: if the file can't be read or written, the notice
/// may show again, which is harmless.
pub fn first_time(name: &str) -> bool {
    let Some(path) = path() else { return true };
    let file = std::fs::read_to_string(&path).unwrap_or_default();
    if contains(&file, name) {
        return false;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut out = file;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(name);
    out.push('\n');
    if scriptorium_fsio::write_atomic(&path, out.as_bytes()).is_err() {
        crate::log::warn(&format!("couldn't remember a seen notice in {}", path.display()));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_whole_lines() {
        assert!(contains("a\nenter-splits\n", ENTER_SPLITS));
        assert!(!contains("enter-splits-old\n", ENTER_SPLITS));
        assert!(!contains("", ENTER_SPLITS));
    }

    #[test]
    fn the_apps_token_is_made_once_and_kept_private() {
        let dir = std::env::temp_dir().join(format!("scriptorium-token-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = token_in(Some(&dir)).unwrap();
        assert!(token_ok(&first), "{first}");
        assert_eq!(token_in(Some(&dir)).as_deref(), Some(first.as_str()), "kept across runs");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(APPS_TOKEN)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // A broken file (readable by all) is replaced.
        std::fs::write(dir.join(APPS_TOKEN), "short\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.join(APPS_TOKEN), std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let second = token_in(Some(&dir)).unwrap();
        assert!(token_ok(&second) && second != first);
        assert_eq!(std::fs::read_to_string(dir.join(APPS_TOKEN)).unwrap().trim(), second);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(APPS_TOKEN)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "a replaced file is made private too");
        }
        // No settings folder: a token for this run, a new one each time.
        let (a, b) = (token_in(None).unwrap(), token_in(None).unwrap());
        assert!(token_ok(&a) && a != b);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
