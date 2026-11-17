//! Scriptorium › "Install command-line tool…": put a `scriptorium` link on
//! the user's PATH pointing at the CLI inside the app, like VS Code's
//! "Install 'code' command in PATH". It runs only when the user chooses
//! it; nothing happens at launch.
//!
//! The pure part ([`plan`], [`message`], the text helpers) decides; the
//! shell part ([`install`], feature `shell`, Unix) looks at the disk, makes
//! the link, or asks for the admin password once (macOS) to make it in
//! `/usr/local/bin`.

use std::path::{Path, PathBuf};

use crate::{NAME, OURS_SUFFIX, shell_quote};

/// Where the admin step puts the link. It is in macOS's default
/// `/etc/paths`, so it works in any new Terminal window.
pub const ADMIN_DIR: &str = "/usr/local/bin";

/// What is already at `<dir>/scriptorium`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A symbolic link to this path (resolved against the link's folder).
    Link(PathBuf),
    /// A file, folder, or anything else that isn't a link.
    Other,
}

/// What the menu item will do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// The first `scriptorium` on the login PATH is already a link to this
    /// copy (the link's path).
    AlreadyOk(PathBuf),
    /// Make (or replace our own old) link in this folder; no password.
    Symlink(PathBuf),
    /// Make the link in [`ADMIN_DIR`] with the admin password.
    AdminSymlink,
    /// Another program's `scriptorium` is here; it is left alone.
    Conflict(PathBuf),
}

/// The CLI beside the app's executable (`Contents/MacOS/scriptorium` in
/// the bundle; `target/<profile>/scriptorium` in a build).
pub fn cli_beside(app_exe: &Path) -> PathBuf {
    app_exe.with_file_name(NAME)
}

/// Whether macOS is running the app from a random read-only copy (App
/// Translocation: opened straight from Downloads without moving it). A
/// link must never point there.
pub fn is_translocated(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == "AppTranslocation")
}

/// Whether the app must be moved before a link may point at it: it is
/// translocated, or runs from a mounted disk image (`/Volumes/…`) that
/// goes away when ejected.
pub fn needs_moving(path: &Path) -> bool {
    is_translocated(path) || path.starts_with("/Volumes")
}

/// Whether a link to `target` is ours: to this copy, or to some
/// Scriptorium.app's CLI (a copy since moved, or another copy).
fn is_ours(target: &Path, this: &Path) -> bool {
    target == this || target.to_str().is_some_and(|t| t.ends_with(OURS_SUFFIX))
}

/// The folders tried, in order, for a link without a password.
/// `/usr/local/bin` comes last: on an Intel Mac with Homebrew the user can
/// write it, and then it needs no password either.
pub fn candidates(home: &Path) -> Vec<PathBuf> {
    vec![PathBuf::from("/opt/homebrew/bin"), home.join(".local/bin"), home.join("bin"), PathBuf::from(ADMIN_DIR)]
}

/// Decide what to do.
///
/// - `this`: the CLI the link should point at.
/// - `login_path`: the folders on the user's login-shell PATH, in order.
/// - `writable(dir)`: whether the user can make a file in `dir`.
/// - `existing(path)`: what is at `path` now, if anything.
///
/// The first `scriptorium` on the PATH decides when there is one: ours and
/// to this copy is [`Plan::AlreadyOk`]; our own old link (to a moved app)
/// is replaced where it is; anything else is a [`Plan::Conflict`], since a
/// new link later on the PATH would be hidden by it. With none on the PATH,
/// the first [`candidates`] folder on the PATH that the user can write gets
/// the link, and otherwise [`ADMIN_DIR`] does, with the password.
pub fn plan(
    this: &Path,
    home: &Path,
    login_path: &[PathBuf],
    writable: impl Fn(&Path) -> bool,
    existing: impl Fn(&Path) -> Option<Target>,
) -> Plan {
    let on_path: Vec<&PathBuf> = login_path.iter().filter(|d| d.is_absolute()).collect();
    for dir in &on_path {
        let at = dir.join(NAME);
        // This copy's own folder is on the PATH (a build's target folder).
        if at == this {
            return Plan::AlreadyOk(at);
        }
        match existing(&at) {
            None => continue,
            Some(Target::Link(t)) if t == this => return Plan::AlreadyOk(at),
            Some(Target::Link(t)) if is_ours(&t, this) => {
                return if writable(dir) {
                    Plan::Symlink(dir.to_path_buf())
                } else if dir.as_path() == Path::new(ADMIN_DIR) {
                    Plan::AdminSymlink
                } else {
                    Plan::Conflict(at)
                };
            }
            Some(_) => return Plan::Conflict(at),
        }
    }
    if let Some(dir) = candidates(home).into_iter().find(|c| on_path.contains(&c) && writable(c)) {
        return Plan::Symlink(dir);
    }
    // Not on the PATH (unusual), but the admin step would overwrite it.
    let admin = Path::new(ADMIN_DIR).join(NAME);
    match existing(&admin) {
        Some(Target::Link(t)) if !is_ours(&t, this) => Plan::Conflict(admin),
        Some(Target::Other) => Plan::Conflict(admin),
        _ => Plan::AdminSymlink,
    }
}

