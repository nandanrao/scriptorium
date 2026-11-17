//! Writing agent configs (step E2; feature `shell`): read, merge, check
//! nobody changed the file meanwhile, back up (`<file>.scriptorium-backup`,
//! the last one kept, and `<file>.scriptorium-original`, the first, never
//! overwritten), write atomically (`scriptorium_fsio`), read back.
//!
//! - Claude Code goes through its own `claude mcp add/remove --scope user`
//!   when `claude` is found (running sessions rewrite `~/.claude.json` all
//!   the time), and otherwise merges the file.
//! - Claude Desktop rewrites its file from memory, so it is never written
//!   while Desktop runs: [`apply`] says [`Error::DesktopRunning`], and
//!   [`apply_restarting_desktop`] quits it, waits, writes and reopens it.
//!   The `.mcpb` route ([`connect_mcpb`]) is ready beside it; hand test 0
//!   picks one.
//! - [`check_connector`] runs `<connector> --version` the way an agent will.
//!
//! The app (E3) and `scriptorium connect` both call these.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::agents::{Agent, CLAUDE_DESKTOP_BUNDLE_ID, Standing, gatekeeper_fix};
use crate::cli_install::is_translocated;
use crate::{Entry, NAME, Refused, State, mcpb};

/// How long Claude Desktop gets to quit before we give up.
pub const QUIT_WAIT: Duration = Duration::from_secs(30);

/// How long `claude mcp …` may take.
const CLAUDE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the connector's `--version` may take (a first Gatekeeper scan
/// can be slow).
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);

/// The PATH agents launched from the Finder get, near enough.
const MINIMAL_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// What Connect works with.
#[derive(Debug, Clone)]
pub struct Setup {
    /// The user's home folder; every config is under it.
    pub home: PathBuf,
    /// The connector the configs point at (absolute).
    pub this: PathBuf,
    /// The `claude` command, when found: Claude Code is set up through it.
    pub claude: Option<PathBuf>,
    /// `PATH` for the commands we run (`claude` may be a Node script, and
    /// a GUI app has launchd's minimal PATH).
    pub path_env: Option<OsString>,
    /// Whether Claude Desktop is running now (unknown counts as running).
    pub desktop_running: fn() -> bool,
    /// `$CODEX_HOME`, where Codex keeps `config.toml` when set.
    pub codex_home: Option<PathBuf>,
    /// `$CLAUDE_CONFIG_DIR`, where Claude Code keeps `.claude.json` when set.
    pub claude_config_dir: Option<PathBuf>,
}

impl Setup {
    /// For `home` and the connector `this`, without looking for `claude`.
    pub fn new(home: impl Into<PathBuf>, this: impl Into<PathBuf>) -> Setup {
        Setup {
            home: home.into(),
            this: this.into(),
            claude: None,
            path_env: None,
            desktop_running: desktop::is_running,
            codex_home: None,
            claude_config_dir: None,
        }
    }

    /// `agent`'s config file, honouring [`Setup::codex_home`] and
    /// [`Setup::claude_config_dir`].
    pub fn config_path(&self, agent: Agent) -> PathBuf {
        match (agent, &self.codex_home, &self.claude_config_dir) {
            (Agent::Codex, Some(dir), _) => dir.join("config.toml"),
            (Agent::ClaudeCode, _, Some(dir)) => dir.join(".claude.json"),
            _ => agent.config_path(&self.home),
        }
    }

    /// `$HOME`, `$CODEX_HOME`, `$CLAUDE_CONFIG_DIR`, and `claude` looked up on the login PATH (which takes the
    /// login shell up to 3 s) and in the usual places.
    pub fn detect(this: impl Into<PathBuf>) -> Result<Setup, Error> {
        let home = home().ok_or_else(|| Error::Io("HOME isn't set".into()))?;
        let mut setup = Setup::new(home, this);
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        setup.codex_home = var("CODEX_HOME");
        setup.claude_config_dir = var("CLAUDE_CONFIG_DIR");
        let login = crate::cli_install::login_path();
        setup.claude = find_claude(&login, &setup.home);
        setup.path_env = std::env::join_paths(&login).ok();
        Ok(setup)
    }

