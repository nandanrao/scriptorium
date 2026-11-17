//! The agents Connect knows, by name: where each keeps its config, and the
//! per-format `state`/`merge`/`remove` behind one switch (pure: no I/O).

use std::fmt;
use std::path::{Path, PathBuf};

use crate::{Entry, NAME, OURS_SUFFIX, Refused, State, claude_code, claude_desktop, codex, shell_quote};

/// Claude Desktop's macOS bundle id (*to verify* on a Mac): the running
/// check, quitting it, reopening it and opening a `.mcpb` with it.
pub const CLAUDE_DESKTOP_BUNDLE_ID: &str = "com.anthropic.claudefordesktop";

/// An agent Connect writes a config for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Agent {
    ClaudeDesktop,
    ClaudeCode,
    Codex,
}

impl Agent {
    pub const ALL: [Agent; 3] = [Agent::ClaudeDesktop, Agent::ClaudeCode, Agent::Codex];

    /// The name on the command line: `claude-desktop`, `claude-code`, `codex`.
    pub fn id(self) -> &'static str {
        match self {
            Agent::ClaudeDesktop => "claude-desktop",
            Agent::ClaudeCode => "claude-code",
            Agent::Codex => "codex",
        }
    }

    /// The name people see.
    pub fn label(self) -> &'static str {
        match self {
            Agent::ClaudeDesktop => "Claude Desktop",
            Agent::ClaudeCode => "Claude Code",
            Agent::Codex => "Codex",
        }
    }

    pub fn from_id(id: &str) -> Option<Agent> {
        Agent::ALL.into_iter().find(|a| a.id() == id)
    }

    /// The agent's config file under the home folder `home`. Claude
    /// Desktop's is in Application Support on macOS (elsewhere, where the
    /// Linux builds keep it: `~/.config/Claude`).
    pub fn config_path(self, home: &Path) -> PathBuf {
        match self {
            Agent::ClaudeDesktop if cfg!(target_os = "macos") => {
                home.join("Library/Application Support/Claude/claude_desktop_config.json")
            }
            Agent::ClaudeDesktop => home.join(".config/Claude/claude_desktop_config.json"),
            Agent::ClaudeCode => home.join(".claude.json"),
            Agent::Codex => home.join(".codex/config.toml"),
        }
    }

    pub fn state(self, text: &str, this: &Path) -> State {
        match self {
            Agent::ClaudeDesktop => claude_desktop::state(text, this),
            Agent::ClaudeCode => claude_code::state(text, this),
            Agent::Codex => codex::state(text, this),
        }
    }

    pub fn merge(self, text: &str, entry: &Entry) -> Result<String, Refused> {
        match self {
            Agent::ClaudeDesktop => claude_desktop::merge(text, entry),
            Agent::ClaudeCode => claude_code::merge(text, entry),
            Agent::Codex => codex::merge(text, entry),
        }
    }

    pub fn remove(self, text: &str, this: &Path) -> Result<String, Refused> {
        match self {
            Agent::ClaudeDesktop => claude_desktop::remove(text, this),
            Agent::ClaudeCode => claude_code::remove(text, this),
            Agent::Codex => codex::remove(text, this),
        }
    }
}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// What a status row says about one agent's config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// Ours, pointing at this copy.
    Connected,
    NotConnected,
    /// Ours, pointing at another copy of Scriptorium (or one since moved).
    OtherCopy(PathBuf),
    /// Another program's `scriptorium` entry.
    Theirs,
    /// The file can't be read or parsed.
    Unreadable(String),
}

impl Standing {
    pub fn of(state: &State, this: &Path) -> Standing {
        match state {
            State::Missing => Standing::NotConnected,
            State::Ours { command } if command == this => Standing::Connected,
            State::Ours { command } => Standing::OtherCopy(command.clone()),
            State::Theirs => Standing::Theirs,
            State::Broken(why) => Standing::Unreadable(why.clone()),
        }
    }
}

impl fmt::Display for Standing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Standing::Connected => f.write_str("Connected"),
            Standing::NotConnected => f.write_str("Not connected"),
            Standing::OtherCopy(c) => write!(f, "Points at another copy of Scriptorium ({})", c.display()),
            Standing::Theirs => f.write_str("Another program's \"scriptorium\" server is set up there"),
            Standing::Unreadable(why) => write!(f, "Can't read its settings: {why}"),
        }
    }
}