/// The folders of a `PATH` value.
pub fn split_path(value: &str) -> Vec<PathBuf> {
    value.split(':').filter(|s| !s.is_empty()).map(PathBuf::from).collect()
}

/// Printed before the environment by the login-shell probe, so whatever
/// the user's startup files print is skipped.
pub const PROBE_MARK: &str = "__SCRIPTORIUM_LOGIN_PATH__";

/// The `-c` script for `$SHELL -l -c`: a marker line, then the
/// environment. `env` rather than `"$PATH"`, which fish joins with spaces.
pub fn probe_script() -> String {
    format!("printf '\\n%s\\n' {PROBE_MARK}; /usr/bin/env")
}

/// The login PATH from the probe's output: the first `PATH=` line after
/// the marker.
pub fn parse_probe(stdout: &str) -> Option<Vec<PathBuf>> {
    let after = &stdout[stdout.rfind(PROBE_MARK)? + PROBE_MARK.len()..];
    after.lines().find_map(|l| l.strip_prefix("PATH=")).map(split_path)
}

/// `ln`'s flags: replace, and don't follow an existing link to a folder
/// (`-h` on macOS, `-n` elsewhere).
pub const LN_FLAGS: &str = if cfg!(target_os = "macos") { "-sfh" } else { "-sfn" };

/// The shell command that makes the link in [`ADMIN_DIR`].
pub fn admin_command(this: &Path, sudo: bool) -> String {
    let s = if sudo { "sudo " } else { "" };
    format!("{s}mkdir -p {ADMIN_DIR} && {s}ln {LN_FLAGS} {} {ADMIN_DIR}/{NAME}", shell_quote(&this.to_string_lossy()))
}

/// The AppleScript for `osascript -e`: the admin command, run with the
/// password prompt.
pub fn admin_applescript(this: &Path) -> String {
    let cmd = admin_command(this, false).replace('\\', "\\\\").replace('"', "\\\"");
    format!("do shell script \"{cmd}\" with administrator privileges")
}

/// Whether `osascript`'s error output means the user cancelled the
/// password prompt (error -128).
pub fn is_cancelled(stderr: &str) -> bool {
    stderr.contains("(-128)") || stderr.contains("User canceled")
}

/// How the menu item ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Already a link to this copy (where).
    AlreadyOk(PathBuf),
    /// The link was made (where).
    Installed(PathBuf),
    /// Another program's `scriptorium` is there; left alone.
    Conflict(PathBuf),
    /// The user cancelled the password prompt; nothing changed.
    Cancelled,
    /// The app runs from a translocated copy or a disk image
    /// ([`needs_moving`]); nothing changed.
    Translocated,
    /// The CLI isn't beside the app.
    NoCli(PathBuf),
    /// The CLI's path isn't valid UTF-8, so no shell command can name it.
    NotUtf8,
    /// It needs the admin step, which failed or isn't available here: the
    /// user can run `command` in a terminal instead.
    Manual { why: String, command: String },
    /// Something else went wrong.
    Failed(String),
}

/// The line the app shows for an outcome.
pub fn message(outcome: &Outcome) -> String {
    const TRY: &str = "Open a new Terminal window and type scriptorium --help.";
    match outcome {
        Outcome::AlreadyOk(at) => format!("The command-line tool is already installed ({}). {TRY}", at.display()),
        Outcome::Installed(_) => format!("Installed. {TRY}"),
        Outcome::Conflict(at) => format!(
            "Didn't install the command-line tool: {} is another program's scriptorium command, so it was left alone.",
            at.display()
        ),
        Outcome::Cancelled => {
            "Didn't install the command-line tool: the password prompt was cancelled. Nothing was changed.".into()
        }
        Outcome::Translocated => {
            "Move Scriptorium to your Applications folder first, then choose Install command-line tool… again.".into()
        }
        Outcome::NoCli(at) => format!("Didn't install the command-line tool: {} is missing.", at.display()),
        Outcome::NotUtf8 => "Didn't install the command-line tool: Scriptorium's path isn't valid UTF-8.".into(),
        Outcome::Manual { why, command } => {
            format!("Didn't install the command-line tool ({why}). In a terminal, run: {command}")
        }
        Outcome::Failed(why) => format!("Couldn't install the command-line tool: {why}"),
    }
}