    fn entry(&self) -> Entry {
        Entry::new(&self.this)
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

/// Where `claude` may be: the login PATH, then `~/.local/bin`,
/// `~/.claude/local`, `/opt/homebrew/bin`.
pub fn claude_candidates(login_path: &[PathBuf], home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = login_path.iter().filter(|d| d.is_absolute()).cloned().collect();
    for d in [home.join(".local/bin"), home.join(".claude/local"), PathBuf::from("/opt/homebrew/bin")] {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    dirs.into_iter().map(|d| d.join("claude")).collect()
}

/// The first `claude` among [`claude_candidates`] that is an executable file.
pub fn find_claude(login_path: &[PathBuf], home: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    claude_candidates(login_path, home)
        .into_iter()
        .find(|p| fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// Connect or disconnect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Connect,
    Disconnect,
}

/// How the change was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// The config file was merged and written.
    File,
    /// Through `claude mcp add/remove --scope user`.
    ClaudeCli,
    /// A `.mcpb` was made and opened with Claude Desktop.
    Mcpb,
}

/// What [`apply`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub agent: Agent,
    pub op: Op,
    /// The config file (for [`How::Mcpb`], the `.mcpb`).
    pub path: PathBuf,
    /// False when it was already so; nothing was written.
    pub changed: bool,
    /// The copy of the file as it was before.
    pub backup: Option<PathBuf>,
    pub how: How,
    /// Something to add (Claude didn't reopen, the `.mcpb` wasn't opened).
    pub note: Option<String>,
}

impl Done {
    /// The line the sheet and the CLI show.
    pub fn message(&self) -> String {
        let a = self.agent.label();
        let mut s = match (self.op, self.changed, self.how) {
            (Op::Connect, _, How::Mcpb) => {
                format!("Opened Scriptorium's extension with Claude ({}): install it there.", self.path.display())
            }
            (Op::Connect, false, _) => format!("{a} is already connected."),
            (Op::Connect, true, _) => match self.agent {
                Agent::ClaudeDesktop => {
                    "Connected Claude Desktop. Open Claude (or reopen it) to use Scriptorium.".into()
                }
                _ => format!("Connected {a}. New {a} sessions can use Scriptorium."),
            },
            (Op::Disconnect, false, _) => format!("{a} wasn't connected."),
            (Op::Disconnect, true, _) => format!("Disconnected {a}."),
        };
        if let Some(n) = &self.note {
            s.push(' ');
            s.push_str(n);
        }
        s
    }
}

/// Why nothing (or not everything) was done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The file doesn't parse, or another program's `scriptorium` is there.
    Refused(Refused),
    /// Claude Desktop is running; it would overwrite the file.
    DesktopRunning,
    /// Asked to quit, Claude Desktop was still running after the wait (or
    /// the user cancelled its quit, say at an unsaved-work question).
    DesktopDidNotQuit,
    /// macOS didn't let Scriptorium ask Claude to quit (Automation).
    DesktopAutomationDenied,
    /// Asking Claude to quit failed otherwise.
    DesktopQuitFailed(String),
    /// Run as root: the configs would end up owned by root.
    Root,
    /// The file belongs to another user.
    NotOwner(PathBuf),
    /// `claude mcp remove` took out our old entry, then `claude mcp add`
    /// failed.
    ClaudeHalfDone(String),
    /// The file changed between our read and our write; not written.
    Changed {
        agent: Agent,
        path: PathBuf,
    },
    /// The file is read-only.
    ReadOnly(PathBuf),
    /// Run from App Translocation: configs must never point there.
    Translocated,
    /// The connector's path isn't absolute.
    NotAbsolute(PathBuf),
    /// `claude mcp …` failed.
    Claude(String),
    /// The file didn't read back as written.
    NotThere(PathBuf),
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Refused(r) => write!(f, "{r}"),
            Error::DesktopRunning => {
                f.write_str("Claude Desktop is open, and it would overwrite its settings. Quit Claude (⌘Q), then connect again, or let Scriptorium quit and reopen it.")
            }
            Error::DesktopDidNotQuit => f.write_str("Claude didn't quit; quit it yourself (⌘Q) and try again. Nothing was written."),
            Error::DesktopAutomationDenied => f.write_str("macOS didn't let Scriptorium quit Claude. Allow Scriptorium to control Claude in System Settings › Privacy & Security › Automation, or quit Claude yourself (⌘Q) and try again. Nothing was written."),
            Error::DesktopQuitFailed(why) => write!(f, "Couldn't ask Claude to quit ({why}). Quit it yourself (⌘Q) and try again. Nothing was written."),
            Error::Root => f.write_str("Scriptorium is running as root; run it as yourself so the settings stay yours. Nothing was written."),
            Error::NotOwner(p) => write!(f, "{} belongs to another user, so Scriptorium left it alone.", p.display()),
            Error::ClaudeHalfDone(why) => write!(f, "the `claude` command failed: {why}. The old entry was removed; run connect again."),
            Error::Changed { agent: Agent::ClaudeCode, path } => write!(
                f,
                "{} changed while Scriptorium was updating it. Quit Claude Code first, then try again. Nothing was written.",
                path.display()
            ),
            Error::Changed { agent, path } => write!(
                f,
                "{} changed while Scriptorium was updating it. Quit {agent}, then try again. Nothing was written.",
                path.display()
            ),
            Error::ReadOnly(p) => write!(f, "{} is read-only, so Scriptorium can't add itself there.", p.display()),
            Error::Translocated => f.write_str("Move Scriptorium to your Applications folder first, then connect again."),
            Error::NotAbsolute(p) => write!(f, "Scriptorium's path isn't absolute: {}", p.display()),
            Error::Claude(why) => write!(f, "the `claude` command failed: {why}"),
            Error::NotThere(p) => write!(f, "{} didn't read back as written", p.display()),
            Error::Io(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Error {}

impl From<Refused> for Error {
    fn from(r: Refused) -> Self {
        Error::Refused(r)
    }
}

/// One agent's row: its file and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub agent: Agent,
    pub path: PathBuf,
    pub state: State,
    pub standing: Standing,
}

/// What `agent`'s config says now.
pub fn status(setup: &Setup, agent: Agent) -> Status {
    let path = setup.config_path(agent);
    let state = match read(&path) {
        Ok(None) => State::Missing,
        Ok(Some(bytes)) => match String::from_utf8(bytes) {
            Ok(text) => agent.state(&text, &setup.this),
            Err(_) => State::Broken("not UTF-8 text".into()),
        },
        Err(e) => State::Broken(e.to_string()),
    };
    let standing = Standing::of(&state, &setup.this);
    Status { agent, path, state, standing }
}

/// Connect or disconnect `agent`. Claude Desktop's file is not written
/// while it runs ([`Error::DesktopRunning`]; see [`apply_restarting_desktop`]).
pub fn apply(setup: &Setup, agent: Agent, op: Op) -> Result<Done, Error> {
    apply_with(setup, agent, op, || {})
}

fn apply_with(setup: &Setup, agent: Agent, op: Op, between: impl FnOnce()) -> Result<Done, Error> {
    check_this(&setup.this)?;
    if agent == Agent::ClaudeCode
        && let Some(claude) = &setup.claude
    {
        return via_claude_cli(setup, claude, op);
    }
    let path = setup.config_path(agent);
    let entry = setup.entry();
    let this = setup.this.clone();
    let guard = || {
        if agent == Agent::ClaudeDesktop && (setup.desktop_running)() { Err(Error::DesktopRunning) } else { Ok(()) }
    };
    let (changed, backup) = match op {
        Op::Connect => edit_file(agent, &path, |t| agent.merge(t, &entry), guard, between)?,
        Op::Disconnect => edit_file(agent, &path, |t| agent.remove(t, &this), guard, between)?,
    };
    if changed {
        verify(setup, agent, op, &path)?;
    }
    Ok(Done { agent, op, path, changed, backup, how: How::File, note: None })
}

/// "Quit and reopen Claude for me": if Claude Desktop runs and the file
/// needs changing, ask it to quit, wait up to `wait`, write, and reopen it
/// (also when the write failed, since we quit it). If it doesn't quit,
/// nothing is written.
pub fn apply_restarting_desktop(setup: &Setup, op: Op, wait: Duration) -> Result<Done, Error> {
    check_this(&setup.this)?;
    if !(setup.desktop_running)() || !needs_change(setup, Agent::ClaudeDesktop, op)? {
        return apply(setup, Agent::ClaudeDesktop, op);
    }
    desktop::quit(wait)?;
    let result = apply(setup, Agent::ClaudeDesktop, op);
    let reopened = desktop::reopen();
    match (result, reopened) {
        (Ok(mut done), Err(e)) => {
            done.note = Some(format!("Couldn't reopen Claude ({e}); open it yourself."));
            Ok(done)
        }
        (result, _) => result,
    }
}

/// Whether `op` would change `agent`'s file (a refusal is an error).
fn needs_change(setup: &Setup, agent: Agent, op: Op) -> Result<bool, Error> {
    let path = setup.config_path(agent);
    let text = read_text(&path)?.unwrap_or_default();
    let out = match op {
        Op::Connect => agent.merge(&text, &setup.entry())?,
        Op::Disconnect => agent.remove(&text, &setup.this)?,
    };
    Ok(out != text)
}

fn check_this(this: &Path) -> Result<(), Error> {
    if nix::unistd::geteuid().is_root() {
        return Err(Error::Root);
    }
    if is_translocated(this) {
        return Err(Error::Translocated);
    }
    if !this.is_absolute() {
        return Err(Error::NotAbsolute(this.to_path_buf()));
    }
    Ok(())
}

/// The file as it should read after `op`.
fn verify(setup: &Setup, agent: Agent, op: Op, path: &Path) -> Result<(), Error> {
    let state = agent.state(&read_text(path)?.unwrap_or_default(), &setup.this);
    let ok = match op {
        Op::Connect => state == State::Ours { command: setup.this.clone() },
        Op::Disconnect => !matches!(state, State::Ours { .. }),
    };
    if ok { Ok(()) } else { Err(Error::NotThere(path.to_path_buf())) }
}

/// The file's bytes, or `None` if there is none.
fn read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io::Error::new(e.kind(), format!("can't read {}: {e}", path.display()))),
    }
}

