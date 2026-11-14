//! The `scriptorium` binary on a small script in a temporary folder: review
//! files attached to the session, used without naming them again.

use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Scene Heading">
      <Text>EXT. FARM HOUSE - NIGHT</Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text>Laura smiles.</Text>
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

const REVIEW_A: &str = "SCRIPT REVIEW v1
Script: film.fdx

---
BEFORE: I am go to the town tomorrow.
AFTER: I am going to town tomorrow.
WHY: verb form
";

const REVIEW_B: &str = "SCRIPT REVIEW v1
Script: film.fdx

---
BEFORE: Laura smiles.
AFTER: Laura grins.
";

/// A fresh folder holding `film.fdx` and two review files; removed on drop.
struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("scriptorium-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("notes")).unwrap();
        std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
        std::fs::write(dir.join("a.review.txt"), REVIEW_A).unwrap();
        std::fs::write(dir.join("notes/b.review.txt"), REVIEW_B).unwrap();
        Self(dir)
    }

    fn run(&self, args: &[&str]) -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_scriptorium")).current_dir(&self.0).args(args).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        (out.status.success(), text)
    }

    fn ok(&self, args: &[&str]) -> String {
        let (ok, text) = self.run(args);
        assert!(ok, "{args:?} failed: {text}");
        text
    }

    fn path(&self, p: &str) -> PathBuf {
        self.0.join(p)
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn session(dir: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join("film.scriptorium.json")).unwrap()).unwrap()
}

#[test]
fn attached_reviews_are_used_without_naming_them() {
    let f = Folder::new("attach");
    let (ok, text) = f.run(&["suggestions", "film.fdx"]);
    assert!(!ok && text.contains("no review files attached"), "{text}");

    assert!(f.ok(&["reviews", "film.fdx", "add", "a.review.txt"]).contains("attached a.review.txt"));
    let list = f.ok(&["suggestions", "film.fdx"]);
    assert!(list.contains("#1") && list.contains("I am going to town tomorrow."), "{list}");
    f.ok(&["decide", "film.fdx", "1", "accept"]);
    assert!(f.ok(&["reviews", "film.fdx"]).contains("a.review.txt  1 of 1 decided"));

    // A second review, outside the folder's top level: stored relative.
    let b = f.path("notes/b.review.txt");
    f.ok(&["reviews", "film.fdx", "add", b.to_str().unwrap()]);
    let s = session(&f.0);
    assert_eq!(s["version"], 2);
    let paths: Vec<&str> = s["reviews"].as_array().unwrap().iter().map(|r| r["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["a.review.txt", "notes/b.review.txt"]);

    // Two attached: a command on one review has to say which.
    let (ok, text) = f.run(&["suggestions", "film.fdx"]);
    assert!(!ok && text.contains("--review"), "{text}");
    f.ok(&["decide", "film.fdx", "--review", "b.review", "1", "keep"]);
    assert!(f.ok(&["reviews", "film.fdx"]).contains("notes/b.review.txt  1 of 1 decided"));

    // Undo needs no review files either; the last step was b's decision.
    f.ok(&["undo", "film.fdx"]);
    assert!(f.ok(&["reviews", "film.fdx"]).contains("notes/b.review.txt  0 of 1 decided"));

    // Detaching keeps the text; the decision on a stays in the written script.
    f.ok(&["reviews", "film.fdx", "remove", "a.review.txt"]);
    assert!(!f.ok(&["reviews", "film.fdx"]).contains("a.review.txt  "));
    let out = f.ok(&["apply", "film.fdx"]);
    assert!(out.contains("1 paragraphs changed"), "{out}");
    let written = std::fs::read_to_string(f.path("film (scriptorium).fdx")).unwrap();
    assert!(written.contains("I am going to town tomorrow."));
}

#[test]
fn a_review_named_on_a_saving_command_is_attached() {
    let f = Folder::new("named");
    f.ok(&["decide", "film.fdx", "a.review.txt", "1", "keep"]);
    assert!(f.ok(&["reviews", "film.fdx"]).contains("a.review.txt  1 of 1 decided"));
    // Naming it again doesn't attach it twice.
    f.ok(&["decide", "film.fdx", "a.review.txt", "1", "accept"]);
    assert_eq!(session(&f.0)["reviews"].as_array().unwrap().len(), 1);
}

#[test]
fn apply_attaches_the_reviews_it_is_given() {
    let f = Folder::new("apply");
    f.ok(&["apply", "film.fdx", "a.review.txt", "--accept-all"]);
    let written = std::fs::read_to_string(f.path("film (scriptorium).fdx")).unwrap();
    assert!(written.contains("I am going to town tomorrow."));
    // Attached, but --accept-all only shaped that output: nothing decided.
    assert!(f.ok(&["reviews", "film.fdx"]).contains("a.review.txt  0 of 1 decided"));
}
