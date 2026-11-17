//! The command-line tool's link, made in a temporary folder.
#![cfg(all(unix, feature = "shell"))]

use std::path::{Path, PathBuf};
use std::time::Duration;

use scriptorium_connect::cli_install::{make_link, probe};

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("scriptorium-cli-install-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn makes_the_link_and_replaces_an_old_one() {
    let d = temp_dir("link");
    let bin = d.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let old = d.join("Old App/Contents/MacOS/scriptorium");
    let new = d.join("Scénario App/Contents/MacOS/scriptorium");
    let at = make_link(&bin, &old).unwrap();
    assert_eq!(at, bin.join("scriptorium"));
    assert_eq!(std::fs::read_link(&at).unwrap(), old);
    make_link(&bin, &new).unwrap();
    assert_eq!(std::fs::read_link(&at).unwrap(), new);
    // Nothing left behind but the link.
    let names: Vec<_> = std::fs::read_dir(&bin).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, ["scriptorium"]);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_missing_folder_fails() {
    assert!(make_link(Path::new("/nonexistent/scriptorium-test"), Path::new("/x")).is_err());
}

#[test]
fn probes_a_login_shell() {
    let sh = Path::new("/bin/sh");
    if !sh.exists() {
        return;
    }
    let path = probe(sh, Duration::from_secs(5)).expect("sh answers");
    assert!(!path.is_empty());
}

#[test]
fn a_slow_shell_times_out() {
    let d = temp_dir("slow");
    let shell = d.join("slow-shell");
    std::fs::write(&shell, "#!/bin/sh\nsleep 5\n").unwrap();
    let mut perms = std::fs::metadata(&shell).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&shell, perms).unwrap();
    let start = std::time::Instant::now();
    assert_eq!(probe(&shell, Duration::from_millis(300)), None);
    assert!(start.elapsed() < Duration::from_secs(3));
    std::fs::remove_dir_all(&d).unwrap();
}