/// Just Scriptorium's part of `agent`'s settings, to add by hand (never a
/// whole file): the `"scriptorium": {…}` member to put under `mcpServers`
/// (Claude Desktop), the `[mcp_servers.scriptorium]` table (Codex), or the
/// `claude mcp add` command (Claude Code).
pub fn fragment(agent: Agent, entry: &Entry) -> Result<String, Refused> {
    match agent {
        Agent::ClaudeDesktop => {
            let fresh: serde_json::Value =
                serde_json::from_str(&claude_desktop::merge("", entry)?).map_err(|e| Refused::Broken(e.to_string()))?;
            let ours = serde_json::to_string_pretty(&fresh["mcpServers"][NAME]).expect("JSON serializes");
            Ok(format!("\"{NAME}\": {ours}\n"))
        }
        Agent::Codex => codex::merge("", entry),
        Agent::ClaudeCode => {
            let mut words = vec!["claude", "mcp", "add", "--scope", "user", "--transport", "stdio", NAME, "--"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            words.push(shell_quote(entry.command.to_str().ok_or(Refused::NotUtf8)?));
            words.extend(entry.args.iter().map(|a| shell_quote(a)));
            Ok(words.join(" ") + "\n")
        }
    }
}

/// The `Scriptorium.app` a connector path is inside, if it is one
/// (`…/Scriptorium.app/Contents/MacOS/scriptorium`).
pub fn app_bundle(this: &Path) -> Option<&Path> {
    let s = this.to_str()?;
    s.strip_suffix(OURS_SUFFIX).filter(|b| b.ends_with(".app")).map(Path::new)
}

/// The Terminal fix for a connector macOS won't run (Gatekeeper quarantine
/// on an unsigned app): clear the attribute on the whole app.
pub fn gatekeeper_fix(this: &Path) -> String {
    let target = app_bundle(this).unwrap_or(this);
    format!("xattr -dr com.apple.quarantine {}", crate::shell_quote(&target.to_string_lossy()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: &str = "/Applications/My Apps/Scriptorium.app/Contents/MacOS/scriptorium";

    #[test]
    fn ids_round_trip() {
        for a in Agent::ALL {
            assert_eq!(Agent::from_id(a.id()), Some(a));
        }
        assert_eq!(Agent::from_id("claude"), None);
    }

    #[test]
    fn config_paths() {
        let home = Path::new("/Users/me");
        assert_eq!(Agent::ClaudeCode.config_path(home), Path::new("/Users/me/.claude.json"));
        assert_eq!(Agent::Codex.config_path(home), Path::new("/Users/me/.codex/config.toml"));
        assert!(Agent::ClaudeDesktop.config_path(home).ends_with("Claude/claude_desktop_config.json"));
    }

    #[test]
    fn dispatch_goes_to_the_format() {
        let this = Path::new(APP);
        for a in Agent::ALL {
            let out = a.merge("", &Entry::new(APP)).unwrap();
            assert_eq!(a.state(&out, this), State::Ours { command: APP.into() });
            assert_eq!(a.state(&a.remove(&out, this).unwrap(), this), State::Missing);
        }
        assert!(Agent::Codex.merge("", &Entry::new(APP)).unwrap().contains("[mcp_servers.scriptorium]"));
    }

    #[test]
    fn standing() {
        let this = Path::new(APP);
        assert_eq!(Standing::of(&State::Ours { command: APP.into() }, this), Standing::Connected);
        let other = PathBuf::from("/Old/Scriptorium.app/Contents/MacOS/scriptorium");
        assert_eq!(Standing::of(&State::Ours { command: other.clone() }, this), Standing::OtherCopy(other));
        assert_eq!(Standing::of(&State::Missing, this), Standing::NotConnected);
        assert_eq!(Standing::of(&State::Theirs, this), Standing::Theirs);
    }

    #[test]
    fn fragments_are_only_ours() {
        let e = Entry::new(APP);
        let d = fragment(Agent::ClaudeDesktop, &e).unwrap();
        assert!(d.starts_with("\"scriptorium\": {") && !d.contains("mcpServers"), "{d}");
        let v: serde_json::Value = serde_json::from_str(&format!("{{{d}}}")).unwrap();
        assert_eq!(v["scriptorium"]["command"], APP);
        let c = fragment(Agent::Codex, &e).unwrap();
        assert!(c.starts_with("[mcp_servers.scriptorium]\n"), "{c}");
        assert_eq!(
            fragment(Agent::ClaudeCode, &e).unwrap(),
            format!("claude mcp add --scope user --transport stdio scriptorium -- '{APP}' mcp\n")
        );
    }

    #[test]
    fn bundle_and_gatekeeper_fix() {
        assert_eq!(app_bundle(Path::new(APP)), Some(Path::new("/Applications/My Apps/Scriptorium.app")));
        assert_eq!(app_bundle(Path::new("/x/target/debug/scriptorium")), None);
        assert_eq!(
            gatekeeper_fix(Path::new(APP)),
            "xattr -dr com.apple.quarantine '/Applications/My Apps/Scriptorium.app'"
        );
    }
}
