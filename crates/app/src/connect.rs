//! Connecting agents and the command line to Scriptorium.
//!
//! Scriptorium › "Install command-line tool…" (step F1): links
//! `scriptorium` onto the user's PATH, like VS Code's "Install 'code'
//! command in PATH". It runs only when chosen; nothing happens at launch.
//! The decisions and the disk work are in `scriptorium_connect::cli_install`.

use gpui::{App, Task, actions};
use scriptorium_connect::cli_install::{self, Outcome};

actions!(scriptorium, [InstallCli]);

/// Run the install off the main thread (it may wait on a login shell and
/// the admin password prompt). Returns the line to show and, when the user
/// must run a command by hand, that command (for the clipboard).
pub fn install_cli(cx: &App) -> Task<(String, Option<String>)> {
    cx.background_executor().spawn(async move {
        let outcome = install();
        let said = cli_install::message(&outcome);
        match outcome {
            Outcome::Manual { command, .. } => (format!("{said} (The command is copied.)"), Some(command)),
            _ => (said, None),
        }
    })
}

#[cfg(unix)]
fn install() -> Outcome {
    match std::env::current_exe() {
        Ok(exe) => cli_install::install(&exe),
        Err(e) => Outcome::Failed(format!("couldn't find the app's own path: {e}")),
    }
}

#[cfg(not(unix))]
fn install() -> Outcome {
    Outcome::Failed("installing the command-line tool isn't supported on this system yet".into())
}
