//! End to end through the `scriptorium` binary (plan step A12; the library
//! flow is `crates/fsio/tests/end_to_end.rs`): `version new` → `tool
//! suggest_changes` on rows → `decide` (accept, keep, rewrite) → the app's
//! write in place (`scriptorium_fsio::versions::write_in_place`; the CLI
//! doesn't write a version in place) → Final Draft edits to both files →
//! `rows`: every status, the reworded row needs work, an update into the
//! same review takes it.

use std::path::PathBuf;
use std::process::Command;

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
    <Paragraph Type="Action">
      <Text>Laura laughs.</Text>
    </Paragraph>
    <Paragraph Type="Character">
      <Text>LAURA</Text>
    </Paragraph>
    <Paragraph Type="Parenthetical">
      <Text>(quietly)</Text>
    </Paragraph>
    <Paragraph Type="Dialogue">
      <Text>Take me with you.</Text>
    </Paragraph>
    <Paragraph Type="Scene Heading">
      <Text>INT. BARN - DAY</Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text>Hay everywhere. The cows watch.</Text>
    </Paragraph>
    <Paragraph Type="Character">
      <Text>CESAR</Text>
    </Paragraph>
    <Paragraph Type="Dialogue">
      <Text>We leave at dawn.</Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text>She nods.</Text>
    </Paragraph>
  </Content>
</FinalDraft>
"#;

const REVIEW: &str = "Translation from English.review.txt";

/// A fresh folder holding `film.fdx`; removed on drop.
struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("scriptorium-cli-e2e-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
        Self(dir.canonicalize().unwrap())
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.0.join(name)).unwrap()
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.0.join(name), text).unwrap();
    }

    /// Run the binary in the folder; its stdout, or panic with its stderr.
    fn run(&self, args: &[&str]) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_scriptorium")).current_dir(&self.0).args(args).output().unwrap();
        let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "scriptorium {args:?}: {stderr}");
        stdout.into_owned()
    }

    fn suggest(&self, items: Value) -> Value {
        let args = json!({ "suggestions": items }).to_string();
        serde_json::from_str(&self.run(&["tool", "film.es.fdx", "suggest_changes", &args, "--review-file", REVIEW]))
            .unwrap()
    }

    /// `rows --json`: each row's (status, needs).
    fn rows(&self) -> Vec<(String, bool)> {
        let v: Value = serde_json::from_str(&self.run(&["rows", "film.es.fdx", "--json"])).unwrap();
        v.as_array()
            .unwrap()
            .iter()
            .map(|r| (r["status"].as_str().unwrap().to_string(), r["needs"].as_bool().unwrap()))
            .collect()
    }

    fn needs(&self) -> Vec<u64> {
        let v: Value = serde_json::from_str(&self.run(&["rows", "film.es.fdx", "--needs", "--json"])).unwrap();
        v.as_array().unwrap().iter().map(|r| r["row"].as_u64().unwrap()).collect()
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn para(kind: &str, text: &str) -> String {
    format!("    <Paragraph Type=\"{kind}\">\n      <Text>{text}</Text>\n    </Paragraph>\n")
}

fn once(text: &str, from: &str, to: &str) -> String {
    assert_eq!(text.matches(from).count(), 1, "{from:?} in\n{text}");
    text.replacen(from, to, 1)
}

#[test]
fn a_version_through_the_binary_and_final_draft() {
    let f = Folder::new("flow");
    f.run(&["version", "new", "film.fdx", "--lang", "es", "--from", "en"]);
    assert_eq!(f.needs(), [1, 2, 4, 5, 7, 8, 9, 10, 12, 13]);

    // The version's ids are the original's positions (a placeholder copy).
    let out = f.suggest(json!([
        { "row": 1, "paragraph_id": 0, "options": ["EXT. CASA DE CAMPO - NOCHE"] },
        { "row": 2, "paragraph_id": 1, "options": ["Laura sonríe."] },
        { "row": 4, "paragraph_id": 3, "options": ["Mañana voy al pueblo.", "Mañana me voy al pueblo."] },
        { "row": 5, "paragraph_id": 4, "options": ["Laura ríe."] },
        { "row": 7, "paragraph_id": 6, "options": ["(en voz baja)"] },
        { "row": 8, "paragraph_id": 7, "options": ["Llévame contigo."] },
        { "row": 12, "paragraph_id": 11, "options": ["Salimos al amanecer."] },
    ]));
    assert_eq!(out["added"], 7, "{out}");
    assert_eq!(f.needs(), [9, 10, 13]);

    // Numbered in script order: #6 and #7 stay pending.
    f.run(&["decide", "film.es.fdx", "1", "accept"]);
    f.run(&["decide", "film.es.fdx", "2", "accept"]);
    f.run(&["decide", "film.es.fdx", "3", "accept", "2"]);
    f.run(&["decide", "film.es.fdx", "4", "keep"]);
    f.run(&["decide", "film.es.fdx", "5", "rewrite", "(bajito)"]);

    // The app's Write .fdx.
    let es = f.0.join("film.es.fdx");
    let done = scriptorium_fsio::versions::write_in_place(&es, &f.read("film.es.fdx")).unwrap();
    assert_eq!(done.message, "Wrote film.es.fdx. Undo starts again from here.");
    let written = f.read("film.es.fdx");
    assert!(written.contains("<Text>Mañana me voy al pueblo.</Text>") && written.contains("<Text>(bajito)</Text>"));

    // Final Draft: the last blank Spanish line translated; an English line
    // added and the pending row's English reworded; both re-saved with other
    // whitespace.
    let blank = para("Action", "");
    let at = written.rfind(&blank).unwrap();
    let mut v = written.clone();
    v.replace_range(at..at + blank.len(), &para("Action", "Ella asiente."));
    f.write("film.es.fdx", &v.replace("\n    <Paragraph", "\n\t<Paragraph"));
    let mut e = once(
        SCRIPT,
        &para("Action", "Laura laughs."),
        &(para("Action", "Laura laughs.") + &para("Action", "A dog barks.")),
    );
    e = once(&e, "<Text>We leave at dawn.</Text>", "<Text>We leave at first light.</Text>");
    f.write("film.fdx", &e.replace("\n      <Text>", "\n\t\t<Text>"));

    let s = |t: &str, n: bool| (t.to_string(), n);
    assert_eq!(
        f.rows(),
        [
            s("in_sync", false),
            s("in_sync", false),
            s("in_sync", false),
            s("in_sync", false),
            s("blank", false),     // kept: still blank, needs nothing
            s("one_sided", false), // A dog barks.
            s("in_sync", false),
            s("in_sync", false),
            s("blank", false), // pending
            s("blank", true),
            s("blank", true),
            s("in_sync", false),
            s("blank", true),         // its pending suggestion is for the old English
            s("this_changed", false), // translated in Final Draft
        ]
    );
    assert_eq!(f.needs(), [10, 11, 13]);

    // The update, into the same review.
    let out = f.suggest(json!([{ "row": 13, "paragraph_id": 11, "options": ["Salimos con la primera luz."] }]));
    assert_eq!(out["added"], 1, "{out}");
    assert_eq!(f.needs(), [10, 11]);
    let review = f.read(REVIEW);
    assert!(review.contains("ROW TEXT: We leave at first light.\n"), "{review}");
    assert_eq!(review.matches("\nROW: ").count(), 8, "one review: {review}");
}
