//! `versions` and `rows` (plan step A5c) on a small pair in a temporary
//! folder, made as `version new` makes it (`blank_copy`, the original's
//! bytes spliced); and `apply` refused on the version without `-o`.

use std::path::{Path, PathBuf};

use scriptorium_core::ElementKind;
use scriptorium_fdx::FdxFile;
use scriptorium_review::sessionfile::hash;
use scriptorium_versions::{Member, blank_copy, links};
use serde_json::Value;

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
    <Paragraph Type="Scene Heading">
      <Text>INT. BARN - DAY</Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text>Hay everywhere.</Text>
    </Paragraph>
  </Content>
</FinalDraft>
"#;

/// A fresh folder with `film.fdx`, its placeholder version `film.es.fdx` and
/// `film.es.links.txt`; removed on drop.
struct Pair(PathBuf);

impl Pair {
    fn new(name: &str) -> Self {
        Self::of(name, SCRIPT, "film.fdx")
    }

    fn of(name: &str, raw: &str, file: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("scriptorium-rows-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(file), raw).unwrap();
        let stem = file.strip_suffix(".fdx").unwrap();
        let fdx = FdxFile::parse(raw.to_string()).unwrap();
        let original = Member { file: file.into(), language: Some("en".into()), hash: hash(raw), created: None };
        let version_name = format!("{stem}.es.fdx");
        let version =
            Member { file: version_name.clone(), language: Some("es".into()), hash: String::new(), created: None };
        let (script, mut pair) =
            blank_copy(&fdx.to_script(), "lk-test", original, version, |k: &ElementKind| fdx.is_caps(k.name()))
                .unwrap();
        let written = scriptorium_fdx::clear_text_offsets(&fdx.write_script(&script).unwrap());
        pair.members[1].hash = hash(&written);
        std::fs::write(dir.join(&version_name), &written).unwrap();
        std::fs::write(dir.join(format!("{stem}.es.links.txt")), links::write(&pair).unwrap()).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }

    fn run(&self, args: &[&str]) -> Result<(String, Vec<String>), String> {
        let mut notes = vec![];
        scriptorium_cli::run(args, &mut notes).map(|out| (out, notes))
    }

    fn json(&self, args: &[&str]) -> Value {
        let (out, _) = self.run(args).unwrap();
        serde_json::from_str(&out).unwrap()
    }
}

impl Drop for Pair {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn statuses(rows: &Value) -> Vec<(String, bool)> {
    rows.as_array()
        .unwrap()
        .iter()
        .map(|r| (r["status"].as_str().unwrap().to_string(), r["needs"].as_bool().unwrap()))
        .collect()
}

fn s(status: &str, needs: bool) -> (String, bool) {
    (status.to_string(), needs)
}

#[test]
fn a_new_version_reads_headings_blank_and_names_in_sync() {
    let p = Pair::new("new");
    let es = p.path("film.es.fdx");
    let rows = p.json(&["rows", &es, "--json"]);
    assert_eq!(
        statuses(&rows),
        vec![
            s("blank", true),    // heading, copied: still its placeholder
            s("blank", true),    // action, blanked
            s("in_sync", false), // character, copied
            s("blank", true),    // dialogue, blanked
            s("blank", true),
            s("blank", true),
        ]
    );
    assert_eq!(rows[0]["label"], "not yet translated");
    assert_eq!(rows[0]["this"][0]["text"], "EXT. FARM HOUSE - NIGHT");
    assert_eq!(rows[1]["this"][0]["text"], "");
    assert_eq!(rows[1]["this"][0]["kind"], "Action");
    assert_eq!(rows[1]["other"][0]["text"], "Laura smiles.");

    let (text, notes) = p.run(&["rows", &es]).unwrap();
    assert!(notes.is_empty(), "{notes:?}");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "# Español (film.es.fdx) | English (film.fdx)");
    assert_eq!(lines[2], "#2 not yet translated · needs  ¶1 [empty] | ¶1 Laura smiles.");
    assert_eq!(lines[3], "#3 in sync  ¶2 CESAR | ¶2 CESAR");