fn read_text(path: &Path) -> Result<Option<String>, Error> {
    match read(path).map_err(|e| Error::Io(e.to_string()))? {
        None => Ok(None),
        Some(b) => String::from_utf8(b)
            .map(Some)
            .map_err(|_| Error::Refused(Refused::Broken(format!("{} isn't UTF-8 text", path.display())))),
    }
}

/// Refuse a file owned by someone other than `euid`.
fn check_owner(path: &Path, euid: u32) -> Result<(), Error> {
    use std::os::unix::fs::MetadataExt;
    match fs::metadata(path) {
        Ok(m) if m.uid() != euid => Err(Error::NotOwner(path.to_path_buf())),
        _ => Ok(()),
    }
}

fn euid() -> u32 {
    nix::unistd::geteuid().as_raw()
}

/// Where the first copy we ever saw of `path` goes; never overwritten.
pub fn original_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(OsString::from).unwrap_or_default();
    name.push(".scriptorium-original");
    path.with_file_name(name)
}

/// Keep `before` (the file as read): as the original if there is none yet,
/// and as the last backup. Returns the backup's path.
fn keep_copies(path: &Path, before: &[u8]) -> Result<PathBuf, Error> {
    let original = original_path(path);
    if fs::symlink_metadata(&original).is_err() {
        scriptorium_fsio::write_atomic_private(&original, before)
            .map_err(|e| Error::Io(format!("can't keep a copy in {}: {e}", original.display())))?;
    }
    let at = backup_path(path);
    scriptorium_fsio::write_atomic_private(&at, before)
        .map_err(|e| Error::Io(format!("can't back up to {}: {e}", at.display())))?;
    Ok(at)
}

