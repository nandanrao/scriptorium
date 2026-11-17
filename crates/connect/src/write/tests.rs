//! Writing configs in temporary home folders.

use std::os::unix::fs::PermissionsExt;

use super::*;

fn temp_home(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("scriptorium-connect-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

const THIS: &str = "/Applications/My Apps/Scriptorium.app/Contents/MacOS/scriptorium";

fn setup(home: &Path) -> Setup {
    Setup { desktop_running: || false, ..Setup::new(home, THIS) }
}

fn ours() -> State {
    State::Ours { command: THIS.into() }
}

#[test]
fn connects_each_agent_in_an_empty_home() {
    let home = temp_home("empty");
    let s = setup(&home);
    for agent in Agent::ALL {
        let done = apply(&s, agent, Op::Connect).unwrap();
        assert!(done.changed, "{agent}");
        assert_eq!(done.how, How::File);
        assert_eq!(done.backup, None, "nothing to back up");
        assert_eq!(status(&s, agent).state, ours(), "{agent}");
        assert_eq!(status(&s, agent).standing, Standing::Connected);
        // New files are the owner's only.
        assert_eq!(fs::metadata(&done.path).unwrap().permissions().mode() & 0o777, 0o600);
        // Again: nothing to do.
        let again = apply(&s, agent, Op::Connect).unwrap();
        assert!(!again.changed);
        assert!(again.message().contains("already connected"));
    }
    assert!(home.join(".codex/config.toml").is_file(), "~/.codex was made");
    for agent in Agent::ALL {
        assert!(apply(&s, agent, Op::Disconnect).unwrap().changed);
        assert_eq!(status(&s, agent).state, State::Missing);
        assert!(!apply(&s, agent, Op::Disconnect).unwrap().changed);
    }
}

#[test]
fn backs_up_and_keeps_other_servers() {
    let home = temp_home("backup");
    let s = setup(&home);
    let path = Agent::ClaudeDesktop.config_path(&home);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let before = "{\n  \"preferences\": {\"x\": 1},\n  \"mcpServers\": {\"other\": {\"command\": \"npx\"}}\n}\n";
    fs::write(&path, before).unwrap();
    let done = apply(&s, Agent::ClaudeDesktop, Op::Connect).unwrap();
    let backup = done.backup.unwrap();
    assert_eq!(backup, path.with_file_name("claude_desktop_config.json.scriptorium-backup"));
    assert_eq!(fs::read_to_string(&backup).unwrap(), before);
    let after: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(after["preferences"]["x"], 1);
    assert_eq!(after["mcpServers"]["other"]["command"], "npx");
    assert_eq!(after["mcpServers"]["scriptorium"]["command"], THIS);
    // The first copy stays; the backup follows the last write.
    assert_eq!(fs::read_to_string(original_path(&path)).unwrap(), before);
    let connected = fs::read_to_string(&path).unwrap();
    apply(&s, Agent::ClaudeDesktop, Op::Disconnect).unwrap();
    assert_eq!(fs::read_to_string(&backup).unwrap(), connected);
    assert_eq!(fs::read_to_string(original_path(&path)).unwrap(), before);
}

#[test]
fn another_users_file_is_refused() {
    let passwd = Path::new("/etc/passwd");
    if !passwd.exists() || euid() == 0 {
        return;
    }
    assert_eq!(check_owner(passwd, euid()), Err(Error::NotOwner(passwd.into())));
    assert_eq!(check_owner(Path::new("/no/such/file"), euid()), Ok(()));
}

#[test]
fn quit_errors() {
    let denied = "0:44: execution error: Not authorized to send Apple events to Claude. (-1743)";
    assert_eq!(desktop::quit_error(denied), Error::DesktopAutomationDenied);
    assert!(Error::DesktopAutomationDenied.to_string().contains("Automation"));
    assert_eq!(desktop::quit_error("execution error: User canceled. (-128)"), Error::DesktopDidNotQuit);
    assert!(matches!(desktop::quit_error("something else (-600)"), Error::DesktopQuitFailed(_)));
}

#[test]
fn codex_home_and_claude_config_dir() {
    let home = temp_home("env-dirs");
    let s = Setup {
        codex_home: Some(home.join("codex-elsewhere")),
        claude_config_dir: Some(home.join("claude-elsewhere")),
        ..setup(&home)
    };
    assert_eq!(s.config_path(Agent::Codex), home.join("codex-elsewhere/config.toml"));
    assert_eq!(s.config_path(Agent::ClaudeCode), home.join("claude-elsewhere/.claude.json"));
    apply(&s, Agent::Codex, Op::Connect).unwrap();
    assert!(home.join("codex-elsewhere/config.toml").is_file());
    assert!(!home.join(".codex").exists());
}

#[test]
fn a_file_changed_meanwhile_is_not_overwritten() {
    let home = temp_home("changed");
    let s = setup(&home);
    let path = home.join(".claude.json");
    fs::write(&path, "{\"numStartups\": 1}\n").unwrap();
    let theirs = "{\"numStartups\": 2}\n";
    let err = apply_with(&s, Agent::ClaudeCode, Op::Connect, || fs::write(&path, theirs).unwrap()).unwrap_err();
    assert!(matches!(err, Error::Changed { agent: Agent::ClaudeCode, .. }));
    assert!(err.to_string().contains("Quit Claude Code first"));
    assert_eq!(fs::read_to_string(&path).unwrap(), theirs);
    // No copies are kept of a write that didn't happen.
    assert!(!backup_path(&path).exists() && !original_path(&path).exists());
}

#[test]
fn writes_through_a_link() {
    let home = temp_home("link");
    let s = setup(&home);
    let real = home.join("dotfiles/config.toml");
    fs::create_dir_all(real.parent().unwrap()).unwrap();
    fs::write(&real, "# mine\nmodel = \"o3\"\n").unwrap();
    fs::create_dir_all(home.join(".codex")).unwrap();
    let link = home.join(".codex/config.toml");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    apply(&s, Agent::Codex, Op::Connect).unwrap();
    assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "still a link");
    let text = fs::read_to_string(&real).unwrap();
    assert!(text.starts_with("# mine\nmodel = \"o3\"\n"));
    assert!(text.contains("[mcp_servers.scriptorium]"));
}

#[test]
fn read_only_is_said() {
    let home = temp_home("readonly");
    let s = setup(&home);
    let path = home.join(".claude.json");
    fs::write(&path, "{}").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
    let err = apply(&s, Agent::ClaudeCode, Op::Connect).unwrap_err();
    assert_eq!(err, Error::ReadOnly(path.clone()));
    assert_eq!(fs::read_to_string(&path).unwrap(), "{}");
}

#[test]
fn broken_and_theirs_are_left_alone() {
    let home = temp_home("refused");
    let s = setup(&home);
    let path = home.join(".claude.json");
    fs::write(&path, "{not json").unwrap();
    assert!(matches!(apply(&s, Agent::ClaudeCode, Op::Connect), Err(Error::Refused(Refused::Broken(_)))));
    assert!(matches!(status(&s, Agent::ClaudeCode).standing, Standing::Unreadable(_)));
    let theirs = "{\"mcpServers\": {\"scriptorium\": {\"command\": \"npx\", \"args\": [\"scriptorium-mcp\"]}}}";
    fs::write(&path, theirs).unwrap();
    assert_eq!(apply(&s, Agent::ClaudeCode, Op::Connect), Err(Error::Refused(Refused::Theirs)));
    assert_eq!(fs::read_to_string(&path).unwrap(), theirs);
    assert!(!backup_path(&path).exists());
}

#[test]
fn claude_desktop_is_not_written_while_it_runs() {
    let home = temp_home("running");
    let mut s = setup(&home);
    s.desktop_running = || true;
    assert_eq!(apply(&s, Agent::ClaudeDesktop, Op::Connect), Err(Error::DesktopRunning));
    assert!(!Agent::ClaudeDesktop.config_path(&home).exists());
    // Already connected needs no write, so no quitting either.
    s.desktop_running = || false;
    apply(&s, Agent::ClaudeDesktop, Op::Connect).unwrap();
    s.desktop_running = || true;
    assert!(!apply(&s, Agent::ClaudeDesktop, Op::Connect).unwrap().changed);
    assert!(!apply_restarting_desktop(&s, Op::Connect, Duration::ZERO).unwrap().changed);
}

#[test]
fn another_copy_is_updated_in_place() {
    let home = temp_home("other-copy");
    let s = setup(&home);
    let old = Setup { this: "/Users/me/Downloads/Scriptorium.app/Contents/MacOS/scriptorium".into(), ..setup(&home) };
    apply(&old, Agent::Codex, Op::Connect).unwrap();
    assert!(matches!(status(&s, Agent::Codex).standing, Standing::OtherCopy(_)));
    assert!(apply(&s, Agent::Codex, Op::Connect).unwrap().changed);
    assert_eq!(status(&s, Agent::Codex).standing, Standing::Connected);
}

#[test]
fn translocated_writes_nothing() {
    let home = temp_home("translocated");
    let s = Setup {
        this: "/private/var/folders/x/T/AppTranslocation/A/d/Scriptorium.app/Contents/MacOS/scriptorium".into(),
        ..setup(&home)
    };
    assert_eq!(apply(&s, Agent::Codex, Op::Connect), Err(Error::Translocated));
    assert!(!home.join(".codex").exists());
}

/// A fake `claude` that logs its arguments and writes what `claude mcp`
/// would to `$HOME/.claude.json`.
fn fake_claude(home: &Path) -> PathBuf {
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    fs::write(
        &claude,
        r#"#!/bin/sh
echo "$*" >> "$HOME/claude.log"
case "$2" in
  add) printf '{"mcpServers":{"scriptorium":{"type":"stdio","command":"%s","args":["mcp"],"env":{}}}}' "$9" > "$HOME/.claude.json" ;;
  remove) printf '{"mcpServers":{}}' > "$HOME/.claude.json" ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).unwrap();
    claude
}

