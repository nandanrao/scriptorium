//! The few things the app remembers between runs, per user: which one-time
//! notices the writer has already seen (`notices-seen`, one notice name per
//! line), and whether the agent panel was open (`agent-panel`: `open` or
//! `closed`). Plain files, so there's nothing to migrate.
//!
//! macOS: `~/Library/Application Support/Scriptorium/`.
//! Elsewhere: `$XDG_CONFIG_HOME/scriptorium/` (default `~/.config/scriptorium/`).

use std::path::PathBuf;

/// "Enter now starts a new line" (the line-rewrite key moved to Cmd/Ctrl+Enter).
pub const ENTER_SPLITS: &str = "enter-splits";

fn dir() -> Option<PathBuf> {
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
}