/// Where the last backup of `path` goes.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(OsString::from).unwrap_or_default();
    name.push(".scriptorium-backup");
    path.with_file_name(name)
}

/// Read `path`, transform it, and if that changes it: `guard`, back up,
/// check it is unchanged since the read, write. `between` runs just before
/// that check (tests change the file there). Returns whether it wrote, and
/// the backup.
fn edit_file(
    agent: Agent,
    path: &Path,
    transform: impl FnOnce(&str) -> Result<String, Refused>,
    guard: impl FnOnce() -> Result<(), Error>,
    between: impl FnOnce(),
) -> Result<(bool, Option<PathBuf>), Error> {
    let io = |e: io::Error| Error::Io(e.to_string());
    let before = read(path).map_err(io)?;
    let text = match &before {
        None => String::new(),
        Some(b) => std::str::from_utf8(b)
            .map_err(|_| Refused::Broken(format!("{} isn't UTF-8 text", path.display())))?
            .to_owned(),
    };
    let new = transform(&text)?;
    if new == text {
        return Ok((false, None));
    }
    guard()?;
    check_owner(path, euid())?;
    if before.is_some() && fs::metadata(path).is_ok_and(|m| m.permissions().readonly()) {
        return Err(Error::ReadOnly(path.to_path_buf()));
    }
    let is_link = fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    if !is_link && let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| Error::Io(format!("can't make {}: {e}", dir.display())))?;
    }
    between();
    if read(path).map_err(io)? != before {
        return Err(Error::Changed { agent, path: path.to_path_buf() });
    }
    let backup = match &before {
        Some(b) if !b.is_empty() => Some(keep_copies(path, b)?),
        _ => None,
    };
    // Through a link (dotfile managers) to its file; a new or plain file
    // is created readable by its owner only (configs may hold tokens).
    let written = if is_link {
        scriptorium_fsio::write_atomic(path, new.as_bytes())
    } else {
        scriptorium_fsio::write_atomic_private(path, new.as_bytes())
    };
    written.map_err(|e| Error::Io(format!("can't write {}: {e}", path.display())))?;
    Ok((true, backup))
}

