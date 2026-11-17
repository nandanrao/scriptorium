//! Connecting agents and the command line to Scriptorium.
//!
//! - Scriptorium › "Connect an AI agent…" (step E3): a sheet with a row each
//!   for Claude Desktop, Claude Code and Codex (status, Connect or
//!   Disconnect), and "Copy setup command" for other agents. The writing
//!   is `scriptorium_connect::write`'s, the same as `scriptorium connect`.
//! - The launch check (E3): each agent connected from here before
//!   ([`Remembered`], in `settings.rs`) is looked at once at launch. One
//!   rule: a config is repaired silently only when the agent that owns it
//!   isn't running; otherwise the fix is offered ([`launch_fix`]).
//! - Scriptorium › "Install command-line tool…" (step F1): links
//!   `scriptorium` onto the user's PATH, like VS Code's "Install 'code'
//!   command in PATH". It runs only when chosen; nothing happens at launch.
//!   The decisions and the disk work are in `scriptorium_connect::cli_install`.
//!
//! Every config read and write, `osascript` (is Claude running, quit it)
//! and the connector check run on the background executor, never on the
//! UI thread. Nothing is written while the app runs from a place it must
//! be moved out of first (`cli_install::needs_moving`: translocated, or a
//! disk image under `/Volumes`).

use std::path::{Path, PathBuf};

use gpui::{
    App, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, FontWeight, SharedString, Task, Window, actions,
    div, prelude::*, px, rgba,
};
use gpui_base::Sheet;
use scriptorium_connect::agents::{self, Agent, Standing};
use scriptorium_connect::cli_install::{self, Outcome, needs_moving};
use scriptorium_connect::write::{self, Error, Op, Setup};
use scriptorium_connect::{Entry, State, setup_command};

use crate::theme::*;

actions!(scriptorium, [InstallCli, ConnectAgent]);

/// How Connect sets up Claude Desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopRoute {
    /// Merge `claude_desktop_config.json`, never while Claude runs ("Quit
    /// and reopen Claude for me").
    ConfigFile,
    /// Make a manifest-only `.mcpb` pointing into this app and open it
    /// with Claude, which shows its install screen.
    Mcpb,
}

/// Claude Desktop's route. Hand test 0 (does Desktop accept a
/// manifest-only `.mcpb` whose command is outside it) decides; until then,
/// the config file.
pub const DESKTOP_ROUTE: DesktopRoute = DesktopRoute::ConfigFile;

// ---- Install command-line tool… (F1) ----------------------------------------

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

fn install() -> Outcome {
    match std::env::current_exe() {
        Ok(exe) => cli_install::install(&exe),
        Err(e) => Outcome::Failed(format!("couldn't find the app's own path: {e}")),
    }
}

// ---- what was connected from here (pure) ---------------------------------

/// An agent connected from this app: which, by which route, and the
/// connector its config (or `.mcpb`) points at. One line each in
/// `settings.rs`'s `agents-connected`: `<agent id>\t<file|mcpb>\t<path>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remembered {
    pub agent: Agent,
    /// Claude Desktop through a `.mcpb` (not in its config file).
    pub mcpb: bool,
    pub command: PathBuf,
}

/// Read `agents-connected`; lines that don't parse are skipped.
pub fn parse_remembered(text: &str) -> Vec<Remembered> {
    let mut out: Vec<Remembered> = vec![];
    for line in text.lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(id), Some(how), Some(path)) = (parts.next(), parts.next(), parts.next()) else { continue };
        let Some(agent) = Agent::from_id(id) else { continue };
        let mcpb = match how {
            "file" => false,
            "mcpb" => true,
            _ => continue,
        };
        if path.is_empty() {
            continue;
        }
        remember(&mut out, Remembered { agent, mcpb, command: PathBuf::from(path) });
    }
    out
}

/// Write `agents-connected` (paths that aren't UTF-8 are left out).
pub fn format_remembered(list: &[Remembered]) -> String {
    list.iter()
        .filter_map(|r| {
            let path = r.command.to_str()?;
            Some(format!("{}\t{}\t{path}\n", r.agent.id(), if r.mcpb { "mcpb" } else { "file" }))
        })
        .collect()
}

/// Add `r`, replacing what was remembered for its agent.
pub fn remember(list: &mut Vec<Remembered>, r: Remembered) {
    match list.iter_mut().find(|x| x.agent == r.agent) {
        Some(x) => *x = r,
        None => list.push(r),
    }
}

/// Forget `agent`.
pub fn forget(list: &mut Vec<Remembered>, agent: Agent) {
    list.retain(|r| r.agent != agent);
}

// ---- the launch check's rule (pure) ----------------------------------------

