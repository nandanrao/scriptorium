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
    assert!(!ok && text.contains("name one with review"), "{text}");
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
/// The session lock file is left out: a command that may write creates it
/// (empty) before finding it has nothing to write.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.to_string_lossy().ends_with(".scriptorium.lock") {
                continue;
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
    assert!(!ok && text.contains("give review_file"), "{text}");
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

    // An existing review without an `Id:` is attached with its Id pinned.
    let args = r#"{"paragraph_id": 3, "options": ["I go to town tomorrow."]}"#;
    let out = f.ok(&["tool", "film.fdx", "suggest_change", args, "--review-file", "notes/b.review.txt"]);
    assert!(out.contains("#2 (e1, ¶3)"), "{out}");
    let b = std::fs::read_to_string(f.path("notes/b.review.txt")).unwrap();
    assert!(b.contains("\nId: ") && b.contains("BEFORE: Laura smiles.") && b.contains("PARA: 3"), "{b}");
    assert_eq!(session(&f.0)["reviews"].as_array().unwrap().len(), 2);

    // A review_file that isn't attached must be a *.review.txt inside the
    // script's folder (no `..`, no links out, no absolute path elsewhere),
    // and an existing one must read as a review. Arguments not offered are
    // refused.
    let outside = f.0.parent().unwrap().join(format!("scriptorium-cli-outside-{}", std::process::id()));
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("x.review.txt"), "SCRIPT REVIEW v1\n").unwrap();
    std::fs::write(f.path("notes.txt"), "keep me\n").unwrap();
    std::fs::write(f.path("config.review.txt"), "export PATH=/tmp\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, f.path("out")).unwrap();
    let abs_new = outside.join("new.review.txt").to_string_lossy().into_owned();
    let abs_old = outside.join("x.review.txt").to_string_lossy().into_owned();
    let mut bad = vec!["notes.txt", "config.review.txt", abs_new.as_str(), abs_old.as_str()];
    let up = format!("../{}/new.review.txt", outside.file_name().unwrap().to_string_lossy());
    bad.push(&up);
    if cfg!(unix) {
        bad.push("out/new.review.txt");
    }
    for bad in bad {
        let (ok, text) = f.run(&["tool", "film.fdx", "suggest_change", args, "--review-file", bad]);
        assert!(!ok && text.contains("review"), "{bad}: {text}");
    }
    assert_eq!(std::fs::read_to_string(f.path("notes.txt")).unwrap(), "keep me\n");
    assert_eq!(std::fs::read_to_string(f.path("config.review.txt")).unwrap(), "export PATH=/tmp\n");
    assert!(!outside.join("new.review.txt").exists());
    assert_eq!(std::fs::read_to_string(outside.join("x.review.txt")).unwrap(), "SCRIPT REVIEW v1\n");
    // A file that isn't a review: refused until the user attaches it.
    let (ok, text) = f.run(&["tool", "film.fdx", "suggest_change", args, "--review-file", "config.review.txt"]);
    assert!(!ok && text.contains("isn't a review file") && text.contains("reviews add"), "{text}");
    // Attached by the user, a review outside the folder can be named.
    f.ok(&["reviews", "film.fdx", "add", &abs_old]);
    let other = r#"{"paragraph_id": 0, "options": ["EXT. FARM HOUSE - DAWN"]}"#;
    let out = f.ok(&["tool", "film.fdx", "suggest_change", other, "--review-file", &abs_old]);
    assert!(out.contains("#1"), "{out}");
    assert!(std::fs::read_to_string(outside.join("x.review.txt")).unwrap().contains("DAWN"));
    f.ok(&["reviews", "film.fdx", "remove", &abs_old]);
    std::fs::remove_dir_all(&outside).unwrap();
    let (ok, text) = f.run(&["tool", "film.fdx", "read_scene", r#"{"scene": 1, "path": "/etc"}"#]);
    assert!(!ok && text.contains("`path`"), "{text}");
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

/// Concurrent commands each decide a different suggestion: the session lock
/// makes them take turns, so every decision is in the session afterwards
/// (without it, one writer saved over another's decision about half the time).
#[test]
fn concurrent_decides_all_land() {
    const N: usize = 6;
    let f = Folder::new("concurrent");
    let mut script = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\" ?>\n<FinalDraft DocumentType=\"Script\" Template=\"No\" Version=\"5\">\n  <Content>\n",
    );
    let mut review = String::from("SCRIPT REVIEW v1\nScript: many.fdx\n\n");
    for k in 0..N {
        script +=
            &format!("    <Paragraph Type=\"Action\">\n      <Text>Line number {k} here.</Text>\n    </Paragraph>\n");
        review += &format!("---\nBEFORE: Line number {k} here.\nAFTER: Line number {k} changed.\n");
    }
    script += "  </Content>\n</FinalDraft>\n";
    std::fs::write(f.path("many.fdx"), script).unwrap();
    std::fs::write(f.path("many.review.txt"), review).unwrap();
    f.ok(&["reviews", "many.fdx", "add", "many.review.txt"]);
    // From threads through the library, as the MCP server calls it.
    let script = f.path("many.fdx").to_string_lossy().into_owned();
    std::thread::scope(|s| {
        for k in 0..N {
            let script = &script;
            s.spawn(move || {
                let n = scriptorium_cli::SuggestionRef::Entry(k);
                let choice = scriptorium_cli::Choice::Keep;
                scriptorium_cli::commands::decide(script, None, n, choice, &mut vec![]).unwrap();
            });
        }
    });
    let list: serde_json::Value = serde_json::from_str(&f.ok(&["suggestions", "many.fdx", "--json"])).unwrap();
    assert!(list.as_array().unwrap().iter().all(|s| s["status"] == "kept"), "{list}");
    // Alternately accept and undecide every suggestion, all at once.
    for round in 0..20 {
        let (choice, status) = if round % 2 == 0 { ("accept", "accepted 1") } else { ("undecide", "pending") };
        let children: Vec<_> = (0..N)
            .map(|k| {
                Command::new(env!("CARGO_BIN_EXE_scriptorium"))
                    .current_dir(&f.0)
                    .args(["decide", "many.fdx", &format!("e{k}"), choice])
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for c in children {
            let out = c.wait_with_output().unwrap();
            assert!(out.status.success(), "round {round}: {}", String::from_utf8_lossy(&out.stderr));
        }
        let list: serde_json::Value = serde_json::from_str(&f.ok(&["suggestions", "many.fdx", "--json"])).unwrap();
        let statuses: Vec<&str> = list.as_array().unwrap().iter().map(|s| s["status"].as_str().unwrap()).collect();
        assert_eq!(statuses, [status; N], "round {round}: a decision was lost");
    }
}

#[test]
fn suggest_and_decide_blocks() {
    let f = Folder::new("blocks");
    let out = f.ok(&[
        "suggest",
        "c.review.txt",
        "film.fdx",
        "--paras",
        "2-3",
        "--block",
        "character: CESAR\ndialogue: I go to town.",
        "--block",
        "[cut]",
        "--why",
        "tighter",
    ]);
    assert_eq!(out.trim(), "added suggestion #1 (e0, ¶2–¶3) to c.review.txt");
    let text = std::fs::read_to_string(f.path("c.review.txt")).unwrap();
    assert!(text.starts_with("SCRIPT REVIEW v2"), "{text}");
    assert!(text.contains("PARA: 2, 3") && text.contains("AFTER: [cut]"), "{text}");
    f.ok(&["reviews", "film.fdx", "add", "c.review.txt"]);
    // Option 2 cuts both lines.
    let out = f.ok(&["decide", "film.fdx", "c.review.txt", "1", "accept", "2"]);
    assert!(out.contains("[cut]"), "{out}");
    let dump = f.ok(&["dump", "film.fdx"]);
    assert!(!dump.contains("CESAR"), "{dump}");
    // Rewritten with the writer's own lines (any case for TYPE).
    let out = f.ok(&["decide", "film.fdx", "c.review.txt", "1", "--rewrite-block", "Action: Cesar shrugs."]);
    assert!(out.contains("ACTION: Cesar shrugs."), "{out}");
    let dump = f.ok(&["dump", "film.fdx"]);
    assert!(dump.contains("Cesar shrugs.") && !dump.contains("I am go"), "{dump}");
    // Undecided: the lines it was made on come back.
    f.ok(&["decide", "film.fdx", "c.review.txt", "1", "undecide"]);
    let dump = f.ok(&["dump", "film.fdx"]);
    assert!(dump.contains("CESAR") && dump.contains("I am go to the town tomorrow."), "{dump}");
    // '' cuts too.
    let out = f.ok(&["decide", "film.fdx", "c.review.txt", "1", "rewrite-block", ""]);
    assert!(out.contains("[cut]"), "{out}");
    let (ok, text) = f.run(&["decide", "film.fdx", "c.review.txt", "1", "--rewrite-block", "MONTAGE: x"]);
    assert!(!ok && text.contains("unknown element type"), "{text}");
}

#[test]
fn suggest_insert_after_and_block_refusals() {
    let f = Folder::new("blockinsert");
    let out = f.ok(&["suggest", "c.review.txt", "film.fdx", "--insert-after", "1", "--block", "ACTION: Thunder."]);
    assert_eq!(out.trim(), "added suggestion #1 (e0, after ¶1) to c.review.txt");
    let text = std::fs::read_to_string(f.path("c.review.txt")).unwrap();
    assert!(text.contains("PARA: 1\n") && text.contains("ACTION: Laura smiles.\n  ACTION: Thunder."), "{text}");
    // A one-line block on one paragraph (a retype) and a list of ids.
    let out = f.ok(&["suggest", "d.review.txt", "film.fdx", "--para", "1", "--block", "general: Laura smiles."]);
    assert!(out.contains("(e0, ¶1)"), "{out}");
    let out = f.ok(&["suggest", "e.review.txt", "film.fdx", "--paras", "2,3", "--block", ""]);
    assert!(out.contains("(e0, ¶2–¶3)"), "{out}");

    let refused = |args: &[&str], want: &str| {
        let (ok, text) = f.run(args);
        assert!(!ok && text.contains(want), "{args:?}: {text}");
    };
    let base = ["suggest", "x.review.txt", "film.fdx"];
    let with = |rest: &[&'static str]| base.iter().copied().chain(rest.iter().copied()).collect::<Vec<_>>();
    refused(&with(&["--insert-after", "1", "--block", "[cut]"]), "insert_after adds lines");
    refused(&with(&["--paras", "2-3", "--after", "x"]), "give the lines with --block");
    refused(&with(&["--paras", "2-3"]), "give at least one --block");
    refused(&with(&["--paras", "3-2", "--block", ""]), "comes before");
    refused(&with(&["--paras", "1,3", "--block", ""]), "aren't next to each other");
    refused(&with(&["--para", "1", "--paras", "2-3", "--block", ""]), "give exactly one of");
    refused(
        &with(&["--paras", "2-3", "--block", "CHARACTER: CESAR\nDIALOGUE: I am go to the town tomorrow."]),
        "wouldn't be visible",
    );
    refused(&with(&["--paras", "2-3", "--block", "just text"]), "TYPE: text");
    assert!(!f.path("x.review.txt").exists());
}

#[test]
fn block_listing_and_plain_rewrite_refusal() {
    let f = Folder::new("blocklist");
    f.ok(&[
        "suggest",
        "c.review.txt",
        "film.fdx",
        "--paras",
        "2-3",
        "--block",
        "character: CESAR\ndialogue: I go to town.",
        "--block",
        "[cut]",
        "--why",
        "tighter",
    ]);
    f.ok(&["reviews", "film.fdx", "add", "c.review.txt"]);
    let out = f.ok(&["suggestions", "film.fdx"]);
    assert!(out.contains("#1 ¶2–¶3 block pending"), "{out}");
    assert!(out.contains("  now:\n    CHARACTER: CESAR\n"), "{out}");
    assert!(out.contains("  1:\n    CHARACTER: CESAR\n    DIALOGUE: I go to town.\n"), "{out}");
    assert!(out.contains("  2:\n    [cut]\n"), "{out}");
    let json: serde_json::Value = serde_json::from_str(&f.ok(&["suggestions", "film.fdx", "--json"])).unwrap();
    assert_eq!(json[0]["paragraph_ids"], serde_json::json!([2, 3]));
    assert_eq!(json[0]["shape"], "block");
    assert_eq!(json[0]["options"][0][1]["kind"], "Dialogue");
    assert_eq!(json[0]["options"][1], serde_json::json!([]));
    assert_eq!(json[0]["current"][0]["text"], "CESAR");
    // Plain text would collapse the block into one line.
    let (ok, text) = f.run(&["decide", "film.fdx", "1", "rewrite", "one line"]);
    assert!(!ok && text.contains("--rewrite-block") && text.contains("lines"), "{text}");
    assert!(f.ok(&["dump", "film.fdx"]).contains("CESAR"));
}