/// Claude Code through its own command, which is safe with sessions
/// running. A stale entry of ours is removed first.
fn via_claude_cli(setup: &Setup, claude: &Path, op: Op) -> Result<Done, Error> {
    let agent = Agent::ClaudeCode;
    let path = setup.config_path(agent);
    let text = read_text(&path)?;
    let state = agent.state(text.as_deref().unwrap_or(""), &setup.this);
    let this = setup.this.to_str().ok_or(Refused::NotUtf8)?;
    let (remove, add) = match (op, &state) {
        (_, State::Broken(why)) => return Err(Refused::Broken(why.clone()).into()),
        (_, State::Theirs) => return Err(Refused::Theirs.into()),
        (Op::Connect, State::Ours { command }) if command == &setup.this => (false, false),
        (Op::Connect, State::Ours { .. }) => (true, true),
        (Op::Connect, State::Missing) => (false, true),
        (Op::Disconnect, State::Ours { .. }) => (true, false),
        (Op::Disconnect, State::Missing) => (false, false),
    };
    let mut done =
        Done { agent, op, path: path.clone(), changed: false, backup: None, how: How::ClaudeCli, note: None };
    if !remove && !add {
        return Ok(done);
    }
    check_owner(&path, euid())?;
    if let Some(t) = text.filter(|t| !t.is_empty()) {
        done.backup = Some(keep_copies(&path, t.as_bytes())?);
    }
    if remove {
        run_claude(setup, claude, &["mcp", "remove", "--scope", "user", NAME])?;
    }
    if add {
        let added = run_claude(
            setup,
            claude,
            &["mcp", "add", "--scope", "user", "--transport", "stdio", NAME, "--", this, "mcp"],
        );
        match added {
            Err(Error::Claude(why)) if remove => return Err(Error::ClaudeHalfDone(why)),
            other => other?,
        }
    }
    done.changed = true;
    verify(setup, agent, op, &path)?;
    Ok(done)
}