/// What the launch check does with one agent connected before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchFix {
    /// Fine as it is, or not ours to change (another copy of Scriptorium
    /// that exists, another program's entry, a file that doesn't parse).
    Leave,
    /// Repair it now, without asking.
    Silent,
    /// Offer the fix (Claude Desktop is running, or it's a `.mcpb`).
    Offer,
    /// The entry (or the whole file) is gone: never re-added behind the
    /// user's back; the agent is forgotten instead.
    Forget,
}

/// The launch check's one rule: repair silently only a config whose owner
/// isn't running; otherwise offer.
///
/// - `state`: what the config says (for a `.mcpb`, pass
///   `State::Ours { command }` with the remembered command).
/// - `this`: this copy's connector; `exists`: whether a path exists.
/// - `running`: whether Claude Desktop runs; asked only when it matters.
///
/// It needs fixing only when ours points at a path that is gone (the app
/// was moved or renamed). A missing entry or file is never re-added (the
/// user may have removed it): the agent is forgotten. Another copy that
/// exists is left alone (no flip-flopping between two copies). Codex and Claude Code are always repaired
/// silently (Codex reads its config at start; Claude Code goes through
/// `claude mcp`, which is safe with sessions running).
pub fn launch_fix(
    agent: Agent,
    mcpb: bool,
    state: &State,
    this: &Path,
    exists: impl Fn(&Path) -> bool,
    running: impl FnOnce() -> bool,
) -> LaunchFix {
    let needs = match state {
        State::Missing => return LaunchFix::Forget,
        State::Ours { command } => command != this && !exists(command),
        State::Theirs | State::Broken(_) => false,
    };
    if !needs {
        return LaunchFix::Leave;
    }
    match agent {
        Agent::Codex | Agent::ClaudeCode => LaunchFix::Silent,
        Agent::ClaudeDesktop if mcpb => LaunchFix::Offer,
        Agent::ClaudeDesktop if running() => LaunchFix::Offer,
        Agent::ClaudeDesktop => LaunchFix::Silent,
    }
}

/// A row's status line, in plain words.
pub fn status_line(standing: &Standing, exists: impl Fn(&Path) -> bool) -> String {
    match standing {
        Standing::Connected => "Connected".into(),
        Standing::NotConnected => "Not connected".into(),
        Standing::OtherCopy(p) if exists(p) => "Points at another copy of Scriptorium".into(),
        Standing::OtherCopy(_) => "Points at a copy of Scriptorium that was moved or deleted. Connect fixes it.".into(),
        Standing::Theirs => {
            "Another program already uses the name \u{201c}scriptorium\u{201d} there, so Scriptorium left it alone."
                .into()
        }
        Standing::Unreadable(_) => "Its settings file has a mistake in it, so Scriptorium left it alone.".into(),
    }
}

/// Where to put the copied part of a settings file that has a mistake in
/// it (the "Copy settings" button's message).
pub fn fragment_hint(agent: Agent) -> &'static str {
    match agent {
        Agent::ClaudeDesktop => {
            "Copied. Fix the file's mistake first, then add this inside \u{201c}mcpServers\u{201d}."
        }
        Agent::Codex => "Copied. Fix the file's mistake first, then paste this at the end of the file.",
        Agent::ClaudeCode => "Copied. Fix the file's mistake first, then run this in Terminal.",
    }
}

/// The line under Claude Desktop's row.
pub fn desktop_hint(route: DesktopRoute) -> &'static str {
    match route {
        DesktopRoute::ConfigFile => {
            "Claude reads this when it starts: quit Claude (\u{2318}Q) and open it again. If Claude is open, \
             Scriptorium can do that for you."
        }
        DesktopRoute::Mcpb => "Claude shows an install screen: click Install there.",
    }
}

// ---- the work, on the background executor ----------------------------------

/// This copy's connector: the `scriptorium` beside the app.
fn connector() -> Result<PathBuf, String> {
    std::env::current_exe()
        .map(|exe| cli_install::cli_beside(&exe))
        .map_err(|e| format!("couldn't find the app's own path: {e}"))
}

fn exists(p: &Path) -> bool {
    p.exists()
}

fn read_remembered() -> Vec<Remembered> {
    parse_remembered(&crate::settings::agents_connected())
}

fn update_remembered(f: impl FnOnce(&mut Vec<Remembered>)) {
    let mut list = read_remembered();
    let before = list.clone();
    f(&mut list);
    if list != before {
        crate::settings::set_agents_connected(&format_remembered(&list));
    }
}

/// One row as loaded.
#[derive(Debug, Clone)]
struct RowInfo {
    agent: Agent,
    standing: Standing,
    /// [`status_line`], worked out off the UI thread (it looks at the disk).
    status: String,
    /// Where its settings file is (shown for a file with a mistake).
    path: PathBuf,
}

/// What the sheet loads when it opens (and after each change).
struct Loaded {
    setup: Setup,
    rows: Vec<RowInfo>,
}

