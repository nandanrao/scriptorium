//! Writing the version Scriptorium made over its own file (plan step A7b,
//! `fsio::versions::write_in_place`): progress into the review files, the
//! `.fdx`, the links file, a fresh session; refused when the pair changed
//! outside, or while a suggestion's line was split, joined or removed.

use std::path::PathBuf;

use scriptorium_core::doc::Pos;
use scriptorium_core::{Group, ParaId, Selection};
use scriptorium_fsio::opened::Opened;
use scriptorium_fsio::versions::{
    InPlaceError, WrittenInPlace, blocked, version_new, write_in_place, write_in_place_with,
};
use scriptorium_review::sessionfile::{self, SessionFile};
use scriptorium_review::{Decision, LineChange, ReviewFile, Status};
use scriptorium_versions::RowStatus;
use scriptorium_versions::links::{self, Links, Stamp};

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
    <Paragraph Type="Dialogue">
      <Text>Take me with you.</Text>
    </Paragraph>
  </Content>
</FinalDraft>
"#;

/// One `ROW:` entry: `before` on the version's side, `option`, the
/// original's text.
fn entry(id: &str, before: &str, option: &str, row_text: &str) -> String {
    format!("\n---\nBEFORE: {before}\nAFTER: {option}\nWHY:\nROW: {id}\nROW TEXT: {row_text}\n")
}

/// A folder with `film.fdx`, its version `film.es.fdx` (`version new`), and
/// `tr.review.txt` with a `ROW:` suggestion for each blank line (#0 the
/// first action, #1 the first dialogue, #2 the second action, #3 the second
/// dialogue); removed on drop.
struct Folder(PathBuf, String);

impl Folder {
    fn new(name: &str) -> Self {
        Self::with(name, "")
    }

    /// With `more` (entries) after the four.
    fn with(name: &str, more: &str) -> Self {
        Self::with_script(name, SCRIPT, more)
    }

