//! Opening a version with its pair (plan step I1): `ROW:` suggestions are
//! placed through the pair's resolver on every open, so a decision on a row
//! survives a Final Draft save of the version (set aside, back from the
//! review file), and a links file that can't be used still opens the script,
//! its `ROW:` suggestions not found with the reason.

use std::path::PathBuf;

use scriptorium_fsio::opened::Opened;
use scriptorium_fsio::versions::{PairState, version_new};
use scriptorium_review::{Decision, Status, Unplaced};
use scriptorium_versions::links;

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

/// A folder with `film.fdx`, the version `version new` makes of it, and a
/// review for the version with one `ROW:` suggestion on the blank dialogue;
/// removed on drop.
struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("scriptorium-rows-open-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
        version_new(&dir.join("film.fdx"), "es", Some("en"), None, &mut vec![]).unwrap();
        let id = links::parse(&std::fs::read_to_string(dir.join("film.es.links.txt")).unwrap()).unwrap().id;
        let review = format!(
            "SCRIPT REVIEW v1\nScript: film.es.fdx\nId: tr-1\n\n---\nBEFORE: [empty]\nAFTER: Mañana voy al \
             pueblo.\nWHY:\nROW: {id}\nROW TEXT: I am going to town tomorrow.\n"
        );
        std::fs::write(dir.join("tr.review.txt"), review).unwrap();
        Self(dir)
    }
    fn es(&self) -> String {
        self.0.join("film.es.fdx").to_string_lossy().into_owned()
    }
    fn review(&self) -> String {
        self.0.join("tr.review.txt").to_string_lossy().into_owned()
    }
    fn dialogue(o: &Opened) -> String {
        o.doc.paragraphs().iter().find(|p| p.kind.name() == "Dialogue").unwrap().text()
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_row_decision_survives_a_final_draft_save_of_the_version() {
    let f = Folder::new("aside");
    let mut notes = vec![];
    let mut o = Opened::open(&f.es(), &[&f.review()], &mut notes).unwrap();
    assert!(o.pair().is_made(), "{:?}", o.pair());
    assert!(o.session.reviews[0].unmatched.is_empty(), "{:?}", o.session.reviews[0].unmatched);
    let sel = o.sel();
    o.session.decide(&mut o.doc, sel, 0, 0, Decision::Accept(0)).unwrap();
    o.save().unwrap();
    assert!(std::fs::read_to_string(f.review()).unwrap().contains("STATUS: accepted"));

    // Final Draft saves the version: other bytes, the dialogue still blank
    // on disk. The session only held the decision: set aside, and the
    // decision is back from the review file, on its row.
    let es = std::fs::read_to_string(f.es()).unwrap().replace("standalone=\"no\" ?>", "standalone=\"no\"?>");
    std::fs::write(f.es(), es).unwrap();
    let mut notes = vec![];
    let o = Opened::open(&f.es(), &[], &mut notes).unwrap();
    assert!(notes.iter().any(|n| n.starts_with("film.es.fdx changed outside Scriptorium")), "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("held only decisions")), "{notes:?}");
    assert_eq!(Folder::dialogue(&o), "Mañana voy al pueblo.");
    assert_eq!(o.session.status(&o.doc, 0, 0), Status::Accepted(0));
}

#[test]
fn a_broken_links_file_still_opens_and_row_suggestions_say_why() {
    let f = Folder::new("broken");
    let at = f.0.join("film.es.links.txt");
    let text = std::fs::read_to_string(&at).unwrap();
    let broken = text.replacen("1 | 1", "0 | 1", 1);
    assert_ne!(broken, text);
    std::fs::write(&at, &broken).unwrap();

    let mut notes = vec![];
    let o = Opened::open(&f.es(), &[&f.review()], &mut notes).unwrap();
    assert!(notes.iter().any(|n| n.contains("film.es.links.txt names film.es.fdx but can't be read")), "{notes:?}");
    assert!(matches!(o.pair(), PairState::Unusable { .. }) && o.pair().maybe_made());
    let unmatched = &o.session.reviews[0].unmatched;
    assert_eq!(unmatched.len(), 1);
    let Some(Unplaced::NoRow(why)) = &unmatched[0].why else { panic!("{unmatched:?}") };
    assert!(why.starts_with("the links file can't be used: film.es.links.txt names film.es.fdx"), "{why}");
    assert_eq!(std::fs::read_to_string(&at).unwrap(), broken, "never rewritten");
}

#[test]
fn the_other_member_missing_still_opens() {
    let f = Folder::new("missing");
    std::fs::rename(f.0.join("film.fdx"), f.0.join("renamed.fdx")).unwrap();
    let mut notes = vec![];
    let o = Opened::open(&f.es(), &[&f.review()], &mut notes).unwrap();
    assert!(notes.iter().any(|n| n.contains("film.fdx is missing")), "{notes:?}");
    assert!(o.pair().maybe_made());
    let Some(Unplaced::NoRow(why)) = &o.session.reviews[0].unmatched[0].why else { panic!() };
    assert!(why.contains("film.fdx is missing"), "{why}");
}