fn load(this: PathBuf, setup: Option<Setup>) -> Result<Loaded, String> {
    let setup = match setup {
        Some(s) => s,
        None => Setup::detect(this).map_err(|e| e.to_string())?,
    };
    let remembered = read_remembered();
    let rows = Agent::ALL
        .into_iter()
        .map(|agent| {
            let file = write::status(&setup, agent);
            if let Standing::Unreadable(why) = &file.standing {
                crate::log::warn(&format!("connect: {} has a mistake: {why}", file.path.display()));
            }
            let standing = match (agent, DESKTOP_ROUTE) {
                // The extension isn't in the config file: what we made is
                // what we know, and a leftover entry in the file counts too.
                (Agent::ClaudeDesktop, DesktopRoute::Mcpb) => {
                    let ext = match remembered.iter().find(|r| r.agent == agent && r.mcpb) {
                        Some(r) => Standing::of(&State::Ours { command: r.command.clone() }, &setup.this),
                        None => Standing::NotConnected,
                    };
                    match (&ext, &file.standing) {
                        (Standing::Connected, _) | (_, Standing::NotConnected) => ext,
                        _ => file.standing,
                    }
                }
                _ => file.standing,
            };
            let status = status_line(&standing, exists);
            RowInfo { agent, standing, status, path: file.path }
        })
        .collect();
    Ok(Loaded { setup, rows })
}

/// What one action came to, for the sheet.
#[derive(Debug, Clone, Default)]
struct Said {
    text: String,
    warn: bool,
    /// A command for the user to run (the Gatekeeper fix), copied by a button.
    fix: Option<String>,
    /// Claude Desktop is open: offer "Quit and reopen Claude for me" for
    /// this.
    restart: Option<Op>,
}

impl Said {
    fn ok(text: impl Into<String>) -> Said {
        Said { text: text.into(), ..Said::default() }
    }

    fn warn(text: impl Into<String>) -> Said {
        Said { text: text.into(), warn: true, ..Said::default() }
    }
}

/// Connect or disconnect `agent` (`restart`: Claude Desktop may be quit
/// and reopened for it; `mcpb`: Claude Desktop through a `.mcpb`).
/// Connecting first runs the connector the way the agent will, so a
/// Gatekeeper block shows here with its fix, and nothing is written.
fn run_op(setup: &Setup, agent: Agent, op: Op, restart: bool, mcpb: bool) -> Said {
    if needs_moving(&setup.this) {
        return Said::warn("Move Scriptorium to your Applications folder first, then open it from there and connect.");
    }
    if op == Op::Connect
        && let Err(said) = check_helper(setup)
    {
        return said;
    }
    let mcpb = mcpb && agent == Agent::ClaudeDesktop;
    const REMOVE_EXTENSION: &str =
        "To remove Scriptorium's extension, open Claude's Settings \u{203a} Extensions and remove it there.";
    let result = match (mcpb, op) {
        (true, Op::Connect) => write::connect_mcpb(setup, env!("CARGO_PKG_VERSION")),
        // A leftover entry in the config file goes too (below).
        (true, Op::Disconnect) if !matches!(write::status(setup, agent).state, State::Ours { .. }) => {
            update_remembered(|l| forget(l, agent));
            return Said::ok(REMOVE_EXTENSION);
        }
        _ if agent == Agent::ClaudeDesktop && restart => write::apply_restarting_desktop(setup, op, write::QUIT_WAIT),
        _ => write::apply(setup, agent, op),
    };
    match result {
        Ok(done) => {
            match op {
                Op::Connect => {
                    update_remembered(|l| remember(l, Remembered { agent, mcpb, command: setup.this.clone() }))
                }
                Op::Disconnect => update_remembered(|l| forget(l, agent)),
            }
            let mut text = done_line(&done);
            if mcpb && op == Op::Disconnect {
                text = format!("{text} {REMOVE_EXTENSION}");
            }
            Said::ok(text)
        }
        Err(Error::DesktopRunning) => Said {
            text: "Claude is open and would undo this. Quit and reopen it for you?".into(),
            warn: true,
            fix: None,
            restart: Some(op),
        },
        Err(e) => {
            crate::log::warn(&format!("connect {}: {e}", agent.id()));
            let what = if op == Op::Connect { "connect" } else { "disconnect" };
            Said::warn(format!("Couldn't {what} {}. {}", agent.label(), plain_error(agent, &e)))
        }
    }
}

/// Run the helper (`scriptorium`) the way an agent will. On failure, why
/// goes to the log and the sheet says what to do, once.
fn check_helper(setup: &Setup) -> Result<(), Said> {
    let Err(why) = write::check_connector(&setup.this, &setup.home) else { return Ok(()) };
    crate::log::warn(&format!("connect: the helper check failed: {why}"));
    if !setup.this.is_file() {
        return Err(Said::warn("Scriptorium's helper is missing from the app. Download Scriptorium again."));
    }
    if cfg!(target_os = "macos") {
        return Err(Said {
            text: "macOS is blocking Scriptorium's helper. Copy the Terminal command below, paste it into \
                   Terminal, press Return, then click Connect again."
                .into(),
            warn: true,
            fix: Some(agents::gatekeeper_fix(&setup.this)),
            restart: None,
        });
    }
    Err(Said::warn("Scriptorium's helper didn't start, so nothing was changed. The log says why."))
}