#[test]
fn claude_code_goes_through_its_command() {
    let home = temp_home("claude-cli");
    let claude = fake_claude(&home);
    assert_eq!(find_claude(&[home.join("bin")], &home), Some(claude.clone()));
    let s = Setup { claude: Some(claude), ..setup(&home) };
    let log = || fs::read_to_string(home.join("claude.log")).unwrap_or_default();

    let done = apply(&s, Agent::ClaudeCode, Op::Connect).unwrap();
    assert_eq!((done.how, done.changed), (How::ClaudeCli, true));
    assert_eq!(log(), format!("mcp add --scope user --transport stdio scriptorium -- {THIS} mcp\n"));
    assert!(!apply(&s, Agent::ClaudeCode, Op::Connect).unwrap().changed, "already there: not run again");

    // A stale entry of ours: removed, then added.
    let old = "/Old/Scriptorium.app/Contents/MacOS/scriptorium";
    fs::write(home.join(".claude.json"), format!("{{\"mcpServers\":{{\"scriptorium\":{{\"command\":\"{old}\"}}}}}}"))
        .unwrap();
    fs::remove_file(home.join("claude.log")).unwrap();
    let done = apply(&s, Agent::ClaudeCode, Op::Connect).unwrap();
    assert!(done.backup.is_some());
    assert!(log().starts_with("mcp remove --scope user scriptorium\nmcp add "));

    fs::remove_file(home.join("claude.log")).unwrap();
    assert!(apply(&s, Agent::ClaudeCode, Op::Disconnect).unwrap().changed);
    assert_eq!(log(), "mcp remove --scope user scriptorium\n");
    assert_eq!(status(&s, Agent::ClaudeCode).state, State::Missing);
}