#[cfg(all(feature = "shell", unix))]
pub use shell::{install, login_path, make_link, probe};

#[cfg(all(feature = "shell", unix))]
mod shell {
    use std::io::{self, Read};
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;

    use super::*;

    /// How long the login shell may take to print its PATH.
    const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

    /// Run the whole menu item for the app whose executable is `app_exe`.
    pub fn install(app_exe: &Path) -> Outcome {
        if needs_moving(app_exe) {
            return Outcome::Translocated;
        }
        let this = cli_beside(app_exe);
        if this.to_str().is_none() {
            return Outcome::NotUtf8;
        }
        if !this.is_file() {
            return Outcome::NoCli(this);
        }
        let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from) else {
            return Outcome::Failed("HOME isn't set".into());
        };
        let path = login_path();
        match plan(&this, &home, &path, writable, existing) {
            Plan::AlreadyOk(at) => Outcome::AlreadyOk(at),
            Plan::Conflict(at) => Outcome::Conflict(at),
            Plan::Symlink(dir) => match make_link(&dir, &this) {
                Ok(at) => Outcome::Installed(at),
                Err(e) => Outcome::Failed(format!("couldn't make the link in {}: {e}", dir.display())),
            },
            Plan::AdminSymlink => admin(&this),
        }
    }

    /// The login shell's PATH (GUI apps get launchd's minimal one), or this
    /// process's if the shell doesn't answer in time.
    pub fn login_path() -> Vec<PathBuf> {
        let shell = std::env::var_os("SHELL").filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
        probe(Path::new(&shell), PROBE_TIMEOUT)
            .unwrap_or_else(|| std::env::var("PATH").map(|p| split_path(&p)).unwrap_or_default())
    }

    /// Ask `shell -l -c` for its PATH, giving up after `timeout`.
    pub fn probe(shell: &Path, timeout: Duration) -> Option<Vec<PathBuf>> {
        let mut child = Command::new(shell)
            .args(["-l", "-c", &probe_script()])
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // Its own process group, so a timeout kills whatever the
            // startup files started too.
            .process_group(0)
            .spawn()
            .ok()?;
        let mut out = child.stdout.take()?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut s = Vec::new();
            let _ = out.read_to_end(&mut s);
            let _ = tx.send(s);
        });
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(20)),
                _ => {
                    if let Ok(pid) = i32::try_from(child.id()) {
                        let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
        // Something the shell started in the background may keep the pipe
        // open; wait for the output only until the time is up.
        let bytes = rx.recv_timeout(timeout.saturating_sub(start.elapsed()).max(Duration::from_millis(100))).ok()?;
        parse_probe(&String::from_utf8_lossy(&bytes))
    }

    /// Whether the user can make a file in `dir` (tried, then removed).
    fn writable(dir: &Path) -> bool {
        let probe = dir.join(format!(".scriptorium-write-test-{}", std::process::id()));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
            Ok(_) => {
                let _ = std::fs::remove_file(&probe);
                true
            }
            Err(_) => false,
        }
    }

    fn existing(path: &Path) -> Option<Target> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        if !meta.file_type().is_symlink() {
            return Some(Target::Other);
        }
        let t = std::fs::read_link(path).ok()?;
        Some(Target::Link(match path.parent() {
            Some(dir) if t.is_relative() => dir.join(t),
            _ => t,
        }))
    }

    /// Make `<dir>/scriptorium` a link to `this`, replacing what is there
    /// in one step (a new link renamed over it). Returns the link's path.
    pub fn make_link(dir: &Path, this: &Path) -> io::Result<PathBuf> {
        let at = dir.join(NAME);
        let tmp = dir.join(format!(".{NAME}.tmp-{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        std::os::unix::fs::symlink(this, &tmp)?;
        if let Err(e) = std::fs::rename(&tmp, &at) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(at)
    }

    /// The link in /usr/local/bin with the admin password (macOS), or the
    /// command to run by hand.
    fn admin(this: &Path) -> Outcome {
        let manual = |why: String| Outcome::Manual { why, command: admin_command(this, true) };
        if !cfg!(target_os = "macos") {
            return manual(format!("it needs administrator rights to write {ADMIN_DIR}"));
        }
        let out = match Command::new("/usr/bin/osascript").arg("-e").arg(admin_applescript(this)).output() {
            Ok(o) => o,
            Err(e) => return manual(format!("couldn't ask for the password: {e}")),
        };
        let at = Path::new(ADMIN_DIR).join(NAME);
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            if is_cancelled(&err) {
                return Outcome::Cancelled;
            }
            return manual(format!("the admin step failed: {}", err.trim()));
        }
        match existing(&at) {
            Some(Target::Link(t)) if t == this => Outcome::Installed(at),
            _ => manual(format!("{} isn't a link to Scriptorium after the admin step", at.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    const THIS: &str = "/Applications/Scriptorium.app/Contents/MacOS/scriptorium";
    const HOME: &str = "/Users/me";

    struct Disk {
        writable: HashSet<PathBuf>,
        files: HashMap<PathBuf, Target>,
    }

    impl Disk {
        fn new() -> Self {
            Disk { writable: HashSet::new(), files: HashMap::new() }
        }
        fn writable(mut self, d: &str) -> Self {
            self.writable.insert(d.into());
            self
        }
        fn link(mut self, at: &str, to: &str) -> Self {
            self.files.insert(at.into(), Target::Link(to.into()));
            self
        }
        fn file(mut self, at: &str) -> Self {
            self.files.insert(at.into(), Target::Other);
            self
        }
        fn plan(&self, path: &str) -> Plan {
            plan(
                Path::new(THIS),
                Path::new(HOME),
                &split_path(path),
                |d| self.writable.contains(d),
                |p| self.files.get(p).cloned(),
            )
        }
    }

    const MAC: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

    #[test]
    fn homebrew_gets_the_link() {
        let d = Disk::new().writable("/opt/homebrew/bin").writable("/Users/me/.local/bin");
        let p = d.plan(&format!("/opt/homebrew/bin:/Users/me/.local/bin:{MAC}"));
        assert_eq!(p, Plan::Symlink("/opt/homebrew/bin".into()));
    }

    #[test]
    fn local_bin_on_path() {
        let d = Disk::new().writable("/Users/me/.local/bin");
        assert_eq!(d.plan(&format!("{MAC}:/Users/me/.local/bin")), Plan::Symlink("/Users/me/.local/bin".into()));
    }

    #[test]
    fn writable_but_not_on_path_is_skipped() {
        let d = Disk::new().writable("/opt/homebrew/bin").writable("/Users/me/bin");
        assert_eq!(d.plan(MAC), Plan::AdminSymlink);
    }

    #[test]
    fn neither_needs_the_password() {
        assert_eq!(Disk::new().plan(MAC), Plan::AdminSymlink);
    }

    #[test]
    fn writable_usr_local_bin_needs_no_password() {
        let d = Disk::new().writable("/usr/local/bin");
        assert_eq!(d.plan(MAC), Plan::Symlink("/usr/local/bin".into()));
    }

    #[test]
    fn a_conflict_is_named_and_left_alone() {
        let d = Disk::new().writable("/opt/homebrew/bin").file("/Users/me/.cargo/bin/scriptorium");
        let p = d.plan(&format!("/Users/me/.cargo/bin:/opt/homebrew/bin:{MAC}"));
        assert_eq!(p, Plan::Conflict("/Users/me/.cargo/bin/scriptorium".into()));
        let d = Disk::new().link("/usr/local/bin/scriptorium", "/somewhere/else/scriptorium");
        assert_eq!(d.plan(MAC), Plan::Conflict("/usr/local/bin/scriptorium".into()));
    }

    #[test]
    fn this_copys_own_folder_on_path_is_already_ok() {
        let d = Disk::new().writable("/opt/homebrew/bin").file(THIS);
        let p = d.plan(&format!("/Applications/Scriptorium.app/Contents/MacOS:/opt/homebrew/bin:{MAC}"));
        assert_eq!(p, Plan::AlreadyOk(THIS.into()));
    }

    #[test]
    fn already_ok() {
        let d = Disk::new().link("/usr/local/bin/scriptorium", THIS);
        assert_eq!(d.plan(MAC), Plan::AlreadyOk("/usr/local/bin/scriptorium".into()));
    }

    #[test]
    fn our_stale_link_is_replaced_where_it_is() {
        let old = "/Users/me/Downloads/Scriptorium.app/Contents/MacOS/scriptorium";
        let d = Disk::new().link("/usr/local/bin/scriptorium", old);
        assert_eq!(d.plan(MAC), Plan::AdminSymlink);
        let d = Disk::new().writable("/opt/homebrew/bin").link("/opt/homebrew/bin/scriptorium", old);
        assert_eq!(d.plan(&format!("/opt/homebrew/bin:{MAC}")), Plan::Symlink("/opt/homebrew/bin".into()));
        // Ours, but in a folder we can't write and the admin step doesn't touch.
        let d = Disk::new().link("/opt/local/bin/scriptorium", old);
        assert_eq!(d.plan(&format!("/opt/local/bin:{MAC}")), Plan::Conflict("/opt/local/bin/scriptorium".into()));
    }

    #[test]
    fn admin_target_off_path_but_taken_is_a_conflict() {
        let d = Disk::new().file("/usr/local/bin/scriptorium");
        assert_eq!(d.plan("/usr/bin:/bin"), Plan::Conflict("/usr/local/bin/scriptorium".into()));
    }

    #[test]
    fn relative_and_empty_path_entries_are_ignored() {
        let d = Disk::new().writable("bin").file("bin/scriptorium");
        assert_eq!(d.plan("bin::/usr/bin"), Plan::AdminSymlink);
    }

    #[test]
    fn probe_output_skips_startup_noise() {
        let out = format!(
            "Welcome!\nPATH=/fake\n\n{PROBE_MARK}\nHOME=/Users/me\nPATH=/opt/homebrew/bin:/usr/bin\nSHELL=/bin/zsh\n"
        );
        assert_eq!(parse_probe(&out), Some(vec!["/opt/homebrew/bin".into(), "/usr/bin".into()]));
        assert_eq!(parse_probe("PATH=/x\n"), None);
    }

    #[test]
    fn admin_script_quotes_spaces_and_quotes() {
        let this = Path::new("/Users/me/My \"Apps\"/Scriptorium.app/Contents/MacOS/scriptorium");
        assert_eq!(
            admin_command(this, true),
            format!(
                "sudo mkdir -p /usr/local/bin && sudo ln {LN_FLAGS} '/Users/me/My \"Apps\"/Scriptorium.app/Contents/MacOS/scriptorium' /usr/local/bin/scriptorium"
            )
        );
        assert_eq!(
            admin_applescript(this),
            format!(
                "do shell script \"mkdir -p /usr/local/bin && ln {LN_FLAGS} '/Users/me/My \\\"Apps\\\"/Scriptorium.app/Contents/MacOS/scriptorium' /usr/local/bin/scriptorium\" with administrator privileges"
            )
        );
    }

    #[test]
    fn translocation_and_cli_path() {
        assert!(is_translocated(Path::new(
            "/private/var/folders/x/T/AppTranslocation/ABC/d/Scriptorium.app/Contents/MacOS/scriptorium-app"
        )));
        assert!(!is_translocated(Path::new("/Applications/Scriptorium.app/Contents/MacOS/scriptorium-app")));
        assert!(needs_moving(Path::new("/Volumes/Scriptorium/Scriptorium.app/Contents/MacOS/scriptorium-app")));
        assert!(!needs_moving(Path::new("/Applications/Scriptorium.app/Contents/MacOS/scriptorium-app")));
        assert_eq!(
            cli_beside(Path::new("/A/Contents/MacOS/scriptorium-app")),
            Path::new("/A/Contents/MacOS/scriptorium")
        );
    }

    #[test]
    fn cancel_is_recognised() {
        assert!(is_cancelled("0:1: execution error: User canceled. (-128)"));
        assert!(!is_cancelled("0:1: execution error: Operation not permitted (1)"));
    }

    #[test]
    fn messages() {
        assert!(message(&Outcome::Installed("/x".into())).starts_with("Installed. Open a new Terminal window"));
        assert!(message(&Outcome::Cancelled).contains("Nothing was changed"));
        assert!(message(&Outcome::Conflict("/a/scriptorium".into())).contains("/a/scriptorium"));
        assert!(message(&Outcome::Translocated).contains("Applications"));
        assert!(!message(&Outcome::Installed("/x".into())).contains('`'));
    }
}