/// What a finished change says, in plain words (paths and raw errors go to
/// the log).
fn done_line(done: &write::Done) -> String {
    if let Some(note) = &done.note {
        crate::log::warn(&format!("connect {}: {note}", done.agent.id()));
    }
    if done.how == write::How::Mcpb {
        return match done.note {
            None => "Claude is showing Scriptorium's install screen: click Install there.".into(),
            Some(_) => format!(
                "Scriptorium couldn't open Claude. Open Claude, then double-click this file to install \
                 Scriptorium: {}",
                done.path.display()
            ),
        };
    }
    let base = write::Done { note: None, ..done.clone() }.message();
    match done.note {
        None => base,
        Some(_) => format!("{base} Scriptorium couldn't open Claude again; open it yourself."),
    }
}

/// A failed change, in plain words (the raw error goes to the log).
fn plain_error(agent: Agent, e: &Error) -> String {
    use scriptorium_connect::Refused;
    let a = agent.label();
    match e {
        Error::Refused(Refused::Broken(_)) => {
            "Its settings file has a mistake in it, so Scriptorium left it alone.".into()
        }
        Error::Refused(Refused::Theirs) => {
            "Another program already uses the name \u{201c}scriptorium\u{201d} there, so Scriptorium left it alone."
                .into()
        }
        Error::Refused(Refused::NotUtf8) => {
            "Scriptorium's folder has a name the settings can't hold. Move Scriptorium to your Applications \
             folder and try again."
                .into()
        }
        Error::DesktopDidNotQuit | Error::DesktopQuitFailed(_) => {
            "Claude didn't quit, so nothing was changed. Quit Claude (\u{2318}Q) and try again.".into()
        }
        Error::NotOwner(_) => "Its settings file belongs to another user, so Scriptorium left it alone.".into(),
        Error::ReadOnly(_) => "Its settings file is locked (read-only), so Scriptorium can't change it.".into(),
        Error::Changed { .. } => {
            format!("{a}'s settings changed while Scriptorium was updating them. Quit {a} and try again.")
        }
        Error::ClaudeHalfDone(_) => format!("{a}'s own setup command failed halfway. Click Connect again."),
        Error::Claude(_) => format!("{a}'s own setup command failed. The log says why."),
        // Already plain.
        Error::DesktopAutomationDenied | Error::Root | Error::Translocated | Error::DesktopRunning => e.to_string(),
        Error::NotAbsolute(_) | Error::NotThere(_) | Error::Io(_) => {
            "Something went wrong writing its settings. The log says why.".into()
        }
    }
}

/// What the launch check found.
#[derive(Debug, Default)]
pub struct Checked {
    /// The status-bar line for what was repaired silently.
    pub said: Option<String>,
    /// A repair to offer (Claude Desktop runs, or its `.mcpb` points at a
    /// moved app).
    pub offer: Option<Offer>,
}

/// The fix the launch check offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    /// Claude Desktop's config: through "Quit and reopen Claude for me".
    DesktopConfig,
    /// Make and open the `.mcpb` again.
    DesktopMcpb,
}

impl Offer {
    pub fn text(self) -> &'static str {
        match self {
            Offer::DesktopConfig => "Claude's Scriptorium setting points at a copy of Scriptorium that was moved.",
            Offer::DesktopMcpb => "Claude's Scriptorium extension points at a copy of Scriptorium that was moved.",
        }
    }

    pub fn button(self) -> &'static str {
        match self {
            Offer::DesktopConfig => "Fix (quits and reopens Claude)",
            Offer::DesktopMcpb => "Install it again",
        }
    }
}

/// The agents connected from here before, if any (read on the UI thread:
/// one small settings file).
pub fn connected_before() -> Vec<Remembered> {
    read_remembered()
}

/// The launch check, off the UI thread, for `remembered` (non-empty).
pub fn launch_check(remembered: Vec<Remembered>, cx: &App) -> Task<Checked> {
    cx.background_executor().spawn(async move { check_at_launch(remembered) })
}