fn run_claude(setup: &Setup, claude: &Path, args: &[&str]) -> Result<(), Error> {
    let mut cmd = Command::new(claude);
    cmd.args(args).current_dir(&setup.home).env("HOME", &setup.home);
    if let Some(p) = &setup.path_env {
        cmd.env("PATH", p);
    }
    let out = run(cmd, CLAUDE_TIMEOUT).map_err(Error::Claude)?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let why =
            if err.trim().is_empty() { String::from_utf8_lossy(&out.stdout).into_owned() } else { err.into_owned() };
        Err(Error::Claude(format!("`claude {}`: {}", args.join(" "), why.trim())))
    }
}

/// Run `cmd` with no stdin, collecting its output, killing it after
/// `timeout`.
fn run(mut cmd: Command, timeout: Duration) -> Result<Output, String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // A program just written may still be open for writing in a child
    // another thread is forking (ETXTBSY); that passes in a moment.
    let mut tries = 0;
    let mut child = loop {
        match cmd.spawn() {
            Ok(c) => break c,
            Err(e) if e.kind() == io::ErrorKind::ExecutableFileBusy && tries < 20 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(format!("couldn't start it: {e}")),
        }
    };
    let pipe = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = pipe(child.stdout.take().map(|r| Box::new(r) as Box<dyn Read + Send>));
    let err = pipe(child.stderr.take().map(|r| Box::new(r) as Box<dyn Read + Send>));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("it didn't finish in {} s", timeout.as_secs()));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    Ok(Output { status, stdout: out.join().unwrap_or_default(), stderr: err.join().unwrap_or_default() })
}

/// Run the connector the way an agent will (absolute path, no shell, `cwd`
/// `/`, only `HOME` and a minimal `PATH`) with `--version`. Returns what it
/// printed, or why it failed, with the Gatekeeper fix on macOS.
pub fn check_connector(this: &Path, home: &Path) -> Result<String, String> {
    let mut cmd = Command::new(this);
    cmd.arg("--version").current_dir("/").env_clear().env("HOME", home).env("PATH", MINIMAL_PATH);
    let why = match run(cmd, CHECK_TIMEOUT) {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if text.starts_with(NAME) {
                return Ok(text);
            }
            format!("it printed {text:?} for --version")
        }
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            match (out.status.code(), err.is_empty()) {
                (Some(c), true) => format!("it exited with status {c}"),
                (None, true) => "macOS stopped it".to_owned(),
                (_, false) => err,
            }
        }
        Err(e) => e,
    };
    let mut msg = format!("{} can't be started: {why}.", this.display());
    if cfg!(target_os = "macos") {
        msg.push_str(&format!(" If macOS blocked it, run this in Terminal, then try again: {}", gatekeeper_fix(this)));
    }
    Err(msg)
}

/// Where [`connect_mcpb`] puts the `.mcpb`: Scriptorium's own support folder.
pub fn mcpb_path(home: &Path) -> PathBuf {
    let dir = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Scriptorium")
    } else {
        home.join(".local/share/scriptorium")
    };
    dir.join(mcpb::FILE_NAME)
}

/// Write the manifest-only `.mcpb` for this connector to `at`.
pub fn write_mcpb(setup: &Setup, at: &Path, version: &str) -> Result<(), Error> {
    check_this(&setup.this)?;
    let bytes = mcpb::bytes(&setup.entry(), version)?;
    if let Some(dir) = at.parent() {
        fs::create_dir_all(dir).map_err(|e| Error::Io(format!("can't make {}: {e}", dir.display())))?;
    }
    scriptorium_fsio::write_atomic_no_link(at, &bytes)
        .map_err(|e| Error::Io(format!("can't write {}: {e}", at.display())))
}

/// Claude Desktop's `.mcpb` route: write it ([`mcpb_path`]) and open it
/// with Claude, which shows its install screen (macOS; elsewhere it is
/// only written, and the note says where).
pub fn connect_mcpb(setup: &Setup, version: &str) -> Result<Done, Error> {
    let at = mcpb_path(&setup.home);
    write_mcpb(setup, &at, version)?;
    let note = desktop::open_file(&at).err().map(|e| format!("Couldn't open it with Claude ({e}); double-click it."));
    Ok(Done {
        agent: Agent::ClaudeDesktop,
        op: Op::Connect,
        path: at,
        changed: true,
        backup: None,
        how: How::Mcpb,
        note,
    })
}

