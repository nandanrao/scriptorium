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

#[test]
fn a_given_file_refused_as_a_duplicate_is_an_error() {
    let f = Folder::new("dupid");
    f.ok(&["reviews", "film.fdx", "add", "a.review.txt"]);
    let pinned = std::fs::read_to_string(f.path("a.review.txt")).unwrap();
    assert!(pinned.contains("\nId: "), "{pinned}");
    // A copy with one AFTER edited shares the Id but isn't the same file.
    std::fs::write(f.path("copy.review.txt"), pinned.replace("I am going to town", "I'm off to town")).unwrap();
    for args in [
        &["decide", "film.fdx", "copy.review.txt", "1", "accept"][..],
        &["suggestions", "film.fdx", "copy.review.txt"],
        &["reviews", "film.fdx", "add", "copy.review.txt"],
    ] {
        let (ok, text) = f.run(args);
        assert!(!ok && text.contains("share `Id:"), "{args:?}: {text}");
    }
    assert!(f.ok(&["reviews", "film.fdx"]).contains("a.review.txt  0 of 1 decided"));
    assert_eq!(session(&f.0)["reviews"].as_array().unwrap().len(), 1);
}

#[test]
fn naming_an_attached_file_again_pins_no_other() {
    let f = Folder::new("repin");
    f.ok(&["reviews", "film.fdx", "add", "a.review.txt", "notes/b.review.txt"]);
    // b back to its header-less text (same hash id, so still the same review).
    std::fs::write(f.path("notes/b.review.txt"), REVIEW_B).unwrap();
    f.ok(&["decide", "film.fdx", "a.review.txt", "1", "keep"]);
    assert_eq!(std::fs::read_to_string(f.path("notes/b.review.txt")).unwrap(), REVIEW_B);
}

#[test]
fn new_review_files_get_distinct_ids() {
    let f = Folder::new("newid");
    let id = |name: &str| {
        f.ok(&["suggest", name, "film.fdx", "--para", "1", "--after", "Laura grins."]);
        let text = std::fs::read_to_string(f.path(name)).unwrap();
        text.lines().find_map(|l| l.strip_prefix("Id: ").map(str::to_string)).expect("Id line")
    };
    assert_ne!(id("x.review.txt"), id("y.review.txt"));
}

#[test]
fn suggest_matches_and_numbers_against_the_live_document() {
    let f = Folder::new("suggestlive");
    f.ok(&["reviews", "film.fdx", "add", "a.review.txt"]);
    f.ok(&["edit", "film.fdx", "1", "Laura grins."]);
    // Only the edited (live) text is there to find.
    let out = f.ok(&["suggest", "a.review.txt", "film.fdx", "--before", "Laura grins.", "--after", "Laura beams."]);
    assert!(out.contains("#1 (¶1)"), "{out}");
    let text = std::fs::read_to_string(f.path("a.review.txt")).unwrap();
    assert!(text.contains("BEFORE: Laura grins.\nPARA: 1\n"), "{text}");
    // Found again on the next open, and numbered the same way.
    let list = f.ok(&["suggestions", "film.fdx"]);
    assert!(list.contains("Laura beams."), "{list}");
    // The attached file was written through the session: no "differs" note.
    let (_, again) = f.run(&["suggestions", "film.fdx"]);
    assert!(!again.contains("differ"), "{again}");
    // A second suggestion on the same line isn't written.
    let (ok, why) = f.run(&["suggest", "a.review.txt", "film.fdx", "--para", "1", "--after", "Laura laughs."]);
    assert!(!ok && why.contains("already covers ¶1"), "{why}");
}