fn check_at_launch(remembered: Vec<Remembered>) -> Checked {
    let mut out = Checked::default();
    let Ok(this) = connector() else { return out };
    // Configs must never point into a translocated copy or a disk image.
    if needs_moving(&this) || !this.is_file() {
        return out;
    }
    let setup = match Setup::detect(this.clone()) {
        Ok(s) => s,
        Err(e) => {
            crate::log::warn(&format!("launch check: {e}"));
            return out;
        }
    };
    let mut repaired = vec![];
    let mut forgotten = vec![];
    // The helper is run once, and only if something is to be repaired.
    let mut helper_ok: Option<bool> = None;
    let mcpb_route = DESKTOP_ROUTE == DesktopRoute::Mcpb;
    for r in &remembered {
        // Connected by the other route (before hand test 0 changed it):
        // not ours to repair any more.
        if r.agent == Agent::ClaudeDesktop && r.mcpb != mcpb_route {
            forgotten.push(r.agent);
            continue;
        }
        let state =
            if r.mcpb { State::Ours { command: r.command.clone() } } else { write::status(&setup, r.agent).state };
        match launch_fix(r.agent, r.mcpb, &state, &this, exists, setup.desktop_running) {
            LaunchFix::Leave => {}
            LaunchFix::Forget => forgotten.push(r.agent),
            LaunchFix::Offer => {
                out.offer = Some(if r.mcpb { Offer::DesktopMcpb } else { Offer::DesktopConfig });
            }
            LaunchFix::Silent => {
                let ok = *helper_ok.get_or_insert_with(|| match write::check_connector(&setup.this, &setup.home) {
                    Ok(_) => true,
                    Err(why) => {
                        crate::log::warn(&format!("launch check: the helper doesn't start, nothing repaired: {why}"));
                        false
                    }
                });
                if !ok {
                    continue;
                }
                match write::apply(&setup, r.agent, Op::Connect) {
                    Ok(done) if done.changed => repaired.push(r.agent),
                    Ok(_) => {}
                    // Claude opened meanwhile: offer instead.
                    Err(Error::DesktopRunning) => out.offer = Some(Offer::DesktopConfig),
                    Err(e) => crate::log::warn(&format!("launch check: couldn't repair {}: {e}", r.agent.id())),
                }
            }
        }
    }
    if !repaired.is_empty() || !forgotten.is_empty() {
        update_remembered(|l| {
            for &agent in &forgotten {
                forget(l, agent);
            }
            for &agent in &repaired {
                remember(l, Remembered { agent, mcpb: false, command: this.clone() });
            }
        });
    }
    if !repaired.is_empty() {
        out.said = Some(repaired_line(&repaired));
    }
    out
}

/// The status-bar line for agents repaired silently at launch.
pub fn repaired_line(agents: &[Agent]) -> String {
    let names: Vec<&str> = agents.iter().map(|a| a.label()).collect();
    match names.as_slice() {
        [one] => format!("Updated {one}'s settings to this copy of Scriptorium."),
        _ => format!("Updated the settings of {} to this copy of Scriptorium.", names.join(" and ")),
    }
}

/// The launch check's offered fix, off the UI thread: the line to show.
pub fn fix_offer(offer: Offer, cx: &App) -> Task<String> {
    cx.background_executor().spawn(async move {
        let setup = match connector().and_then(|this| Setup::detect(this).map_err(|e| e.to_string())) {
            Ok(s) => s,
            Err(e) => {
                crate::log::warn(&format!("fix Claude's setting: {e}"));
                return "Couldn't fix Claude's setting. The log says why.".into();
            }
        };
        let said = match offer {
            Offer::DesktopConfig => run_op(&setup, Agent::ClaudeDesktop, Op::Connect, true, false),
            Offer::DesktopMcpb => run_op(&setup, Agent::ClaudeDesktop, Op::Connect, false, true),
        };
        said.text
    })
}

// ---- the sheet -----------------------------------------------------------

/// The sheet asks to be closed.
pub struct Closed;

pub struct ConnectSheet {
    focus: FocusHandle,
    /// This copy's connector (`None`: the app's own path is unknown).
    this: Result<PathBuf, String>,
    /// The app must be moved before anything is written.
    moving: bool,
    setup: Option<Setup>,
    rows: Vec<RowInfo>,
    /// Loading, or an action running: other clicks wait.
    busy: bool,
    said: Option<Said>,
    /// The agent the last message is about (for its restart button).
    said_for: Option<Agent>,
}

impl EventEmitter<Closed> for ConnectSheet {}

