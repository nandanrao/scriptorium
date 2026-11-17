//! `scriptorium connect` end to end, in a temporary home folder: the real
//! binary writes Codex's settings, checks itself with `--version`, and
//! takes them out again.

use std::path::PathBuf;
use std::process::Command;

fn temp_home(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("scriptorium-cli-connect-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn scriptorium(home: &PathBuf, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_scriptorium")).args(args).env("HOME", home).output().unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into())
}

#[test]
fn version() {
    let (ok, out, _) = scriptorium(&temp_home("version"), &["--version"]);
    assert!(ok);
    assert_eq!(out, format!("scriptorium {}\n", env!("CARGO_PKG_VERSION")));
}

#[test]
fn connects_codex_and_disconnects() {
    let home = temp_home("codex");
    let config = home.join(".codex/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "# my settings\nmodel = \"o3\"\n").unwrap();

    let (ok, out, err) = scriptorium(&home, &["connect", "codex"]);
    assert!(ok, "{out}{err}");
    assert!(out.contains("Connected Codex"), "{out}");
    assert!(err.contains("scriptorium-backup"), "{err}");
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.starts_with("# my settings\nmodel = \"o3\"\n"));
    assert!(text.contains("[mcp_servers.scriptorium]"));
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_scriptorium")).unwrap();
    assert!(text.contains(exe.to_str().unwrap()), "{text}");
    assert_eq!(
        std::fs::read_to_string(home.join(".codex/config.toml.scriptorium-backup")).unwrap(),
        "# my settings\nmodel = \"o3\"\n"
    );

    let (ok, out, _) = scriptorium(&home, &["connect", "--status"]);
    assert!(ok);
    assert!(out.contains("Codex: Connected"), "{out}");

    let (ok, out, _) = scriptorium(&home, &["connect", "codex"]);
    assert!(ok && out.contains("already connected"), "{out}");

    let (ok, out, _) = scriptorium(&home, &["connect", "codex", "--remove"]);
    assert!(ok && out.contains("Disconnected Codex"), "{out}");
    assert!(!std::fs::read_to_string(&config).unwrap().contains("scriptorium"));
}