    /// The original `script` (lines as in [`SCRIPT`], cues aside).
    fn with_script(name: &str, script: &str, more: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("scriptorium-in-place-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        std::fs::write(dir.join("film.fdx"), script).unwrap();
        version_new(&dir.join("film.fdx"), "es", Some("en"), None, &mut vec![]).unwrap();
        let id = links::parse(&std::fs::read_to_string(dir.join("film.es.links.txt")).unwrap()).unwrap().id;
        let mut review = "SCRIPT REVIEW v1\nScript: film.es.fdx\nId: tr-1\n".to_string();
        review += &entry(&id, "[empty]", "Laura sonríe.", "Laura smiles.");
        review += &entry(&id, "[empty]", "Mañana voy al pueblo.", "I am going to town tomorrow.");
        review += &entry(&id, "[empty]", "Laura ríe.", "Laura laughs.");
        review += &entry(&id, "[empty]", "Llévame contigo.", "Take me with you.");
        review += &more.replace("{id}", &id);
        std::fs::write(dir.join("tr.review.txt"), review).unwrap();
        // Attach it.
        let mut o = Opened::open(
            &dir.join("film.es.fdx").to_string_lossy(),
            &[&dir.join("tr.review.txt").to_string_lossy()],
            &mut vec![],
        )
        .unwrap();
        o.save().unwrap();
        Self(dir, id)
    }
    fn at(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn es(&self) -> String {
        self.at("film.es.fdx").to_string_lossy().into_owned()
    }
    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.at(name)).unwrap()
    }
    fn links(&self) -> Links {
        links::parse(&self.read("film.es.links.txt")).unwrap()
    }
    fn open(&self) -> Opened {
        let mut notes = vec![];
        Opened::open(&self.es(), &[], &mut notes).unwrap_or_else(|e| panic!("{e}"))
    }
    /// Every file write_in_place writes, as it is now.
    fn files(&self) -> Vec<String> {
        ["film.es.fdx", "film.es.links.txt", "tr.review.txt", "film.es.scriptorium.json"]
            .iter()
            .map(|n| self.read(n))
            .collect()
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn decide(o: &mut Opened, i: usize, d: Decision) {
    let sel = o.sel();
    o.session.decide(&mut o.doc, sel, 0, i, d).unwrap();
}

/// What the app does: save the session, then write in place.
fn write(o: &mut Opened) -> Result<WrittenInPlace, InPlaceError> {
    o.save().unwrap();
    write_in_place(&o.dir.join(&o.script_name), &o.raw)
}

/// Remove paragraph `id` (with nothing else changed).
fn remove(o: &mut Opened, id: u64) {
    let s = o.sel();
    o.doc
        .edit(s, Group::Other, |e| {
            e.remove_para(ParaId(id))?;
            Ok((Selection::cursor(Pos::new(ParaId(0), 0)), ()))
        })
        .unwrap();
}

fn item_on(o: &Opened, id: u64) -> usize {
    o.session.reviews[0].items.iter().position(|it| it.para == ParaId(id)).unwrap()
}

fn labels(o: &Opened) -> Vec<String> {
    (0..o.session.reviews[0].items.len())
        .map(|i| {
            let entry = o.session.reviews[0].items[i].entry;
            format!("{entry}: {}", o.session.status_label(&o.doc, 0, i))
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The row statuses as the pair lays them out, row by row.
fn statuses(o: &Opened) -> Vec<RowStatus> {
    o.lay_out_pair().unwrap().rows.iter().map(|r| r.status).collect()
}

#[test]
fn accepted_rows_are_written_and_stay_decided_after_reopening() {
    let f = Folder::new("accept");
    let mut o = f.open();
    assert!(o.session.reviews[0].unmatched.is_empty(), "{:?}", o.session.reviews[0].unmatched);
    decide(&mut o, 0, Decision::Accept(0));
    decide(&mut o, 1, Decision::Accept(0));
    decide(&mut o, 2, Decision::KeepMine);
    // #3 stays pending.
    let before = labels(&o);
    let rows_before = statuses(&o);

    let done = write(&mut o).unwrap();
    assert_eq!(done.message, "Wrote film.es.fdx. Undo starts again from here.");
    let es = f.read("film.es.fdx");
    assert_eq!(done.hash, sessionfile::hash(&es));
    assert!(es.contains("Laura sonríe.") && es.contains("Mañana voy al pueblo."), "{es}");
    assert!(!es.contains("Llévame contigo."), "a pending suggestion isn't applied");

    // The links file: the new hash, ids as positions, `=` stamps on the rows
    // in sync, the pending row still a placeholder.
    let l = f.links();
    assert_eq!(l.members[1].hash, done.hash);
    let ids: Vec<u64> = l.rows.iter().flat_map(|r| r.cells[1].iter().map(|p| p.0)).collect();
    assert_eq!(ids, (0..7).collect::<Vec<_>>());
    let stamp = |id: u64| l.paras[1][&ParaId(id)].stamp.clone();
    assert_eq!(stamp(1), Some(Stamp::Synced("Laura sonríe.".into())));
    assert_eq!(stamp(3), Some(Stamp::Synced("Mañana voy al pueblo.".into())));
    assert!(matches!(stamp(6), Some(Stamp::Placeholder(_))), "{:?}", stamp(6));

    // The review file: progress, and PARA at the new positions.
    let file = ReviewFile::parse(&f.read("tr.review.txt"));
    let paras: Vec<Vec<u64>> = file.entries.iter().map(|e| e.para.iter().map(|p| p.0).collect()).collect();
    assert_eq!(paras, vec![vec![1], vec![3], vec![4], vec![6]]);
    assert!(f.read("tr.review.txt").contains("STATUS: accepted"));

    // A fresh session for the new bytes.
    let saved = SessionFile::from_json(&f.read("film.es.scriptorium.json")).unwrap();
    assert!(saved.is_for(&es));
    assert!(saved.events.is_empty(), "undo starts again from here");

    // Reopened: every decided and pending suggestion placed, the same
    // statuses, the rows read the same.
    let mut o = f.open();
    assert!(o.session.reviews[0].unmatched.is_empty(), "{:?}", o.session.reviews[0].unmatched);
    assert_eq!(labels(&o), before);
    assert_eq!(statuses(&o), rows_before);
    assert_eq!(statuses(&o)[1], RowStatus::InSync);
    assert_eq!(statuses(&o)[6], RowStatus::Blank);
    let i = (0..4).find(|&i| o.session.reviews[0].items[i].entry == 3).unwrap();
    assert_eq!(o.session.status(&o.doc, 0, i), Status::Pending);

    // Undecide after the write: the blank comes back, the row reads blank.
    let i = (0..4).find(|&i| o.session.reviews[0].items[i].entry == 0).unwrap();
    decide(&mut o, i, Decision::Undecided);
    assert_eq!(o.doc.paragraphs()[1].text(), "");
    assert_eq!(statuses(&o)[1], RowStatus::Blank);
    let row = &o.lay_out_pair().unwrap().rows[1];
    assert!(!row.needs, "the suggestion, pending again, still reads the original");
}

#[test]
fn entries_an_agent_appended_just_before_the_write_are_kept() {
    let f = Folder::new("append");
    let mut o = f.open();
    decide(&mut o, 0, Decision::Accept(0));
    o.save().unwrap();
    // An agent appends after the app's last save.
    let mut review = f.read("tr.review.txt");
    review += &entry(&f.1, "LAURA", "LAURA (V.O.)", "LAURA");
    std::fs::write(f.at("tr.review.txt"), &review).unwrap();

    write_in_place(&f.at("film.es.fdx"), &o.raw).unwrap();
    let file = ReviewFile::parse(&f.read("tr.review.txt"));
    assert_eq!(file.entries.len(), 5);
    assert_eq!(file.entries[4].para.iter().map(|p| p.0).collect::<Vec<_>>(), vec![5]);
    let o = f.open();
    assert!(o.session.reviews[0].unmatched.is_empty(), "{:?}", o.session.reviews[0].unmatched);
    assert_eq!(o.session.reviews[0].items.len(), 5);
    let i = (0..5).find(|&i| o.session.reviews[0].items[i].entry == 0).unwrap();
    assert_eq!(o.session.status(&o.doc, 0, i), Status::Accepted(0));
}

#[test]
fn a_not_found_row_entry_stays_not_found_and_loses_its_stale_para() {
    let more = "\n---\nBEFORE: [empty]\nPARA: 1\nAFTER: Otra cosa.\nWHY:\nROW: {id}\nROW TEXT: Something else.\n";
    let f = Folder::with("not-found", more);
    let mut o = f.open();
    assert_eq!(o.session.reviews[0].unmatched.len(), 1);
    decide(&mut o, 0, Decision::Accept(0));
    write(&mut o).unwrap();
    let last = ReviewFile::parse(&f.read("tr.review.txt")).entries.pop().unwrap();
    assert!(last.para.is_empty(), "{last:?}");
    assert_eq!(last.row.as_ref().unwrap().texts, vec!["Something else.".to_string()]);
    let o = f.open();
    assert_eq!(o.session.reviews[0].unmatched.len(), 1);
    assert_eq!(o.session.reviews[0].unmatched[0].entry, 4);
}

#[test]
fn an_unmatched_entry_is_not_moved_onto_a_shifted_line_by_its_para() {
    // Not found (its BEFORE isn't there; ¶4 is a blank action). After ¶2 is
    // removed and the file written, position 4 is the cue LAURA, which reads
    // the entry's option: a stale PARA would place it there as decided.
    let more = "\n---\nBEFORE: Nobody says this.\nPARA: 4\nAFTER: LAURA\nWHY:\n";
    let f = Folder::with("shifted", more);
    let mut o = f.open();
    assert_eq!(o.session.reviews[0].unmatched.len(), 1);
    remove(&mut o, 2);
    write(&mut o).unwrap();
    assert_eq!(f.open().doc.paragraphs()[4].text(), "LAURA");
    let last = ReviewFile::parse(&f.read("tr.review.txt")).entries.pop().unwrap();
    assert!(last.para.is_empty(), "{last:?}");
    let o = f.open();
    assert_eq!(o.session.reviews[0].unmatched.len(), 1);
    assert_eq!(o.session.reviews[0].unmatched[0].entry, 4);
}

#[test]
fn refused_when_the_pair_or_the_session_changed_outside() {
    let f = Folder::new("guard");
    let mut o = f.open();
    decide(&mut o, 0, Decision::Accept(0));
    o.save().unwrap();

    // Final Draft saved the version since it was opened.
    let es = f.read("film.es.fdx");
    std::fs::write(f.at("film.es.fdx"), es.replace("standalone=\"no\" ?>", "standalone=\"no\"?>")).unwrap();
    let files = f.files();
    let e = write_in_place(&f.at("film.es.fdx"), &o.raw).unwrap_err();
    assert_eq!(e.to_string(), "film.es.fdx changed since you opened it. Close it in Final Draft and reopen it here.");
    assert_eq!(f.files(), files, "nothing written");
    std::fs::write(f.at("film.es.fdx"), &es).unwrap();

    // The links file no longer describes the bytes the session was opened on.
    let text = f.read("film.es.links.txt");
    let hash = f.links().members[1].hash.clone();
    std::fs::write(f.at("film.es.links.txt"), text.replace(&hash, "0000000000000000")).unwrap();
    let files = f.files();
    let e = write_in_place(&f.at("film.es.fdx"), &o.raw).unwrap_err();
    assert!(e.to_string().starts_with("film.es.fdx changed since you opened it"), "{e}");
    assert_eq!(f.files(), files, "nothing written");
    std::fs::write(f.at("film.es.links.txt"), &text).unwrap();

    // The session file isn't saved for these bytes.
    let session = f.read("film.es.scriptorium.json");
    std::fs::remove_file(f.at("film.es.scriptorium.json")).unwrap();
    let e = write_in_place(&f.at("film.es.fdx"), &o.raw).unwrap_err();
    assert!(e.to_string().contains("session isn't saved for the file you opened; reload it and try again"), "{e}");
    std::fs::write(f.at("film.es.scriptorium.json"), &session).unwrap();
    write_in_place(&f.at("film.es.fdx"), &o.raw).unwrap();
}

#[test]
fn the_original_changed_since_opening_is_found_again_and_the_write_goes_on() {
    let f = Folder::new("original");
    let mut o = f.open();
    decide(&mut o, 1, Decision::Accept(0));
    // Final Draft saves the English with a line added at the end.
    let en = SCRIPT.replace(
        "  </Content>",
        "    <Paragraph Type=\"Action\">\n      <Text>A dog barks.</Text>\n    </Paragraph>\n  </Content>",
    );
    std::fs::write(f.at("film.fdx"), &en).unwrap();
    let done = write(&mut o).unwrap();
    assert!(done.notes.iter().any(|n| n.starts_with("film.fdx changed outside Scriptorium")), "{:?}", done.notes);
    let l = f.links();
    assert_eq!(l.members[0].hash, sessionfile::hash(&en));
    assert_eq!(l.members[1].hash, done.hash);
    let o = f.open();
    let rows = statuses(&o);
    assert_eq!(rows.len(), 8, "the dog has a row of its own");
    assert_eq!(rows[3], RowStatus::InSync);
    assert_eq!(rows[7], RowStatus::OneSided);
}

/// A pending suggestion on line `para`, split at byte `at` and saved:
/// reopened, the suggestion is still on that line, which reads as split, and
/// Write .fdx refuses, writing nothing.
fn split_refuses(f: &Folder, entry: usize, para: u64, at: usize) {
    let mut o = f.open();
    let unmatched = &o.session.reviews[0].unmatched;
    assert!(!unmatched.iter().any(|u| u.entry == entry), "{unmatched:?}");
    let i = o.session.reviews[0].items.iter().position(|it| it.entry == entry).unwrap();
    assert_eq!(o.session.reviews[0].items[i].para, ParaId(para));
    o.doc.enter(Selection::cursor(Pos::new(ParaId(para), at))).unwrap();
    o.save().unwrap();
    let o = f.open();
    let unmatched = &o.session.reviews[0].unmatched;
    assert!(!unmatched.iter().any(|u| u.entry == entry), "{unmatched:?}");
    assert_eq!(o.session.reviews[0].items[i].para, ParaId(para), "never moved to an identical row");
    assert_eq!(o.session.line_change(&o.doc, 0, i), Some(LineChange::Split));
    let files = f.files();
    let e = write_in_place(&f.at("film.es.fdx"), &o.raw).unwrap_err();
    assert_eq!(e, InPlaceError::Blocked(vec![(0, i, LineChange::Split)]));
    assert_eq!(f.files(), files, "nothing written");
}

#[test]
fn a_pending_suggestion_over_a_split_line_refuses_the_write() {
    // A plain line suggestion, and a `ROW:` one, on the only CESAR.
    let f = Folder::with("split-plain", "\n---\nBEFORE: CESAR\nPARA: 2\nAFTER: CÉSAR\nWHY:\n");
    split_refuses(&f, 4, 2, 2);
    let f =
        Folder::with("split-row", "\n---\nBEFORE: CESAR\nPARA: 2\nAFTER: CÉSAR\nWHY:\nROW: {id}\nROW TEXT: CESAR\n");
    split_refuses(&f, 4, 2, 2);
    assert_eq!(blocked(&[(4, LineChange::Split), (9, LineChange::Joined)]), "Decide or rewrite #4 and #9 first.");
    assert_eq!(
        blocked(&[(2, LineChange::Split), (7, LineChange::Removed)]),
        "Decide or rewrite #2 first. #7's line was removed: undo that first."
    );
}

#[test]
fn a_row_suggestion_on_a_split_line_stays_off_identical_rows() {
    // The cue: the second CESAR's row suggestion, its line split.
    let cue = "\n---\nBEFORE: CESAR\nPARA: 5\nAFTER: CÉSAR\nWHY:\nROW: {id}\nROW TEXT: CESAR\n";
    let f = Folder::with_script("split-cue", &two_cesars(), cue);
    split_refuses(&f, 4, 5, 2);

    // An action: both "Laura smiles." rows typed the same by hand, the
    // suggestion made for the first.
    let action = "\n---\nBEFORE: Laura sonrie y se va.\nPARA: 1\nAFTER: Laura sonríe y se marcha.\nWHY:\nROW: \
                  {id}\nROW TEXT: Laura smiles.\n";
    let f = Folder::with_script("split-action", &SCRIPT.replace("Laura laughs.", "Laura smiles."), "");
    let mut o = f.open();
    for p in [1, 4] {
        o.doc.insert_text(Selection::cursor(Pos::new(ParaId(p), 0)), "Laura sonrie y se va.").unwrap();
    }
    o.save().unwrap();
    let mut review = f.read("tr.review.txt");
    review += &action.replace("{id}", &f.1);
    std::fs::write(f.at("tr.review.txt"), review).unwrap();
    split_refuses(&f, 4, 1, 12);
}

/// The original with the cue LAURA as a second CESAR.
fn two_cesars() -> String {
    SCRIPT.replace("<Text>LAURA</Text>", "<Text>CESAR</Text>")
}

#[test]
fn a_suggestion_whose_line_was_removed_refuses_the_write_pending_or_decided() {
    // A suggestion on the second CESAR; the first reads the same, so the
    // written file could place it there.
    let cue = "\n---\nBEFORE: CESAR\nPARA: 5\nAFTER: CÉSAR (V.O.)\nWHY:\n";
    for accept in [false, true] {
        let f = Folder::with_script(&format!("removed-{accept}"), &two_cesars(), cue);
        let mut o = f.open();
        let i = item_on(&o, 5);
        if accept {
            decide(&mut o, i, Decision::Accept(0));
        }
        remove(&mut o, 5);
        o.save().unwrap();
        let files = f.files();
        let e = write_in_place(&f.at("film.es.fdx"), &o.raw).unwrap_err();
        assert_eq!(e, InPlaceError::Blocked(vec![(0, i, LineChange::Removed)]), "accepted: {accept}");
        assert_eq!(f.files(), files, "nothing written");
    }
}

#[test]
fn the_fdx_write_failing_midway_puts_the_review_files_back() {
    // A suggestion on the second CESAR (¶5); a line added above it moves it
    // to position 6, so the progress written first hints PARA 6, which is
    // nothing in the old file, and the first CESAR (¶2) reads the same.
    let cue = "\n---\nBEFORE: CESAR\nPARA: 5\nAFTER: CÉSAR (V.O.)\nWHY:\n";
    let f = Folder::with_script("fail", &two_cesars(), cue);
    let mut o = f.open();
    decide(&mut o, 0, Decision::Accept(0));
    let at = Pos::new(ParaId(1), 0);
    o.doc.enter(Selection::cursor(at)).unwrap();
    o.save().unwrap();
    let before = labels(&o);
    let files = f.files();

    let fail = |_: &std::path::Path, _: &[u8]| Err(std::io::Error::other("disk full"));
    let e = write_in_place_with(&f.at("film.es.fdx"), &o.raw, &fail).unwrap_err();
    assert!(e.to_string().contains("film.es.fdx wasn't written: disk full"), "{e}");
    assert_eq!(f.files(), files, "every file as it was, the review file put back");

    // The open session goes on: a decision, saved, then the write again.
    let cue_item = item_on(&o, 5);
    decide(&mut o, cue_item, Decision::KeepMine);
    o.save().expect("save after a failed write");
    let reopened = f.open();
    assert_eq!(reopened.session.reviews[0].items[item_on(&reopened, 5)].para, ParaId(5));
    assert_ne!(labels(&reopened), before);
    write(&mut o).expect("the write again");
    let o = f.open();
    assert!(o.session.reviews[0].unmatched.is_empty(), "{:?}", o.session.reviews[0].unmatched);
    assert_eq!(o.session.status_label(&o.doc, 0, item_on(&o, 6)), "kept");
    assert!(!o.doc.paragraphs().iter().any(|p| p.text().contains("V.O.")));
}

#[test]
fn a_crash_after_the_fdx_write_is_found_again_on_the_next_open() {
    let f = Folder::new("crash");
    let mut o = f.open();
    decide(&mut o, 0, Decision::Accept(0));
    decide(&mut o, 1, Decision::Accept(0));
    o.save().unwrap();
    let before = labels(&o);
    // The run stops right after the `.fdx`: links file and session are the old ones.
    let (links_text, session) = (f.read("film.es.links.txt"), f.read("film.es.scriptorium.json"));
    write(&mut o).unwrap();
    std::fs::write(f.at("film.es.links.txt"), &links_text).unwrap();
    std::fs::write(f.at("film.es.scriptorium.json"), &session).unwrap();

    let mut notes = vec![];
    let o = Opened::open(&f.es(), &[], &mut notes).unwrap();
    assert!(notes.iter().any(|n| n.starts_with("film.es.fdx changed outside Scriptorium")), "{notes:?}");
    assert!(o.session.reviews[0].unmatched.is_empty(), "{:?}", o.session.reviews[0].unmatched);
    assert_eq!(labels(&o), before);
    // Re-find gave every line its row back, on the written bytes.
    let l = f.links();
    assert_eq!(l.members[1].hash, sessionfile::hash(&f.read("film.es.fdx")));
    assert_eq!(l.rows.len(), 7);
}

#[test]
fn a_decided_line_retyped_since_stays_on_its_line() {
    let cue = "\n---\nBEFORE: CESAR\nPARA: 5\nAFTER: CÉSAR (V.O.)\nWHY:\n";
    let f = Folder::with_script("retyped", &two_cesars(), cue);
    let mut o = f.open();
    let i = item_on(&o, 5);
    decide(&mut o, i, Decision::Accept(0));
    let len = o.doc.paragraph(ParaId(5)).unwrap().text().len();
    o.doc.insert_text(Selection { anchor: Pos::new(ParaId(5), 0), head: Pos::new(ParaId(5), len) }, "LAURA").unwrap();
    write(&mut o).unwrap();
    let o = f.open();
    let texts: Vec<String> = o.doc.paragraphs().iter().map(|p| p.text()).collect();
    assert_eq!(texts[2], "CESAR");
    assert_eq!(texts[5], "LAURA");
    assert_eq!(o.session.status_label(&o.doc, 0, item_on(&o, 5)), "rewritten");
}

#[test]
fn the_links_file_not_written_after_the_fdx_says_so_as_written() {
    let f = Folder::new("incomplete");
    let mut o = f.open();
    decide(&mut o, 0, Decision::Accept(0));
    o.save().unwrap();
    // The links file is a link: never written through, so its write fails
    // after the `.fdx` was written.
    std::fs::rename(f.at("film.es.links.txt"), f.at("links.data")).unwrap();
    std::os::unix::fs::symlink(f.at("links.data"), f.at("film.es.links.txt")).unwrap();
    let e = write_in_place(&f.at("film.es.fdx"), &o.raw).unwrap_err();
    let InPlaceError::WrittenButIncomplete { path, message } = &e else { panic!("{e:?}") };
    assert_eq!(*path, f.at("film.es.fdx"));
    assert!(message.starts_with("Wrote film.es.fdx, but not film.es.links.txt"), "{message}");
    assert!(!message.contains("Final Draft"));
    assert!(f.read("film.es.fdx").contains("Laura sonríe."));
    // The failure passes; reopened: found again, the decision back from
    // the review file.
    std::fs::remove_file(f.at("film.es.links.txt")).unwrap();
    std::fs::rename(f.at("links.data"), f.at("film.es.links.txt")).unwrap();
    let mut notes = vec![];
    let o = Opened::open(&f.es(), &[], &mut notes).unwrap();
    assert!(notes.iter().any(|n| n.starts_with("film.es.fdx changed outside Scriptorium")), "{notes:?}");
    let i = (0..4).find(|&i| o.session.reviews[0].items[i].entry == 0).unwrap();
    assert_eq!(o.session.status(&o.doc, 0, i), Status::Accepted(0));
}

#[test]
fn a_review_that_cant_be_written_refuses_even_for_para_hints_only() {
    let f = Folder::new("unwritable");
    // Attached by the writer, but without `SCRIPT REVIEW`: read-only here.
    std::fs::write(f.at("notes.review.txt"), "Script: film.es.fdx\n\n---\nBEFORE: CESAR\nPARA: 2\nAFTER: CÉSAR\n")
        .unwrap();
    let mut o = Opened::open(&f.es(), &[&f.at("notes.review.txt").to_string_lossy()], &mut vec![]).unwrap();
    assert_eq!(o.session.reviews[1].items.len(), 1);
    // A line added above: only its PARA hint would change.
    o.doc.enter(Selection::cursor(Pos::new(ParaId(0), 0))).unwrap();
    o.save().unwrap();
    let files = f.files();
    let e = write_in_place(&f.at("film.es.fdx"), &o.raw).unwrap_err();
    let msg = e.to_string();
    assert!(msg.starts_with("notes.review.txt must be updated with film.es.fdx (where its suggestions are)"), "{msg}");
    assert!(
        msg.contains("Add `SCRIPT REVIEW v1` as its first line, or move it into the folder of film.es.fdx"),
        "{msg}"
    );
    assert_eq!(f.files(), files, "nothing written");
}

/// The session's "Lined up on the left" choice (`links`, plan B1) is kept
/// by a save and by the fresh session written in place.
#[test]
fn the_left_choice_survives_a_write_in_place() {
    let f = Folder::new("links-field");
    let mut o = f.open();
    assert_eq!(o.links, None);
    o.links = Some("none".into());
    decide(&mut o, 0, Decision::Accept(0));
    write(&mut o).unwrap();
    let saved = SessionFile::from_json(&f.read("film.es.scriptorium.json")).unwrap();
    assert_eq!(saved.links.as_deref(), Some("none"));
    assert_eq!(f.open().links.as_deref(), Some("none"));
}