impl ConnectSheet {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let this = connector();
        let moving = this.as_ref().is_ok_and(|p| needs_moving(p));
        let mut sheet = Self {
            focus: cx.focus_handle(),
            this,
            moving,
            setup: None,
            rows: vec![],
            busy: false,
            said: None,
            said_for: None,
        };
        sheet.reload(cx);
        sheet
    }

    /// Read every agent's status again (off the UI thread).
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Ok(this) = self.this.clone() else { return };
        self.busy = true;
        let setup = self.setup.clone();
        let task = cx.background_executor().spawn(async move { load(this, setup) });
        cx.spawn(async move |sheet, cx| {
            let loaded = task.await;
            sheet
                .update(cx, |s, cx| {
                    s.busy = false;
                    match loaded {
                        Ok(l) => {
                            s.setup = Some(l.setup);
                            s.rows = l.rows;
                        }
                        Err(e) => s.said = Some(Said::warn(format!("Couldn't look at the agents' settings: {e}"))),
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
        cx.notify();
    }

    fn act(&mut self, agent: Agent, op: Op, restart: bool, cx: &mut Context<Self>) {
        if self.busy || self.moving {
            return;
        }
        let Some(setup) = self.setup.clone() else { return };
        let mcpb = agent == Agent::ClaudeDesktop && DESKTOP_ROUTE == DesktopRoute::Mcpb;
        self.busy = true;
        self.said =
            Some(Said::ok(if restart && agent == Agent::ClaudeDesktop { "Quitting Claude…" } else { "Working…" }));
        self.said_for = Some(agent);
        let task = cx.background_executor().spawn(async move { run_op(&setup, agent, op, restart, mcpb) });
        cx.spawn(async move |sheet, cx| {
            let said = task.await;
            sheet
                .update(cx, |s, cx| {
                    s.busy = false;
                    s.said = Some(said);
                    s.reload(cx);
                })
                .ok();
        })
        .detach();
        cx.notify();
    }

    fn copy(&mut self, text: String, said: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.said = Some(Said::ok(said));
        self.said_for = None;
        cx.notify();
    }

    fn copy_setup(&mut self, cx: &mut Context<Self>) {
        if self.moving {
            return;
        }
        let Ok(this) = &self.this else { return };
        let command = setup_command(&Entry::new(this));
        self.copy(
            command,
            "Copied. In your agent's settings, add an MCP server (\u{201c}stdio\u{201d}) and paste this as its command."
                .into(),
            cx,
        );
    }

    /// For a file we won't touch: just our part, to add by hand.
    fn copy_fragment(&mut self, agent: Agent, cx: &mut Context<Self>) {
        if self.moving {
            return;
        }
        let Ok(this) = &self.this else { return };
        match agents::fragment(agent, &Entry::new(this)) {
            Ok(text) => self.copy(text, fragment_hint(agent).into(), cx),
            Err(e) => {
                crate::log::warn(&format!("connect: no settings to copy for {}: {e}", agent.id()));
                self.said = Some(Said::warn("Scriptorium couldn't make its settings to copy. The log says why."));
                cx.notify();
            }
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(Closed);
    }

    fn render_row(&self, row: &RowInfo, cx: &mut Context<Self>) -> gpui::Div {
        let agent = row.agent;
        let id = agent.id();
        let status = row.status.clone();
        let connected = row.standing == Standing::Connected;
        let color = if connected { OK } else { INK_2 };
        let can_act = !self.busy && !self.moving && self.setup.is_some();
        let button = match &row.standing {
            _ if self.moving => None,
            // The status line says why; Copy setup command covers the rest.
            Standing::Theirs => None,
            Standing::Unreadable(_) => Some(
                crate::script_view::button(SharedString::from(format!("copy-{id}")), "Copy settings", None, false)
                    .on_click(cx.listener(move |this, _, _, cx| this.copy_fragment(agent, cx))),
            ),
            _ if !can_act => None,
            Standing::Connected => Some(
                crate::script_view::button(SharedString::from(format!("disconnect-{id}")), "Disconnect", None, false)
                    .on_click(cx.listener(move |this, _, _, cx| this.act(agent, Op::Disconnect, false, cx))),
            ),
            _ => Some(
                crate::script_view::button(SharedString::from(format!("connect-{id}")), "Connect", None, true)
                    .on_click(cx.listener(move |this, _, _, cx| this.act(agent, Op::Connect, false, cx))),
            ),
        };
        let why = matches!(row.standing, Standing::Unreadable(_)).then(|| format!("The file: {}", row.path.display()));
        let restart = self.said.as_ref().filter(|_| self.said_for == Some(agent) && !self.busy).and_then(|s| s.restart);
        div()
            .flex()
            .flex_col()
            .gap_1()
            .py_3()
            .border_b_1()
            .border_color(RULE)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(agent.label()))
                            .child(div().text_size(px(12.)).text_color(color).child(status)),
                    )
                    .children(button.map(|b| div().flex_none().child(b))),
            )
            .children(why.map(|w| div().text_size(px(12.)).text_color(INK_3).child(w)))
            .when(agent == Agent::ClaudeDesktop, |d| {
                d.child(div().text_size(px(12.)).text_color(INK_3).child(desktop_hint(DESKTOP_ROUTE)))
            })
            .children(restart.map(|op| {
                div().pt_1().child(
                    crate::script_view::button("quit-reopen-claude", "Quit and reopen Claude for me", None, true)
                        .on_click(cx.listener(move |this, _, _, cx| this.act(Agent::ClaudeDesktop, op, true, cx))),
                )
            }))
    }
}

impl Focusable for ConnectSheet {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ConnectSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let me = cx.entity().downgrade();
        let mut body = div().flex().flex_col().gap_3().child(div().text_color(INK_2).child(
            "Let an AI app work on the script open in Scriptorium. Click Connect next to the app you use; \
             it then sees what you see here, and its suggestions show up as you work.",
        ));
        if self.moving {
            body =
                body.child(
                    div().p_3().rounded_md().bg(WARN_SOFT).text_color(WARN).font_weight(FontWeight::SEMIBOLD).child(
                        "Move Scriptorium to your Applications folder first, then open it from there and connect.",
                    ),
                );
        }
        if let Err(e) = &self.this {
            body = body.child(div().text_color(WARN).child(e.clone()));
        }
        let rows: Vec<gpui::Div> = if self.rows.is_empty() && self.busy {
            vec![div().text_color(INK_3).child("Looking at your agents\u{2019} settings\u{2026}")]
        } else {
            let rows = self.rows.clone();
            rows.iter().map(|r| self.render_row(r, cx)).collect()
        };
        body = body.child(div().flex().flex_col().children(rows));
        if let Ok(this) = &self.this
            && !self.moving
        {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Other agents"))
                    .child(div().text_size(px(12.)).text_color(INK_2).child(
                        "Any other app that can use MCP servers can connect too. Add a server there with this \
                         command:",
                    ))
                    .child(
                        div()
                            .p_2()
                            .rounded_md()
                            .bg(CHROME)
                            .border_1()
                            .border_color(RULE)
                            .text_size(px(12.))
                            .child(setup_command(&Entry::new(this))),
                    )
                    .child(
                        div().flex().child(
                            crate::script_view::button("copy-setup", "Copy setup command", None, false)
                                .on_click(cx.listener(|this, _, _, cx| this.copy_setup(cx))),
                        ),
                    ),
            );
        }
        if let Some(said) = self.said.clone() {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(if said.warn { WARN } else { INK })
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(said.text),
                    )
                    .children(said.fix.map(|fix| {
                        div().flex().child(
                            crate::script_view::button("copy-fix", "Copy the Terminal command", None, false).on_click(
                                cx.listener(move |this, _, _, cx| this.copy(fix.clone(), "Copied.".into(), cx)),
                            ),
                        )
                    })),
            );
        }
        let buttons = div().flex().justify_end().gap_2().child(
            crate::script_view::button("connect-close", "Close", Some("Esc"), false)
                .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
        );
        Sheet::new(cx)
            .focus_handle(self.focus.clone())
            .request_close(move |_, cx| {
                me.update(cx, |s, cx| s.close(cx)).ok();
            })
            .overlay(div().absolute().inset_0().bg(rgba(0x1c1f2455)))
            .surface(
                div()
                    .absolute()
                    .right_0()
                    .top_0()
                    .h_full()
                    .w(px(440.))
                    .occlude()
                    .bg(PAPER)
                    .border_l_1()
                    .border_color(RULE)
                    .flex()
                    .flex_col()
                    .text_size(px(13.))
                    .text_color(INK)
                    .child(
                        div().flex_none().px_4().py_3().border_b_1().border_color(RULE).child(
                            div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child("Connect an AI agent"),
                        ),
                    )
                    .child(div().id("connect-body").flex_1().min_h_0().overflow_y_scroll().p_4().child(body))
                    .child(div().flex_none().px_4().py_3().border_t_1().border_color(RULE).child(buttons)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THIS: &str = "/Applications/Scriptorium.app/Contents/MacOS/scriptorium";
    const MOVED: &str = "/Users/me/Downloads/Scriptorium.app/Contents/MacOS/scriptorium";
    const OTHER: &str = "/Users/me/Applications/Scriptorium.app/Contents/MacOS/scriptorium";

    /// Only `OTHER` (and this copy) exists.
    fn exists(p: &Path) -> bool {
        p == Path::new(OTHER) || p == Path::new(THIS)
    }

    fn ours(p: &str) -> State {
        State::Ours { command: p.into() }
    }

    fn fix(agent: Agent, mcpb: bool, state: State, running: bool) -> LaunchFix {
        launch_fix(agent, mcpb, &state, Path::new(THIS), exists, || running)
    }

    #[test]
    fn launch_fix_leaves_what_is_fine_or_not_ours() {
        for a in Agent::ALL {
            for running in [false, true] {
                assert_eq!(fix(a, false, ours(THIS), running), LaunchFix::Leave);
                // Another copy that exists: no flip-flopping.
                assert_eq!(fix(a, false, ours(OTHER), running), LaunchFix::Leave);
                assert_eq!(fix(a, false, State::Theirs, running), LaunchFix::Leave);
                assert_eq!(fix(a, false, State::Broken("x".into()), running), LaunchFix::Leave);
                // A missing entry or file is never re-added.
                assert_eq!(fix(a, false, State::Missing, running), LaunchFix::Forget);
            }
        }
    }

    #[test]
    fn codex_and_claude_code_are_repaired_silently() {
        for a in [Agent::Codex, Agent::ClaudeCode] {
            for running in [false, true] {
                assert_eq!(fix(a, false, ours(MOVED), running), LaunchFix::Silent);
            }
        }
    }

    #[test]
    fn desktop_is_repaired_silently_only_when_it_isnt_running() {
        assert_eq!(fix(Agent::ClaudeDesktop, false, ours(MOVED), false), LaunchFix::Silent);
        assert_eq!(fix(Agent::ClaudeDesktop, false, ours(MOVED), true), LaunchFix::Offer);
    }

    #[test]
    fn running_is_asked_only_when_it_matters() {
        let asked = std::cell::Cell::new(false);
        let r = launch_fix(Agent::ClaudeDesktop, false, &ours(THIS), Path::new(THIS), exists, || {
            asked.set(true);
            true
        });
        assert_eq!(r, LaunchFix::Leave);
        assert!(!asked.get());
    }

    #[test]
    fn a_moved_mcpb_is_offered_again() {
        for running in [false, true] {
            assert_eq!(fix(Agent::ClaudeDesktop, true, ours(MOVED), running), LaunchFix::Offer);
            assert_eq!(fix(Agent::ClaudeDesktop, true, ours(THIS), running), LaunchFix::Leave);
            assert_eq!(fix(Agent::ClaudeDesktop, true, ours(OTHER), running), LaunchFix::Leave);
        }
    }

    #[test]
    fn remembered_round_trips_and_skips_junk() {
        let list = vec![
            Remembered { agent: Agent::Codex, mcpb: false, command: THIS.into() },
            Remembered { agent: Agent::ClaudeDesktop, mcpb: true, command: "/My Apps/Scénario\tx".into() },
        ];
        let text = format_remembered(&list);
        assert_eq!(parse_remembered(&text), list);
        let junk = "nonsense\nclaude-code\tweird\t/x\ncodex\tfile\t\nclaude-code\tfile\t/a\nclaude-code\tfile\t/b\n";
        assert_eq!(
            parse_remembered(junk),
            vec![Remembered { agent: Agent::ClaudeCode, mcpb: false, command: "/b".into() }]
        );
    }

    #[test]
    fn remember_replaces_and_forget_removes() {
        let mut l = vec![];
        remember(&mut l, Remembered { agent: Agent::Codex, mcpb: false, command: MOVED.into() });
        remember(&mut l, Remembered { agent: Agent::Codex, mcpb: false, command: THIS.into() });
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].command, Path::new(THIS));
        forget(&mut l, Agent::Codex);
        assert!(l.is_empty());
    }

    #[test]
    fn status_lines_are_plain() {
        assert_eq!(status_line(&Standing::Connected, exists), "Connected");
        assert_eq!(status_line(&Standing::NotConnected, exists), "Not connected");
        assert_eq!(status_line(&Standing::OtherCopy(OTHER.into()), exists), "Points at another copy of Scriptorium");
        assert!(status_line(&Standing::OtherCopy(MOVED.into()), exists).contains("moved or deleted"));
        let unreadable = status_line(&Standing::Unreadable("key must be a string".into()), exists);
        assert_eq!(unreadable, "Its settings file has a mistake in it, so Scriptorium left it alone.");
        assert!(status_line(&Standing::Theirs, exists).contains("left it alone"));
    }

    #[test]
    fn fragment_hints_say_where() {
        assert!(fragment_hint(Agent::ClaudeDesktop).contains("mcpServers"));
        assert!(fragment_hint(Agent::Codex).contains("end of the file"));
    }

    #[test]
    fn errors_are_plain() {
        use scriptorium_connect::Refused;
        let e = Error::Refused(Refused::Broken("expected value at line 1".into()));
        assert!(!plain_error(Agent::Codex, &e).contains("line 1"));
        assert!(!plain_error(Agent::Codex, &Error::Io("EACCES /x/y".into())).contains("/x/y"));
        let changed = Error::Changed { agent: Agent::Codex, path: "/h/.codex/config.toml".into() };
        assert!(!plain_error(Agent::Codex, &changed).contains("/h/"));
    }

    #[test]
    fn repaired_lines() {
        assert_eq!(repaired_line(&[Agent::Codex]), "Updated Codex's settings to this copy of Scriptorium.");
        assert_eq!(
            repaired_line(&[Agent::Codex, Agent::ClaudeCode]),
            "Updated the settings of Codex and Claude Code to this copy of Scriptorium."
        );
    }

    #[test]
    fn the_route_defaults_to_the_config_file() {
        assert_eq!(DESKTOP_ROUTE, DesktopRoute::ConfigFile);
        assert!(desktop_hint(DesktopRoute::ConfigFile).contains("quit Claude"));
    }
}
