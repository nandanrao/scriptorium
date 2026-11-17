//! The write tools on versions (plan step A9) through `scriptorium tool`, the
//! host MCP runs them through: `new_version` makes the version and returns
//! its absolute path (the same path when run again), suggestions on its rows
//! go into a review attached to the version's session, and an update to a
//! decided row (once the original's line changed) goes into the same review.

use std::path::PathBuf;

use serde_json::{Value, json};

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
      <Text>I am going to town tomorrow.</Text>
    </Paragraph>
  </Content>
</FinalDraft>
"#;

/// A fresh folder holding `film.fdx`; removed on drop.
struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("scriptorium-write-tools-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
        Self(dir.canonicalize().unwrap())
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }

    fn tool(&self, script: &str, name: &str, args: Value) -> Result<String, String> {
        scriptorium_cli::tool(&self.path(script), name, &args, None, &mut vec![])
    }

    fn run(&self, args: &[&str]) -> Result<String, String> {
        scriptorium_cli::run(args, &mut vec![])
    }

    /// `new_version` in Spanish on `film.fdx`: the version's path, from the
    /// result's `The version:` line.
    fn new_version(&self) -> (String, String) {
        let out = self.tool("film.fdx", "new_version", json!({ "language": "es", "original_language": "en" })).unwrap();
        let path = out.lines().find_map(|l| l.strip_prefix("The version: ")).expect(&out).to_string();
        (out, path)
    }

    fn rows(&self) -> Value {
        serde_json::from_str(&self.run(&["rows", &self.path("film.es.fdx"), "--json"]).unwrap()).unwrap()
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn new_version_returns_the_absolute_path_and_the_same_path_again() {
    let f = Folder::new("new");
    let (out, path) = f.new_version();
    assert_eq!(path, f.path("film.es.fdx"), "{out}");
    assert!(out.starts_with("made film.es.fdx (Español) and film.es.links.txt"), "{out}");
    assert!(out.contains("pass this path as `script`"), "{out}");
    assert!(std::path::Path::new(&path).is_file());

    let (again, path2) = f.new_version();
    assert_eq!(path2, path);
    assert!(again.starts_with("film.es.fdx was already made"), "{again}");
    assert!(!again.contains("lines to translate") && again.contains("read_rows with needs"), "{again}");
    // Another language: one version per script.
    let e = f.tool("film.fdx", "new_version", json!({ "language": "fr", "original_language": "en" })).unwrap_err();
    assert!(e.contains("already has a version, film.es.fdx"), "{e}");
    // On the version itself: its path.
    let out = f.tool("film.es.fdx", "new_version", json!({ "language": "es", "original_language": "en" })).unwrap();
    assert!(out.contains("is already the version (Español)") && out.contains(&path), "{out}");
}

#[test]
fn a_broken_links_file_makes_no_second_pair() {
    let f = Folder::new("broken");
    f.new_version();
    let links = f.path("film.es.links.txt");
    std::fs::write(&links, "SCRIPTORIUM LINKS v1\nnot a links file\n").unwrap();
    let e = f.tool("film.fdx", "new_version", json!({ "language": "es", "original_language": "en" })).unwrap_err();
    assert!(e.contains("film.es.links.txt can't be read"), "{e}");
    assert_eq!(std::fs::read_to_string(&links).unwrap(), "SCRIPTORIUM LINKS v1\nnot a links file\n");
}

#[test]
fn an_unusable_pair_makes_no_second_one() {
    let f = Folder::new("unusable");
    f.new_version();
    // The version renamed away: the links file names a file that isn't there.
    std::fs::rename(f.path("film.es.fdx"), f.path("elsewhere.fdx")).unwrap();
    let before = std::fs::read_to_string(f.path("film.es.links.txt")).unwrap();
    let e = f.tool("film.fdx", "new_version", json!({ "language": "es", "original_language": "en" })).unwrap_err();
    assert!(e.contains("can't be used, so no version is made"), "{e}");
    assert!(!std::path::Path::new(&f.path("film.es.fdx")).exists());
    assert_eq!(std::fs::read_to_string(f.path("film.es.links.txt")).unwrap(), before);
    // The original still takes ordinary suggestions…
    let plain = json!({ "paragraph_id": 1, "options": ["Laura grins."], "review_file": "notes.review.txt" });
    f.tool("film.fdx", "suggest_change", plain).unwrap();
    // …but the version (back under its name, its links file broken) takes none.
    std::fs::rename(f.path("elsewhere.fdx"), f.path("film.es.fdx")).unwrap();
    std::fs::write(f.path("film.es.links.txt"), format!("{before}\nnot a row\n")).unwrap();
    let cue = json!({ "paragraph_id": 2, "options": ["CÉSAR"], "review_file": "t.review.txt" });
    let e = f.tool("film.es.fdx", "suggest_change", cue).unwrap_err();
    assert!(e.contains("Suggestions on a version are placed by its links file"), "{e}");
    assert!(!std::path::Path::new(&f.path("t.review.txt")).exists());
}

#[test]
fn row_suggestions_go_into_a_review_attached_to_the_version() {
    let f = Folder::new("rows");
    f.new_version();
    let first = "Translation from English.review.txt";
    let batch = json!({
        "review_file": first,
        "suggestions": [
            { "row": 2, "paragraph_id": 1, "options": ["Laura sonríe."] },
            { "row": 4, "paragraph_id": 3, "options": ["Mañana voy al pueblo.", "Mañana me voy al pueblo."], "why": "2 is softer" },
        ],
    });
    let out: Value = serde_json::from_str(&f.tool("film.es.fdx", "suggest_changes", batch).unwrap()).unwrap();
    assert_eq!(out["added"], 2, "{out}");
    let text = std::fs::read_to_string(f.path(first)).unwrap();
    assert!(text.contains("BEFORE: [empty]\nPARA: 1\nAFTER: Laura sonríe.\n"), "{text}");
    assert!(text.contains("ROW TEXT: Laura smiles.\n"), "{text}");
    // Attached to the version's session, not the original's.
    let reviews = f.run(&["reviews", &f.path("film.es.fdx")]).unwrap();
    assert!(reviews.contains(first), "{reviews}");
    let reviews = f.run(&["reviews", &f.path("film.fdx")]).unwrap();
    assert!(!reviews.contains(first), "{reviews}");
    let rows = f.rows();
    assert_eq!(rows[1]["needs"], false);
    assert_eq!(rows[1]["suggestions"][0]["review"], first);

    // On the original: refused, naming the version.
    let on_original = json!({ "row": 2, "paragraph_id": 1, "options": ["x"], "review_file": "o.review.txt" });
    let e = f.tool("film.fdx", "suggest_change", on_original).unwrap_err();
    assert!(e.contains(&format!("rows are suggested on the version, {}", f.path("film.es.fdx"))), "{e}");

    // Decided: nothing to update while the original's line is unchanged.
    f.run(&["decide", &f.path("film.es.fdx"), &f.path(first), "1", "accept"]).unwrap();
    let update =
        json!({ "review_file": first, "suggestions": [{ "paragraph_id": 1, "options": ["Laura sonríe apenas."] }] });
    let out: Value = serde_json::from_str(&f.tool("film.es.fdx", "suggest_changes", update.clone()).unwrap()).unwrap();
    let error = out["failed"][0]["error"].as_str().unwrap();
    assert_eq!(error, format!("row 2 is already decided in review {first} (#1, accepted 1); nothing to update"));
    // The original's line changes (as Final Draft would save it): the update
    // goes in the same review, the row found from the line.
    let en = std::fs::read_to_string(f.path("film.fdx")).unwrap().replace("Laura smiles.", "Laura laughs.");
    std::fs::write(f.path("film.fdx"), en).unwrap();
    let rows = f.rows();
    // "both changed": the accepted line isn't written to the version's file yet.
    assert_eq!(rows[1]["status"], "both_changed", "{}", rows[1]);
    assert_eq!(rows[1]["needs"], true, "{}", rows[1]);
    let out: Value = serde_json::from_str(&f.tool("film.es.fdx", "suggest_changes", update).unwrap()).unwrap();
    assert_eq!(out["added"], 1, "{out}");
    let text = std::fs::read_to_string(f.path(first)).unwrap();
    assert!(text.contains("ROW TEXT: Laura laughs.\n"), "{text}");
    let rows = f.rows();
    assert_eq!(rows[1]["suggestions"].as_array().unwrap().len(), 1, "{}", rows[1]);
    assert_eq!(rows[1]["suggestions"][0]["status"], "pending", "{}", rows[1]);
}
