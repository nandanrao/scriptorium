//! `scriptorium connect` (plan step E2): the app's "Connect an AI agent…"
//! from the command line. Writes Scriptorium's MCP server entry into Claude
//! Desktop's, Claude Code's or Codex's config, pointing at this very
//! `scriptorium` (`<this> mcp`). The work is in
//! `scriptorium_connect::write`, which the app calls too.

use std::path::{Path, PathBuf};

use scriptorium_connect::agents::{Agent, fragment};
use scriptorium_connect::write::{self, Op, Setup};
use scriptorium_connect::{Entry, setup_command};

use crate::commands::{EXIT_USAGE, Failed};

/// The commands this file handles, dispatched from `commands::run_command`.
pub const COMMANDS: &[&str] = &["connect"];

pub fn command(args: &[&str], notes: &mut Vec<String>) -> Result<String, Failed> {
    let this = this()?;
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| Failed::from("HOME isn't set".to_owned()))?;
    run(args, &home, &this, notes)
}

/// The connector the configs point at: this executable, links resolved
/// (`/usr/local/bin/scriptorium` is a link into the app).
fn this() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("couldn't find this program's own path: {e}"))?;
    Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
}

fn usage() -> Failed {
    Failed::new(EXIT_USAGE, format!("wrong arguments for `connect`\n\n{}", crate::USAGE))
}

fn run(args: &[&str], home: &Path, this: &Path, notes: &mut Vec<String>) -> Result<String, Failed> {
    match args {
        ["--status"] => Ok(status(&setup(home, this))),
        ["--print"] => Ok(format!("{}\n", setup_command(&Entry::new(this)))),
        [agent, flags @ ..] => {
            let agent = Agent::from_id(agent).ok_or_else(|| {
                Failed::new(EXIT_USAGE, format!("unknown agent {agent:?}: use claude-desktop, claude-code or codex"))
            })?;
            let mut print = false;
            let mut op = Op::Connect;
            let mut restart = false;
            let mut mcpb = false;
            for f in flags {
                match *f {
                    "--print" => print = true,
                    "--remove" => op = Op::Disconnect,
                    "--restart" if agent == Agent::ClaudeDesktop => restart = true,
                    "--mcpb" if agent == Agent::ClaudeDesktop => mcpb = true,
                    _ => return Err(usage()),
                }
            }
            if print {
                if op == Op::Disconnect || restart || mcpb {
                    return Err(usage());
                }
                let part = fragment(agent, &Entry::new(this)).map_err(|e| Failed::from(e.to_string()))?;
                return Ok(match agent {
                    Agent::ClaudeDesktop => format!(
                        "Add this under \"mcpServers\" in {}:\n{part}",
                        setup(home, this).config_path(agent).display()
                    ),
                    _ => part,
                });
            }
            connect(agent, op, restart, mcpb, home, this, notes)
        }
        _ => Err(usage()),
    }
}

/// A [`Setup`] for `home`, honouring `$CODEX_HOME` and `$CLAUDE_CONFIG_DIR`.
fn setup(home: &Path, this: &Path) -> Setup {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    Setup { codex_home: var("CODEX_HOME"), claude_config_dir: var("CLAUDE_CONFIG_DIR"), ..Setup::new(home, this) }
}

fn status(setup: &Setup) -> String {
    let mut out = String::new();
    for agent in Agent::ALL {
        let s = write::status(setup, agent);
        out.push_str(&format!("{}: {}\n  {}\n", agent.label(), s.standing, s.path.display()));
    }
    out
}

fn connect(
    agent: Agent,
    op: Op,
    restart: bool,
    mcpb: bool,
    home: &Path,
    this: &Path,
    notes: &mut Vec<String>,
) -> Result<String, Failed> {
    let setup = if agent == Agent::ClaudeCode {
        // Looks for `claude` (on the login PATH: up to 3 s).
        Setup { home: home.to_path_buf(), ..Setup::detect(this).map_err(|e| e.to_string())? }
    } else {
        setup(home, this)
    };
    let result = if mcpb {
        if op == Op::Disconnect {
            return Err(Failed::from(
                "a .mcpb is removed in Claude's settings (Settings › Extensions), not here".to_owned(),
            ));
        }
        write::connect_mcpb(&setup, env!("CARGO_PKG_VERSION"))
    } else if restart {
        write::apply_restarting_desktop(&setup, op, write::QUIT_WAIT)
    } else {
        write::apply(&setup, agent, op)
    };
    let done = match result {
        Ok(done) => done,
        Err(write::Error::DesktopRunning) => {
            return Err(Failed::from(format!(
                "{} Or run `scriptorium connect claude-desktop --restart` to have it quit and reopened.",
                write::Error::DesktopRunning
            )));
        }
        Err(e) => return Err(Failed::from(e.to_string())),
    };
    if let Some(b) = &done.backup {
        notes.push(format!("the previous settings are in {}", b.display()));
    }
    let mut out = format!("{}\n", done.message());
    if op == Op::Connect {
        match write::check_connector(this, home) {
            Ok(_) => {}
            Err(why) => {
                return Err(Failed { stdout: std::mem::take(&mut out), ..Failed::from(why) });
            }
        }
        if agent == Agent::ClaudeDesktop && done.changed && !restart && !mcpb {
            out.push_str("If Claude is open, quit it (⌘Q) and open it again.\n");
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("scriptorium-cli-connect-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const THIS: &str = "/Applications/Scriptorium.app/Contents/MacOS/scriptorium";

    #[test]
    fn print_writes_nothing() {
        let home = temp_home("print");
        let out = run(&["codex", "--print"], &home, Path::new(THIS), &mut vec![]).unwrap();
        assert!(out.starts_with("[mcp_servers.scriptorium]\n") && out.contains(THIS), "{out}");
        let out = run(&["claude-desktop", "--print"], &home, Path::new(THIS), &mut vec![]).unwrap();
        assert!(out.starts_with("Add this under \"mcpServers\" in ") && out.contains("\"scriptorium\": {"), "{out}");
        let out = run(&["claude-code", "--print"], &home, Path::new(THIS), &mut vec![]).unwrap();
        assert!(out.starts_with("claude mcp add --scope user"), "{out}");
        assert!(!home.join(".codex").exists());
        assert_eq!(run(&["--print"], &home, Path::new(THIS), &mut vec![]).unwrap(), format!("{THIS} mcp\n"));
    }

    #[test]
    fn status_lists_every_agent() {
        let home = temp_home("status");
        let out = run(&["--status"], &home, Path::new(THIS), &mut vec![]).unwrap();
        for label in ["Claude Desktop: Not connected", "Claude Code: Not connected", "Codex: Not connected"] {
            assert!(out.contains(label), "{out}");
        }
    }

    #[test]
    fn bad_arguments() {
        let home = temp_home("bad");
        for args in [&["gemini"][..], &["codex", "--restart"], &["codex", "--print", "--remove"], &[]] {
            assert_eq!(run(args, &home, Path::new(THIS), &mut vec![]).unwrap_err().code, EXIT_USAGE, "{args:?}");
        }
    }
}