/// Claude Desktop as a running app (macOS, by bundle id, through
/// `osascript`/`open`, which ask Launch Services as `NSRunningApplication`
/// does; the workspace forbids the `unsafe` a direct AppKit call needs).
/// Asking it to quit needs the app's `NSAppleEventsUsageDescription` and
/// the user's Automation permission. Elsewhere it never counts as running.
pub mod desktop {
    use super::*;

    /// How long one `osascript` or `pgrep` may take.
    const ASK_TIMEOUT: Duration = Duration::from_secs(5);

    fn osascript(script: &str, timeout: Duration) -> Result<Output, String> {
        let mut cmd = Command::new("/usr/bin/osascript");
        cmd.args(["-e", script]);
        run(cmd, timeout)
    }

    /// Whether Claude Desktop is running. When `osascript` can't tell, its
    /// process (`pgrep -x Claude`) decides, and when that can't either it
    /// counts as running, so nothing is written under it.
    pub fn is_running() -> bool {
        if !cfg!(target_os = "macos") {
            return false;
        }
        let script = format!("application id \"{CLAUDE_DESKTOP_BUNDLE_ID}\" is running");
        if let Ok(o) = osascript(&script, ASK_TIMEOUT)
            && o.status.success()
        {
            match String::from_utf8_lossy(&o.stdout).trim() {
                "true" => return true,
                "false" => return false,
                _ => {}
            }
        }
        let mut cmd = Command::new("/usr/bin/pgrep");
        cmd.args(["-x", "Claude"]);
        // pgrep exits 1 for "no such process"; anything else is unknown.
        !matches!(run(cmd, ASK_TIMEOUT).map(|o| o.status.code()), Ok(Some(1)))
    }

    /// What a failed `tell … to quit` means, from `osascript`'s error output.
    pub fn quit_error(stderr: &str) -> Error {
        if stderr.contains("-1743") {
            Error::DesktopAutomationDenied
        } else if stderr.contains("-128") {
            Error::DesktopDidNotQuit
        } else {
            Error::DesktopQuitFailed(stderr.trim().to_owned())
        }
    }

    /// Ask it to quit (a chat in progress may make it ask the user), and
    /// wait up to `wait` in all for it to be gone.
    pub fn quit(wait: Duration) -> Result<(), Error> {
        if !is_running() {
            return Ok(());
        }
        let start = Instant::now();
        let script = format!("tell application id \"{CLAUDE_DESKTOP_BUNDLE_ID}\" to quit");
        match osascript(&script, wait) {
            Ok(o) if !o.status.success() => return Err(quit_error(&String::from_utf8_lossy(&o.stderr))),
            // Done, or still waiting on a question Claude asked: watch it.
            Ok(_) | Err(_) => {}
        }
        while start.elapsed() < wait {
            if !is_running() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        if is_running() { Err(Error::DesktopDidNotQuit) } else { Ok(()) }
    }

    /// Open it again (`open -b`).
    pub fn reopen() -> Result<(), String> {
        open(&["-b", CLAUDE_DESKTOP_BUNDLE_ID])
    }

    /// Open `file` with it (`open -b <id> <file>`).
    pub fn open_file(file: &Path) -> Result<(), String> {
        let file = file.to_str().ok_or("the path isn't UTF-8")?;
        open(&["-b", CLAUDE_DESKTOP_BUNDLE_ID, file])
    }

    fn open(args: &[&str]) -> Result<(), String> {
        if !cfg!(target_os = "macos") {
            return Err("only on macOS".into());
        }
        let mut cmd = Command::new("/usr/bin/open");
        cmd.args(args);
        let out = run(cmd, Duration::from_secs(15))?;
        if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_owned()) }
    }
}

#[cfg(test)]
mod tests;