    // --needs leaves the cue out; numbers stay the rows'.
    let (text, _) = p.run(&["rows", &es, "--needs"]).unwrap();
    assert!(!text.contains("#3 "), "{text}");
    assert!(text.contains("#4 not yet translated"), "{text}");
}

#[test]
fn on_the_original_nothing_needs_work() {
    let p = Pair::new("original");
    let en = p.path("film.fdx");
    let rows = p.json(&["rows", &en, "--json"]);
    assert!(statuses(&rows).iter().all(|(_, needs)| !needs));
    assert_eq!(rows[1]["this"][0]["text"], "Laura smiles.");
    assert_eq!(rows[1]["other"][0]["text"], "");
    let (text, _) = p.run(&["rows", &en, "--needs"]).unwrap();
    assert_eq!(text.lines().count(), 1, "only the header: {text}");

    let v = p.json(&["versions", &en, "--json"]);
    assert_eq!(v["pair"]["this"]["file"], "film.fdx");
    assert_eq!(v["pair"]["other"]["file"], "film.es.fdx");
    assert_eq!(v["pair"]["other"]["made"], true);
    assert_eq!(v["pair"]["this_is_version"], false);
    assert_eq!(v["pair"]["needs"], 0);
}

#[test]
fn versions_counts_rows_by_status() {
    let p = Pair::new("versions");
    let es = p.path("film.es.fdx");
    let v = p.json(&["versions", &es, "--json"]);
    let pair = &v["pair"];
    assert_eq!(pair["id"], "lk-test");
    assert_eq!(pair["this"]["language"], "es");
    assert_eq!(pair["this"]["label"], "Español");
    assert_eq!(pair["other"]["label"], "English");
    assert_eq!(pair["rows"], 6);
    assert_eq!(pair["status"]["blank"], 5);
    assert_eq!(pair["status"]["in_sync"], 1);
    assert_eq!(pair["needs"], 5);
    assert_eq!(pair["new_in_original"], serde_json::json!([]));

    let (text, _) = p.run(&["versions", &es]).unwrap();
    assert!(text.contains("this     film.es.fdx: Español (es), the version, made by Scriptorium"), "{text}");
    assert!(text.contains("rows     6: 1 in sync, 5 not yet translated"), "{text}");
    assert!(text.contains("needs    5"), "{text}");
}

#[test]
fn a_script_with_no_pair() {
    let p = Pair::new("none");
    std::fs::write(p.0.join("other.fdx"), SCRIPT).unwrap();
    let other = p.path("other.fdx");
    let (text, _) = p.run(&["versions", &other]).unwrap();
    assert!(text.starts_with("no version is lined up with"), "{text}");
    assert_eq!(p.json(&["versions", &other, "--json"])["pair"], Value::Null);
    let e = p.run(&["rows", &other]).unwrap_err();
    assert!(e.contains("no version is lined up"), "{e}");
}

#[test]
fn a_broken_links_file_is_an_error_and_left_alone() {
    let p = Pair::new("broken");
    let at = p.0.join("film.es.links.txt");
    let text = std::fs::read_to_string(&at).unwrap();
    // Two rows naming one id: refused when read.
    let broken = text.replacen("1 | 1", "0 | 1", 1);
    assert_ne!(broken, text);
    std::fs::write(&at, &broken).unwrap();
    // Change the version too: a re-find would rewrite the links file if it
    // were read at all.
    let es = p.path("film.es.fdx");
    let v = std::fs::read_to_string(&es).unwrap();
    std::fs::write(&es, v.replace("<Text>CESAR</Text>", "<Text>CÉSAR</Text>")).unwrap();
    for cmd in ["versions", "rows"] {
        let e = p.run(&[cmd, &es]).unwrap_err();
        assert!(e.contains("film.es.links.txt") && e.contains("line"), "{e}");
    }
    assert_eq!(std::fs::read_to_string(&at).unwrap(), broken);
}

#[test]
fn apply_on_the_version_needs_an_output() {
    let p = Pair::new("apply");
    let es = p.path("film.es.fdx");
    let e = p.run(&["apply", &es]).unwrap_err();
    assert!(e.contains("film.es.fdx is written from the app"), "{e}");
    assert!(!p.0.join("film.es (scriptorium).fdx").exists());
    let out = p.path("out.fdx");
    p.run(&["apply", &es, "-o", &out]).unwrap();
    assert!(Path::new(&out).exists());
    // The original isn't the made member: its default output still works.
    p.run(&["apply", &p.path("film.fdx")]).unwrap();
}

#[test]
fn a_changed_original_is_found_again_first() {
    let p = Pair::new("refind");
    let en = p.path("film.fdx");
    let es = p.path("film.es.fdx");
    // Final Draft adds a line to the English and saves.
    let added = SCRIPT.replace(
        "    <Paragraph Type=\"Character\">",
        "    <Paragraph Type=\"Action\">\n      <Text>A dog barks.</Text>\n    </Paragraph>\n    <Paragraph Type=\"Character\">",
    );
    std::fs::write(&en, &added).unwrap();
    let links_at = p.0.join("film.es.links.txt");
    let before = std::fs::read_to_string(&links_at).unwrap();

    let (text, notes) = p.run(&["rows", &es]).unwrap();
    assert_eq!(
        notes,
        vec!["film.fdx changed outside Scriptorium: 6 lines found again, 0 changed, 1 new, 0 gone.".to_string()]
    );
    assert!(text.contains("#3 nothing on one side  — | ¶2 A dog barks."), "{text}");
    assert!(text.contains("    note: nothing on this side"), "{text}");
    // Rows after it moved on the English side only.
    assert!(text.contains("#4 in sync  ¶2 CESAR | ¶3 CESAR"), "{text}");

    let after = std::fs::read_to_string(&links_at).unwrap();
    assert_ne!(after, before);
    let parsed = links::parse(&after).unwrap();
    assert_eq!(parsed.members[0].hash, hash(&added));

    // Found once: the next read says nothing and writes nothing.
    let (_, notes) = p.run(&["versions", &es]).unwrap();
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(std::fs::read_to_string(&links_at).unwrap(), after);
    let v = p.json(&["versions", &es, "--json"]);
    assert_eq!(v["pair"]["new_in_original"], serde_json::json!([3]));
    assert_eq!(v["pair"]["status"]["one_sided"], 1);
}

#[test]
fn a_missing_member_says_how_to_fix_it() {
    let p = Pair::new("missing");
    std::fs::rename(p.0.join("film.es.fdx"), p.0.join("renamed.fdx")).unwrap();
    let e = p.run(&["rows", &p.path("film.fdx")]).unwrap_err();
    assert!(e.contains("film.es.fdx is missing"), "{e}");
    assert!(e.contains("edit line 6 of film.es.links.txt"), "{e}");
    assert!(e.contains("renamed.fdx holds the bytes it records"), "{e}");
}

#[test]
fn rows_of_one_scene() {
    let p = Pair::new("scene");
    let es = p.path("film.es.fdx");
    let by_number = p.json(&["rows", &es, "--scene", "2", "--json"]);
    let rows: Vec<u64> = by_number.as_array().unwrap().iter().map(|r| r["row"].as_u64().unwrap()).collect();
    assert_eq!(rows, vec![5, 6]);
    let by_text = p.json(&["rows", &es, "--scene", "farm", "--json"]);
    let rows: Vec<u64> = by_text.as_array().unwrap().iter().map(|r| r["row"].as_u64().unwrap()).collect();
    assert_eq!(rows, vec![1, 2, 3, 4]);
    let e = p.run(&["rows", &es, "--scene", "9"]).unwrap_err();
    assert!(e.contains("no scene 9 (1 to 2)"), "{e}");
    let e = p.run(&["rows", &es, "--scene", "kitchen"]).unwrap_err();
    assert!(e.contains("no scene heading contains"), "{e}");
}

/// The real script, when present: every heading reads blank and needs a
/// suggestion; every character cue reads in sync.
#[test]
fn real_script_new_version() {
    let path = std::env::var("SCRIPTORIUM_REAL_FDX")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../2026_11_05_PURPLE LIST.fdx"));
    let Ok(raw) = std::fs::read_to_string(&path) else {
        eprintln!("skipped: {} not found", path.display());
        return;
    };
    let p = Pair::of("real", &raw, "real.fdx");
    let rows = p.json(&["rows", &p.path("real.es.fdx"), "--json"]);
    let mut headings = 0;
    for r in rows.as_array().unwrap() {
        let kind = r["other"][0]["kind"].as_str().unwrap();
        let blank = r["other"].as_array().unwrap().iter().all(|x| x["text"].as_str().unwrap().trim().is_empty());
        match kind {
            "Scene Heading" if !blank => {
                headings += 1;
                assert_eq!(r["status"], "blank", "{r}");
                assert_eq!(r["needs"], true, "{r}");
            }
            "Character" => assert_eq!(r["status"], "in_sync", "{r}"),
            _ => {}
        }
    }
    assert!(headings > 0);
}
