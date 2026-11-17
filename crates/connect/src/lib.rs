//! Connect an AI agent: Scriptorium's MCP server entry in each agent's
//! config file, as text in and text out (pure: no I/O). Also the
//! command-line tool's link onto PATH ([`cli_install`]), whose disk work is
//! behind the default feature `shell`; without it the crate is pure.
//!
//! Each agent has a module with the same three functions:
//! - `state(text, this)`: is our `scriptorium` entry there, and is it ours?
//! - `merge(text, &Entry)`: add our entry, or update ours in place.
//! - `remove(text, this)`: take ours out.
//!
//! `this` is the path of the running connector (the `command` we would
//! write now).
//!
//! The rules, for every format: never drop or reorder other keys or
//! servers, refuse a file that doesn't parse, treat an empty (or missing:
//! pass `""`) file as a fresh one, and leave another program's
//! `scriptorium` entry alone.
//!
//! "Ours" means the entry is named `scriptorium` and its command is `this`
//! or ends in `/Contents/MacOS/scriptorium` (inside a Scriptorium.app; maybe
//! another copy, or one since moved).
//!
//! The JSON configs are re-serialized whole (key order, and CRLF line
//! endings, kept): AGENTS.md rule 2 covers the writer's scripts, not agent
//! configs. E2 keeps a backup before every write.

use std::fmt;
use std::path::{Path, PathBuf};

pub mod agents;
pub mod claude_code;
pub mod claude_desktop;
pub mod cli_install;
pub mod codex;
mod json;
pub mod mcpb;
/// Writing the configs (step E2).
#[cfg(all(feature = "shell", unix))]
pub mod write;

/// The MCP server name in every config.
pub const NAME: &str = "scriptorium";

/// What a command must end with to count as ours.
pub const OURS_SUFFIX: &str = "/Contents/MacOS/scriptorium";

/// The entry we write: the connector's absolute path and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub command: PathBuf,
    pub args: Vec<String>,
}

impl Entry {
    /// The connector at `command`, started as `<command> mcp`.
    pub fn new(command: impl Into<PathBuf>) -> Self {
        Entry { command: command.into(), args: vec!["mcp".to_owned()] }
    }
}

/// What a config says about the `scriptorium` server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// No `scriptorium` entry (or an empty file).
    Missing,
    /// Our entry, pointing at this command (which may no longer exist).
    Ours { command: PathBuf },
    /// A `scriptorium` entry another program wrote.
    Theirs,
    /// The file doesn't parse, or isn't shaped as the agent expects.
    Broken(String),
}

/// Why `merge` or `remove` left the file alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// The file doesn't parse, or isn't shaped as the agent expects.
    Broken(String),
    /// Another program's `scriptorium` entry is there.
    Theirs,
    /// The entry's command isn't valid UTF-8, so no config can hold it.
    NotUtf8,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refused::Broken(why) => write!(f, "can't read its settings: {why}"),
            Refused::Theirs => write!(f, "another program's \"{NAME}\" server is set up there; left it alone"),
            Refused::NotUtf8 => write!(f, "Scriptorium's path isn't valid UTF-8"),
        }
    }
}

impl std::error::Error for Refused {}

/// Whether a server's command is ours: the running connector `this`, or a
/// Scriptorium.app's.
pub fn is_ours(command: &str, this: &Path) -> bool {
    Path::new(command) == this || command.ends_with(OURS_SUFFIX)
}

/// Classify an existing entry's command (`None`: no usable command).
fn classify(command: Option<&str>, this: &Path) -> State {
    match command {
        Some(c) if is_ours(c, this) => State::Ours { command: PathBuf::from(c) },
        _ => State::Theirs,
    }
}

fn command_str(entry: &Entry) -> Result<&str, Refused> {
    entry.command.to_str().ok_or(Refused::NotUtf8)
}

/// The setup command for other agents, as text for a shell:
/// `<path> mcp`, each word quoted when it needs to be.
pub fn setup_command(entry: &Entry) -> String {
    let mut words = vec![shell_quote(&entry.command.to_string_lossy())];
    words.extend(entry.args.iter().map(|a| shell_quote(a)));
    words.join(" ")
}

/// Quote one word for a POSIX shell: bare if every character is safe,
/// otherwise in single quotes (a `'` inside becomes `'\''`).
pub fn shell_quote(word: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c);
    if !word.is_empty() && word.chars().all(safe) {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_command_plain_path_is_bare() {
        let e = Entry::new("/Applications/Scriptorium.app/Contents/MacOS/scriptorium");
        assert_eq!(setup_command(&e), "/Applications/Scriptorium.app/Contents/MacOS/scriptorium mcp");
    }

    #[test]
    fn setup_command_quotes_spaces_unicode_and_quotes() {
        let e = Entry::new("/Users/x/My Apps/Scénario's/Scriptorium.app/Contents/MacOS/scriptorium");
        assert_eq!(
            setup_command(&e),
            r"'/Users/x/My Apps/Scénario'\''s/Scriptorium.app/Contents/MacOS/scriptorium' mcp"
        );
    }

    #[test]
    fn shell_quote_empty_word() {
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn ours_needs_the_bundle_suffix() {
        let this = Path::new("/home/me/scriptorium/target/debug/scriptorium");
        assert!(is_ours("/Applications/Scriptorium.app/Contents/MacOS/scriptorium", this));
        assert!(is_ours("/home/me/scriptorium/target/debug/scriptorium", this));
        assert!(!is_ours("/usr/local/bin/scriptorium", this));
        assert!(!is_ours("npx", this));
    }

    #[test]
    fn refused_messages() {
        assert!(Refused::Theirs.to_string().contains("left it alone"));
        assert!(Refused::Broken("x".into()).to_string().contains("x"));
    }
}
