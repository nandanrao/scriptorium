//! `scriptorium version new` (plan step A5b): the command line around
//! `scriptorium_fsio::versions::version_new` (whose own tests cover the
//! re-run rules): arguments, `-o`, and the absolute path printed last.

use std::path::PathBuf;
use std::process::Command;

const SCRIPT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Scene Heading">
      <Text>EXT. FARM HOUSE - NIGHT</Text>
    </Paragraph>
    <Paragraph Type="Character">
      <Text>CESAR</Text>
    </Paragraph>
    <Paragraph Type="Dialogue">
      <Text>I am go to the town tomorrow.</Text>
    </Paragraph>
  </Content>
</FinalDraft>
"#;

struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("scriptorium-cli-version-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
        Self(dir.canonicalize().unwrap())
    }
    /// Run from inside the folder: (success, stdout, stderr).
    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_scriptorium")).current_dir(&self.0).args(args).output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn prints_the_absolute_path_last() {
    let f = Folder::new("path");
    let (ok, out, err) = f.run(&["version", "new", "film.fdx", "--lang", "es", "--from", "en"]);
    assert!(ok, "{err}");
    let last = out.lines().last().unwrap();
    assert_eq!(PathBuf::from(last), f.0.join("film.es.fdx"));
    assert!(f.0.join("film.es.links.txt").exists());
    assert!(out.contains("Next: scriptorium rows"), "{out}");
    // Again: already made, same path.
    let (ok, out, _) = f.run(&["version", "new", "film.fdx", "--lang", "es"]);
    assert!(ok);
    assert!(out.contains("already made"), "{out}");
    assert_eq!(PathBuf::from(out.lines().last().unwrap()), f.0.join("film.es.fdx"));
}

#[test]
fn arguments_are_checked() {
    let f = Folder::new("args");
    let (ok, _, err) = f.run(&["version", "new", "film.fdx"]);
    assert!(!ok && err.contains("--lang"), "{err}");
    let (ok, _, err) = f.run(&["version", "new", "film.fdx", "--lang"]);
    assert!(!ok && err.contains("needs a value"), "{err}");
    let (ok, _, err) = f.run(&["version", "new", "film.fdx", "--lang", "e\ns"]);
    assert!(!ok && err.contains("language code"), "{err}");
    let (ok, _, err) = f.run(&["version", "new", "film.fdx", "--lang", "es", "-o", "a\nb.fdx"]);
    assert!(!ok, "{err}");
    assert!(!f.0.join("film.scriptorium.lock").exists(), "no lock for bad arguments");
    let (ok, _, err) = f.run(&["version", "new", "film.fdx", "--lang", "es", "-o", "sub/x.fdx"]);
    assert!(!ok && err.contains("script's folder"), "{err}");
    assert!(!f.0.join("sub/x.fdx").exists());

    // A path to the script's own folder is fine.
    let (ok, out, err) = f.run(&["version", "new", "film.fdx", "--lang", "es", "-o", "./Película.fdx"]);
    assert!(ok, "{err}");
    assert_eq!(PathBuf::from(out.lines().last().unwrap()), f.0.join("Película.fdx"));
    assert!(f.0.join("Película.links.txt").exists());
}