#[test]
fn claude_add_failing_after_remove_says_so() {
    let home = temp_home("claude-half");
    let claude = home.join("claude");
    fs::write(
        &claude,
        "#!/bin/sh\ncase \"$2\" in remove) printf '{}' > \"$HOME/.claude.json\" ;; add) echo 'offline' >&2; exit 1 ;; esac\n",
    )
    .unwrap();
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        home.join(".claude.json"),
        "{\"mcpServers\":{\"scriptorium\":{\"command\":\"/Old/Scriptorium.app/Contents/MacOS/scriptorium\"}}}",
    )
    .unwrap();
    let s = Setup { claude: Some(claude), ..setup(&home) };
    let err = apply(&s, Agent::ClaudeCode, Op::Connect).unwrap_err();
    assert!(matches!(err, Error::ClaudeHalfDone(_)), "{err:?}");
    assert!(err.to_string().contains("The old entry was removed; run connect again"), "{err}");
}

#[test]
fn claude_command_failure_is_reported() {
    let home = temp_home("claude-fails");
    let claude = home.join("claude");
    fs::write(&claude, "#!/bin/sh\necho 'no such scope' >&2\nexit 1\n").unwrap();
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).unwrap();
    let s = Setup { claude: Some(claude), ..setup(&home) };
    let err = apply(&s, Agent::ClaudeCode, Op::Connect).unwrap_err();
    assert!(err.to_string().contains("no such scope"), "{err}");
}

#[test]
fn claude_candidates_in_order() {
    let home = Path::new("/Users/me");
    let c = claude_candidates(&["/opt/homebrew/bin".into(), "rel".into(), "/usr/bin".into()], home);
    let want: Vec<PathBuf> = ["/opt/homebrew/bin", "/usr/bin", "/Users/me/.local/bin", "/Users/me/.claude/local"]
        .iter()
        .map(|d| Path::new(d).join("claude"))
        .collect();
    assert_eq!(c, want);
}

#[test]
fn connector_check() {
    let home = temp_home("check");
    let good = home.join("good");
    // Run from `/`, as agents start it.
    fs::write(&good, "#!/bin/sh\n[ \"$(pwd)\" = / ] && echo \"scriptorium 0.6.0\"\n").unwrap();
    fs::set_permissions(&good, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(check_connector(&good, &home), Ok("scriptorium 0.6.0".into()));
    let bad = home.join("bad");
    fs::write(&bad, "#!/bin/sh\necho 'Killed: 9' >&2\nexit 137\n").unwrap();
    fs::set_permissions(&bad, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(check_connector(&bad, &home).unwrap_err().contains("Killed: 9"));
    assert!(check_connector(&home.join("missing"), &home).unwrap_err().contains("can't be started"));
}

#[test]
fn mcpb_is_written() {
    let home = temp_home("mcpb");
    let s = setup(&home);
    let at = mcpb_path(&home);
    write_mcpb(&s, &at, "0.6.0").unwrap();
    let bytes = fs::read(&at).unwrap();
    assert_eq!(&bytes[..4], b"PK\x03\x04");
    assert!(String::from_utf8_lossy(&bytes).contains(THIS));
}
