//! A7 from the CLI: suggestions whose lines were removed, joined, split or
//! retyped. Every command reopens the saved session, so each step also checks
//! that the rule survives saving and reopening.

use std::path::PathBuf;
use std::process::Command;

fn para(kind: &str, text: &str) -> String {
    format!("    <Paragraph Type=\"{kind}\">\n      <Text>{text}</Text>\n    </Paragraph>\n")
}

/// ids: 0 heading, 1 action, 2 character, 3 dialogue, 4 action, 5 action.
fn script() -> String {
    let body = [
        para("Scene Heading", "EXT. FARM - NIGHT"),
        para("Action", "Laura smiles."),
        para("Character", "CESAR"),
        para("Dialogue", "I am go to the town."),
        para("Action", "They play truco."),
        para("Action", "She say hello and leaves."),
    ];
    format!(
        "<?xml version=\"1.0\"?>\n<FinalDraft Version=\"5\">\n  <Content>\n{}  </Content>\n</FinalDraft>\n",
        body.concat()
    )
}

/// One suggestion on each of ¶1..=¶5: #1..#5.
const REVIEW: &str = "SCRIPT REVIEW v1\nId: lines\n\
    ---\nBEFORE: Laura smiles.\nPARA: 1\nAFTER: Laura grins.\n\
    ---\nBEFORE: CESAR\nPARA: 2\nAFTER: CÉSAR\n\
    ---\nBEFORE: I am go to the town.\nPARA: 3\nAFTER: I am going to the town.\n\
    ---\nBEFORE: They play truco.\nPARA: 4\nAFTER: They are playing truco.\n\
    ---\nBEFORE: She say hello and leaves.\nPARA: 5\nAFTER: She says hello and leaves.\n";

struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("scriptorium-lines-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("film.fdx"), script()).unwrap();
        std::fs::write(dir.join("r.review.txt"), REVIEW).unwrap();
        let f = Self(dir);
        f.ok(&["reviews", "film.fdx", "add", "r.review.txt"]);
        f
    }

    fn run(&self, args: &[&str]) -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_scriptorium")).current_dir(&self.0).args(args).output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr))
    }

    fn ok(&self, args: &[&str]) -> String {
        let (ok, text) = self.run(args);
        assert!(ok, "{args:?} failed: {text}");
        text
    }

    fn err(&self, args: &[&str]) -> String {
        let (ok, text) = self.run(args);
        assert!(!ok, "{args:?} should fail: {text}");
        text
    }

    /// (n, paragraph, status, line) per suggestion, by number.
    fn rows(&self) -> Vec<(u64, u64, String, Option<String>)> {
        let v: serde_json::Value = serde_json::from_str(&self.ok(&["suggestions", "film.fdx", "--json"])).unwrap();
        v.as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["n"].as_u64().unwrap(),
                    r["paragraph"].as_u64().unwrap(),
                    r["status"].as_str().unwrap().to_string(),
                    r["line"].as_str().map(str::to_string),
                )
            })
            .collect()
    }

    fn row(&self, n: u64) -> (u64, String, Option<String>) {
        let (_, p, s, l) = self.rows().into_iter().find(|r| r.0 == n).expect("a row numbered n");
        (p, s, l)
    }

    /// (id, text) of every paragraph.
    fn texts(&self) -> Vec<(u64, String)> {
        let v: serde_json::Value = serde_json::from_str(&self.ok(&["dump", "film.fdx", "--json"])).unwrap();
        let list = v.as_array().or_else(|| v["paragraphs"].as_array()).expect("a list").clone();
        list.iter().map(|p| (p["id"].as_u64().unwrap(), p["text"].as_str().unwrap().to_string())).collect()
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn row(p: u64, status: &str, line: Option<&str>) -> (u64, String, Option<String>) {
    (p, status.to_string(), line.map(str::to_string))
}

#[test]
fn removed_line_keeps_its_number_and_cant_be_decided() {
    let f = Folder::new("removed");
    f.ok(&["remove", "film.fdx", "3"]);
    let numbers: Vec<(u64, u64)> = f.rows().iter().map(|r| (r.0, r.1)).collect();
    assert_eq!(numbers, [(1, 1), (2, 2), (3, 3), (4, 4), (5, 5)], "no #n shifts");
    assert_eq!(f.row(3), row(3, "line removed", Some("line removed")));
    for d in [&["accept"][..], &["keep"], &["undecide"], &["rewrite", "x"]] {
        let e = f.err(&[&["decide", "film.fdx", "3"][..], d].concat());
        assert!(e.contains("#3: line removed"), "{e}");
    }
    // The others decide as before, under the same numbers.
    assert!(f.ok(&["decide", "film.fdx", "4", "accept"]).starts_with("#4 accepted 1: They are playing truco."));
    let text = f.ok(&["suggestions", "film.fdx"]);
    assert!(text.contains("#3 ¶3 line removed\n  was: I am go to the town.\n"), "{text}");

    let summary = f.ok(&["review", "film.fdx"]);
    assert!(summary.contains("1 accepted, 0 kept, 0 rewritten, 3 pending"), "{summary}");
    assert!(summary.contains("line removed  1"), "{summary}");
    let list = f.ok(&["reviews", "film.fdx"]);
    assert!(list.contains("1 of 5 decided") && list.contains("1 on lines removed since"), "{list}");
    // Progress keeps it pending, with no MINE.
    let progress = f.ok(&["progress", "film.fdx"]);
    let block = progress.split("---").find(|b| b.contains("PARA: 3")).unwrap();
    assert!(!block.contains("STATUS") && !block.contains("MINE"), "{block}");
    // --accept-all skips it, and says why.
    let out = f.ok(&["apply", "film.fdx", "--accept-all", "-o", "out.fdx"]);
    assert!(out.contains("skipped r.review.txt #3: line removed: ¶3 was removed"), "{out}");
    // Undo brings the line back, pending.
    f.ok(&["undo", "film.fdx"]);
    f.ok(&["undo", "film.fdx"]);
    assert_eq!(f.row(3), row(3, "pending", None));
}

#[test]
fn decided_line_removed_shows_as_line_cut() {
    let f = Folder::new("decided");
    f.ok(&["decide", "film.fdx", "1", "accept"]);
    f.ok(&["decide", "film.fdx", "4", "keep"]);
    f.ok(&["remove", "film.fdx", "1"]);
    f.ok(&["remove", "film.fdx", "4"]);
    let numbers: Vec<(u64, u64)> = f.rows().iter().map(|r| (r.0, r.1)).collect();
    assert_eq!(numbers, [(1, 1), (2, 2), (3, 3), (4, 4), (5, 5)]);
    assert_eq!(f.row(1), row(1, "line cut", Some("line cut")));
    assert_eq!(f.row(4), row(4, "line cut", Some("line cut")));
    let text = f.ok(&["suggestions", "film.fdx"]);
    assert!(text.contains("#1 ¶1 line cut\n  was: Laura smiles.\n"), "{text}");
    assert!(f.err(&["decide", "film.fdx", "1", "undecide"]).contains("#1: line removed"));
    let progress = f.ok(&["progress", "film.fdx"]);
    for p in ["PARA: 1", "PARA: 4"] {
        let block = progress.split("---").find(|b| b.contains(p)).unwrap();
        assert!(block.contains("STATUS: rewritten") && block.contains("MINE: [cut]"), "{block}");
    }
}

#[test]
fn joined_lines() {
    let f = Folder::new("joined");
    // ¶5 joined onto ¶4: #5's line is gone, #4's has the next line on it.
    f.ok(&["join", "film.fdx", "4", "5"]);
    assert_eq!(f.row(5), row(5, "line removed", Some("line removed")));
    assert_eq!(f.row(4), row(4, "pending", Some("line joined with the next")));
    let e = f.err(&["decide", "film.fdx", "4", "accept"]);
    assert!(e.contains("#4: line changed") && e.contains("; use decide 4 rewrite <text>"), "{e}");
    let text = f.ok(&["suggestions", "film.fdx"]);
    assert!(text.contains("#4 ¶4 pending (line changed since: line joined with the next)"), "{text}");
    f.ok(&["decide", "film.fdx", "4", "rewrite", "They are playing truco. She says hello."]);
    assert_eq!(f.row(4).1, "rewritten");
}

/// The review's repro: deciding on a split head wrote the whole line back
/// into it, giving "CESAR" + "SAR".
#[test]
fn split_head_never_duplicates_the_tail() {
    let f = Folder::new("split");
    f.ok(&["decide", "film.fdx", "2", "accept"]);
    f.ok(&["split", "film.fdx", "2", "--at", "SAR"]);
    assert_eq!(f.row(2), row(2, "rewritten", Some("line split")));
    for d in [&["accept"][..], &["keep"]] {
        let e = f.err(&[&["decide", "film.fdx", "2"][..], d].concat());
        assert!(e.contains("#2: line changed") && e.contains("split"), "{e}");
    }
    assert!(f.ok(&["decide", "film.fdx", "2", "undecide"]).starts_with("#2 pending: CÉ"));
    let texts: Vec<String> = f.texts().into_iter().map(|t| t.1).collect();
    assert!(texts.contains(&"CÉ".to_string()) && texts.contains(&"SAR".to_string()), "{texts:?}");
    assert!(!texts.iter().any(|t| t.contains("CESAR") || t.contains("CÉSAR")), "{texts:?}");
    assert_eq!(f.row(2), row(2, "pending", Some("line split")));
    // A pending split line: accept refused, keep and rewrite allowed.
    f.ok(&["split", "film.fdx", "5", "--at", " and leaves."]);
    assert!(f.err(&["decide", "film.fdx", "5", "accept"]).contains("#5: line changed"));
    let text = f.ok(&["suggestions", "film.fdx"]);
    assert!(text.contains("#5 ¶5 pending (line changed since: line split)"), "{text}");
    f.ok(&["decide", "film.fdx", "5", "rewrite", "She says hello"]);
    let texts: Vec<String> = f.texts().into_iter().map(|t| t.1).collect();
    assert!(texts.contains(&"She says hello".to_string()) && texts.contains(&" and leaves.".to_string()));
}

#[test]
fn retyped_line_stays_outdated_and_takes_decisions() {
    let f = Folder::new("retyped");
    f.ok(&["retype", "film.fdx", "1", "General"]);
    assert_eq!(f.row(1), row(1, "pending", Some("line retyped")));
    let text = f.ok(&["suggestions", "film.fdx"]);
    assert!(text.contains("#1 ¶1 pending (line changed since: line retyped)"), "{text}");
    assert!(f.ok(&["decide", "film.fdx", "1", "accept"]).starts_with("#1 accepted 1: Laura grins."));
}

#[test]
fn numbers_stay_when_lines_around_are_added_and_removed() {
    let f = Folder::new("numbers");
    f.ok(&["insert", "film.fdx", "--after", "2", "Action", "A new line."]);
    f.ok(&["remove", "film.fdx", "2"]);
    f.ok(&["remove", "film.fdx", "5"]);
    let numbers: Vec<(u64, u64)> = f.rows().iter().map(|r| (r.0, r.1)).collect();
    assert_eq!(numbers, [(1, 1), (2, 2), (3, 3), (4, 4), (5, 5)]);
    assert!(f.ok(&["decide", "film.fdx", "3", "accept"]).starts_with("#3 accepted 1: I am going to the town."));
    assert!(f.err(&["decide", "film.fdx", "2", "accept"]).contains("#2: line removed"));
    assert!(f.err(&["decide", "film.fdx", "e1", "accept"]).contains("#2: line removed"));
}

/// The review's repro: accept #1 (13 characters -> 12), then split at 12,
/// the end of the line as it is now: not a split.
#[test]
fn enter_at_end_of_an_accepted_shorter_line_is_not_a_split() {
    let f = Folder::new("shorter");
    f.ok(&["decide", "film.fdx", "1", "accept"]); // "Laura smiles." -> "Laura grins."
    f.ok(&["split", "film.fdx", "1", "--offset", "12"]);
    assert_eq!(f.row(1), row(1, "accepted 1", None));
    let text = f.ok(&["suggestions", "film.fdx"]);
    assert!(text.contains("#1 ¶1 accepted 1\n"), "{text}");
    assert!(f.ok(&["decide", "film.fdx", "1", "keep"]).starts_with("#1 kept: Laura smiles."));
    assert!(f.ok(&["decide", "film.fdx", "1", "undecide"]).starts_with("#1 pending: Laura smiles."));
    assert_eq!(f.row(1), row(1, "pending", None));
    // A pending line shortened by hand, then Enter at its end: the same.
    f.ok(&["edit", "film.fdx", "5", "She leaves."]);
    f.ok(&["split", "film.fdx", "5", "--offset", "11"]);
    assert_eq!(f.row(5), row(5, "pending", None));
}

/// `apply --accept-all` names what it left and why.
#[test]
fn accept_all_reports_what_it_skipped() {
    let f = Folder::new("acceptall");
    f.ok(&["split", "film.fdx", "5", "--at", " and leaves."]);
    f.ok(&["join", "film.fdx", "3", "4"]);
    let out = f.ok(&["apply", "film.fdx", "--accept-all", "-o", "out.fdx"]);
    assert!(out.contains("skipped r.review.txt #3: line changed: ¶3 was joined with the next line"), "{out}");
    assert!(out.contains("skipped r.review.txt #4: line removed"), "{out}");
    assert!(out.contains("skipped r.review.txt #5: line changed: ¶5 was split"), "{out}");
    assert!(out.contains("use decide 5 rewrite <text>"), "{out}");
    assert!(!out.contains("#1") && !out.contains("#2"), "{out}");
}