/// Every file under `dir` with its contents, to show nothing was written.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let modified = std::fs::metadata(&p).unwrap().modified().unwrap();
                out.push((p.clone(), std::fs::read(&p).unwrap(), modified));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn tool_subcommand() {
    let f = Folder::new("tool");
    let scene = f.ok(&["tool", "film.fdx", "read_scene", r#"{"scene": 1}"#]);
    assert!(scene.contains("SCENE 1 of 1") && scene.contains("¶1 Laura smiles."), "{scene}");

    // No review attached and none named: the CLI has no review of its own.
    let args = r#"{"paragraph_id": 1, "options": ["Laura grins."], "why": "livelier"}"#;
    let (ok, text) = f.run(&["tool", "film.fdx", "suggest_change", args]);
    assert!(!ok && text.contains("give --review-file"), "{text}");
    assert!(!f.path("film.scriptorium.json").exists());

    // Into a new review file: created with an Id, attached.
    let out = f.ok(&["tool", "film.fdx", "suggest_change", args, "--review-file", "new.review.txt"]);
    assert!(out.contains("#1"), "{out}");
    let written = std::fs::read_to_string(f.path("new.review.txt")).unwrap();
    assert!(written.contains("\nId: ") && written.contains("BEFORE: Laura smiles.\nPARA: 1\n"), "{written}");
    let s = session(&f.0);
    assert_eq!(s["reviews"][0]["path"], "new.review.txt");

    // The only attached review is the default.
    let list = f.ok(&["tool", "film.fdx", "list_suggestions"]);
    let rows: serde_json::Value = serde_json::from_str(&list).unwrap();
    assert_eq!(rows[0]["options"][0], "Laura grins.", "{list}");
    assert_eq!(rows[0]["entry"], 0, "{list}");

    // A second one into the same file, named again: no duplicate attach.
    let args = r#"{"paragraph_id": 3, "options": ["I'm going to town tomorrow."]}"#;
    f.ok(&["tool", "film.fdx", "suggest_change", args, "--review-file", "new.review.txt"]);
    assert_eq!(session(&f.0)["reviews"].as_array().unwrap().len(), 1);
    assert!(f.ok(&["reviews", "film.fdx"]).contains("new.review.txt  0 of 2 decided"));

    // An existing header-less file is attached with its Id pinned.
    let args = r#"{"paragraph_id": 3, "options": ["I go to town tomorrow."]}"#;
    let out = f.ok(&["tool", "film.fdx", "suggest_change", args, "--review-file", "notes/b.review.txt"]);
    assert!(out.contains("#2 (e1, ¶3)"), "{out}");
    let b = std::fs::read_to_string(f.path("notes/b.review.txt")).unwrap();
    assert!(b.contains("\nId: ") && b.contains("BEFORE: Laura smiles.") && b.contains("PARA: 3"), "{b}");
    assert_eq!(session(&f.0)["reviews"].as_array().unwrap().len(), 2);

    let (ok, text) = f.run(&["tool", "film.fdx", "no_such_tool"]);
    assert!(!ok && text.contains("unknown tool"), "{text}");
    let (ok, text) = f.run(&["tool", "film.fdx", "read_scene", "{not json"]);
    assert!(!ok && text.contains("JSON"), "{text}");
}

#[test]
fn decide_by_entry_ref() {
    let f = Folder::new("entryref");
    f.ok(&["reviews", "film.fdx", "add", "a.review.txt"]);
    let out = f.ok(&["decide", "film.fdx", "e0", "accept"]);
    assert!(out.contains("#1 accepted 1: I am going to town tomorrow."), "{out}");
    let (ok, text) = f.run(&["decide", "film.fdx", "e5", "keep"]);
    assert!(!ok && text.contains("no suggestion e5"), "{text}");
}

#[test]
fn atomic_write_leaves_no_temp() {
    let f = Folder::new("atomic");
    f.ok(&["reviews", "film.fdx", "add", "a.review.txt", "notes/b.review.txt"]);
    f.ok(&["decide", "film.fdx", "--review", "a.review", "1", "accept"]);
    f.ok(&["suggest", "c.review.txt", "film.fdx", "--para", "1", "--after", "Laura beams."]);
    f.ok(&["edit", "film.fdx", "0", "EXT. FARM HOUSE - DAY"]);
    let args = r#"{"paragraph_id": 2, "options": ["CÉSAR"]}"#;
    f.ok(&["tool", "film.fdx", "suggest_change", args, "--review-file", "d.review.txt"]);
    f.ok(&["progress", "film.fdx", "--review", "a.review", "-o", "a.review.txt"]);
    f.ok(&["reviews", "film.fdx", "remove", "notes/b.review.txt"]);
    f.ok(&["apply", "film.fdx"]);
    let files = snapshot(&f.0);
    assert!(files.iter().any(|(p, _, _)| p.ends_with("film (scriptorium).fdx")));
    for (p, _, _) in &files {
        assert!(!p.to_string_lossy().contains(".tmp-"), "left behind: {}", p.display());
    }
}

#[test]
fn info_reads_live_doc() {
    let f = Folder::new("live");
    assert!(f.ok(&["info", "film.fdx"]).contains("words       15"));
    f.ok(&["decide", "film.fdx", "a.review.txt", "1", "accept"]);
    let dump = f.ok(&["dump", "film.fdx"]);
    assert!(dump.contains("I am going to town tomorrow.") && !dump.contains("I am go to"), "{dump}");
    let json = f.ok(&["dump", "film.fdx", "--json"]);
    let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(rows[3]["text"], "I am going to town tomorrow.");
    assert!(f.ok(&["info", "film.fdx"]).contains("words       14"));
}

#[test]
fn read_only_commands_write_nothing() {
    let f = Folder::new("readonly");
    // No session yet: reading doesn't make one.
    f.ok(&["info", "film.fdx"]);
    f.ok(&["dump", "film.fdx"]);
    f.ok(&["tool", "film.fdx", "script_info"]);
    assert!(!f.path("film.scriptorium.json").exists());

    f.ok(&["decide", "film.fdx", "a.review.txt", "1", "accept"]);
    let before = snapshot(&f.0);
    for args in [
        &["info", "film.fdx"][..],
        &["dump", "film.fdx"],
        &["dump", "film.fdx", "--json"],
        &["roundtrip", "film.fdx"],
        &["reviews", "film.fdx"],
        &["review", "film.fdx"],
        // A header-less file named on a reading command isn't pinned or attached.
        &["review", "film.fdx", "notes/b.review.txt"],
        &["suggestions", "film.fdx"],
        &["suggestions", "film.fdx", "--json", "--pending"],
        &["progress", "film.fdx"],
        &["tool", "film.fdx", "script_info"],
        &["tool", "film.fdx", "read_scene", r#"{"scene": 1}"#],
        &["tool", "film.fdx", "read_script"],
        &["tool", "film.fdx", "list_paragraphs"],
        &["tool", "film.fdx", "list_reviews", r#"{"summary": true}"#],
        &["tool", "film.fdx", "list_suggestions"],
    ] {
        f.ok(args);
    }
    // A suggestion refused by validation writes nothing either.
    let args = r#"{"paragraph_id": 0, "options": ["ext. farm house - night"]}"#;
    let (ok, _) = f.run(&["tool", "film.fdx", "suggest_change", args, "--review-file", "e.review.txt"]);
    assert!(!ok);
    assert!(before == snapshot(&f.0), "a read-only command wrote a file");
}

#[test]
fn tool_numbers_by_script_position_like_suggestions_and_decide() {
    let f = Folder::new("toolnumbers");
    // An existing suggestion on ¶1, then one on ¶0: its item is added last,
    // but it comes first in the script, so it is #1 everywhere.
    f.ok(&["reviews", "film.fdx", "add", "notes/b.review.txt"]);
    let args = r#"{"paragraph_id": 0, "options": ["EXT. FARM HOUSE - DAY"]}"#;
    let out = f.ok(&["tool", "film.fdx", "suggest_change", args]);
    assert!(out.contains("#1 (e1, ¶0)"), "{out}");
    let list = f.ok(&["suggestions", "film.fdx"]);
    assert!(list.starts_with("#1 ¶0 pending") && list.contains("#2 ¶1 pending"), "{list}");
    let rows: serde_json::Value = serde_json::from_str(&f.ok(&["tool", "film.fdx", "list_suggestions"])).unwrap();
    assert_eq!((rows[0]["n"].as_u64(), rows[0]["paragraph"].as_u64()), (Some(1), Some(0)), "{rows}");
    let out = f.ok(&["decide", "film.fdx", "1", "accept"]);
    assert!(out.contains("#1 accepted 1: EXT. FARM HOUSE - DAY"), "{out}");
    assert!(f.ok(&["dump", "film.fdx"]).contains("EXT. FARM HOUSE - DAY"));
    let out = f.ok(&["decide", "film.fdx", "2", "keep"]);
    assert!(out.contains("#2 kept: Laura smiles."), "{out}");
}

#[test]
fn tool_list_prints_the_catalogue() {
    let f = Folder::new("toollist");
    let list = f.ok(&["tool", "--list"]);
    assert_eq!(list.lines().count(), 8, "{list}");
    assert!(list.lines().any(|l| l.starts_with("suggest_change ") && l.contains("Propose")), "{list}");
    assert_eq!(f.ok(&["tool", "film.fdx", "--list"]), list);
    let specs: serde_json::Value = serde_json::from_str(&f.ok(&["tool", "--list", "--json"])).unwrap();
    let suggest = specs.as_array().unwrap().iter().find(|s| s["name"] == "suggest_change").unwrap();
    assert!(suggest["schema"]["properties"]["review_file"].is_object(), "{suggest}");
    assert!(suggest["schema"]["properties"]["before"].is_object(), "{suggest}");
    let (ok, _) = f.run(&["tool", "--list", "a", "b"]);
    assert!(!ok);
}

#[test]
fn read_only_tools_note_a_corrupt_session() {
    let f = Folder::new("toolcorrupt");
    std::fs::write(f.path("film.scriptorium.json"), "{ not a session").unwrap();
    let before = snapshot(&f.0);
    for name in ["script_info", "list_paragraphs", "read_script", "list_reviews"] {
        let out = f.ok(&["tool", "film.fdx", name]);
        assert!(out.contains("isn't used"), "{name}: {out}");
    }
    let (ok, out) = f.run(&["tool", "film.fdx", "read_scene", r#"{"scene": 1}"#]);
    assert!(ok && out.contains("¶1 Laura smiles."), "{out}");
    // A tool that writes still refuses to work over it.
    let args = r#"{"paragraph_id": 1, "options": ["Laura grins."]}"#;
    let (ok, out) = f.run(&["tool", "film.fdx", "suggest_change", args, "--review-file", "a.review.txt"]);
    assert!(!ok && out.contains("can't use the saved session"), "{out}");
    assert_eq!(snapshot(&f.0), before);
}

#[test]
fn tool_runs_from_the_library() {
    let f = Folder::new("toollib");
    let script = f.path("film.fdx").to_string_lossy().to_string();
    let mut notes = vec![];
    let out = scriptorium_cli::tool(&script, "read_scene", &serde_json::json!({"scene": 1}), None, &mut notes).unwrap();
    assert!(out.contains("¶1 Laura smiles.") && out.ends_with('\n'), "{out}");
    let review = f.path("lib.review.txt").to_string_lossy().to_string();
    let args = serde_json::json!({"paragraph_id": 1, "options": ["Laura grins."]});
    let out = scriptorium_cli::tool(&script, "suggest_change", &args, Some(&review), &mut notes).unwrap();
    assert!(out.contains("#1"), "{out}");
    assert!(std::fs::read_to_string(&review).unwrap().contains("PARA: 1"));
    let err = scriptorium_cli::tool(&script, "no_such_tool", &serde_json::json!({}), None, &mut notes).unwrap_err();
    assert!(err.contains("unknown tool"), "{err}");
}
