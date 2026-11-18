//! `scriptorium apps <script> [--json]`: the list, and exit 1 on problems.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("scriptorium-apps-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("film.fdx"),
            "<?xml version=\"1.0\"?>\n<FinalDraft Version=\"5\">\n  <Content>\n  </Content>\n</FinalDraft>\n",
        )
        .unwrap();
        Self(dir)
    }

    fn app(&self, name: &str, manifest: Option<&str>, index: bool) {
        let dir = self.0.join("apps").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(m) = manifest {
            std::fs::write(dir.join("app.json"), m).unwrap();
        }
        if index {
            std::fs::write(dir.join("index.html"), "<html></html>").unwrap();
        }
    }

    /// (exit code, stdout, stderr).
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_scriptorium")).current_dir(&self.0).args(args).output().unwrap();
        (
            out.status.code().unwrap(),
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

const GOOD: &str = r#"{"scriptorium": 1, "title": "Scene list", "description": "Every scene."}"#;

#[test]
fn lists_the_examples_and_exits_0() {
    let f = Folder::new("good");
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../apps/examples");
    for name in ["scene-list", "characters-by-scene"] {
        let to = f.0.join("apps").join(name);
        std::fs::create_dir_all(&to).unwrap();
        for e in std::fs::read_dir(examples.join(name)).unwrap() {
            let e = e.unwrap();
            std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
        }
    }
    let (code, out, _) = f.run(&["apps", "film.fdx"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("2 apps in "), "{out}");
    assert!(out.contains("- scene-list:") && out.contains("- characters-by-scene:"), "{out}");
    let (code, out, _) = f.run(&["apps", "film.fdx", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2);
    assert_eq!(v[0]["ok"], true);
    assert!(Path::new(v[0]["folder"].as_str().unwrap()).is_absolute());
    let line = out_folder_line(&f.run(&["apps", "film.fdx"]).1);
    assert!(Path::new(&line).is_absolute(), "{line}");
}

#[test]
fn a_broken_manifest_exits_1_with_its_problem() {
    let f = Folder::new("broken");
    f.app("good", Some(GOOD), true);
    f.app("broken", Some("{\"description\": \"x\"}"), false);
    let (code, out, err) = f.run(&["apps", "film.fdx"]);
    assert_eq!(code, 1, "{out}{err}");
    assert!(out.contains("- good: Scene list") && out.contains("- broken") && out.contains("problem:"), "{out}");
    assert!(err.contains("broken"), "{err}");
    let (code, out, _) = f.run(&["apps", "film.fdx", "--json"]);
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let broken = v.as_array().unwrap().iter().find(|a| a["name"] == "broken").unwrap();
    assert_eq!(broken["ok"], false);
    assert!(!broken["problems"].as_array().unwrap().is_empty());
}

#[test]
fn no_apps_folder() {
    let f = Folder::new("none");
    let (code, out, _) = f.run(&["apps", "film.fdx"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("No apps in ") && out.trim_end().ends_with("/apps/."), "{out}");
    let (code, out, _) = f.run(&["apps", "film.fdx", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "[]");
}

#[test]
fn usage_and_missing_script() {
    let f = Folder::new("usage");
    assert_eq!(f.run(&["apps"]).0, 2);
    assert_eq!(f.run(&["apps", "nope.fdx"]).0, 1);
}

fn out_folder_line(text: &str) -> String {
    text.lines().find_map(|l| l.trim().strip_prefix("folder ")).unwrap().to_string()
}
