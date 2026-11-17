//! Opening a script whose bytes changed since its session was saved (Final
//! Draft saved it): the session is set aside with a note when it holds only
//! decisions the review files have, and refused otherwise. And progress
//! written into the review files on every save (plan C2), which is what
//! makes the first case the common one.

use std::path::PathBuf;

use scriptorium_core::{ParaId, Pos, Selection};
use scriptorium_fsio::opened::Opened;
use scriptorium_review::Decision;

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

const REVIEW: &str = "SCRIPT REVIEW v1
Script: film.fdx

---
BEFORE: I am go to the town tomorrow.
AFTER: I am going to town tomorrow.
WHY: verb form
";

/// A fresh folder holding `film.fdx` and `film.review.txt`; removed on drop.
struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("scriptorium-fsio-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
        std::fs::write(dir.join("film.review.txt"), REVIEW).unwrap();
        Self(dir)
    }
    fn script(&self) -> String {
        self.0.join("film.fdx").to_string_lossy().into_owned()
    }
    fn review(&self) -> String {
        self.0.join("film.review.txt").to_string_lossy().into_owned()
    }
    /// With `made`, a links file naming `film.fdx` as the version made here;
    /// without, none.
    fn made(&self, made: bool) {
        let at = self.0.join("film.links.txt");
        if made {
            std::fs::write(
                &at,
                "SCRIPTORIUM LINKS v1\nId: lk-test\na: original.fdx\na hash: 0000000000000000\nb: film.fdx\n\
                 b hash: 0000000000000000\nb created: version new\n---\n",
            )
            .unwrap();
        } else {
            let _ = std::fs::remove_file(at);
        }
    }
    fn session(&self) -> PathBuf {
        self.0.join("film.scriptorium.json")
    }
    fn set_aside(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("film.scriptorium.set-aside-"))
            .collect();
        names.sort();
        names
    }
    /// What Final Draft does on save: the same script, other bytes. With
    /// `line_above`, a new line before the dialogue, so every id after it moves.
    fn final_draft_save(&self, line_above: bool) {
        let mut s = SCRIPT.replace("standalone=\"no\" ?>", "standalone=\"no\"?>");
        if line_above {
            s = s.replace(
                "    <Paragraph Type=\"Dialogue\">",
                "    <Paragraph Type=\"Parenthetical\">\n      <Text>(quietly)</Text>\n    </Paragraph>\n    \
                 <Paragraph Type=\"Dialogue\">",
            );
        }
        std::fs::write(self.0.join("film.fdx"), s).unwrap();
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Open with the review attached, accept its suggestion, save. `made`: a
/// links file names `film.fdx` as the version Scriptorium made, so saves
/// write progress into the review.
fn accept(f: &Folder, made: bool) -> Opened {
    f.made(made);
    let mut notes = vec![];
    let mut o = Opened::open(&f.script(), &[&f.review()], &mut notes).unwrap();
    assert_eq!(o.pair().map(|p| p.is_made), made.then_some(true));
    let sel = o.sel();
    o.session.decide(&mut o.doc, sel, 0, 0, Decision::Accept(0)).unwrap();
    o.save().unwrap();
    o
}

fn dialogue(o: &Opened) -> String {
    o.doc.paragraphs().iter().find(|p| p.kind.name() == "Dialogue").unwrap().text()
}

#[test]
fn progress_is_written_into_the_review_on_save() {
    let f = Folder::new("progress");
    accept(&f, true);
    let text = std::fs::read_to_string(f.review()).unwrap();
    assert!(text.contains("STATUS: accepted"), "{text}");
    // The session records the new text, so the next open sees no outside change.
    let mut notes = vec![];
    let o = Opened::open(&f.script(), &[], &mut notes).unwrap();
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(dialogue(&o), "I am going to town tomorrow.");

    // Without it (any script but a made version), the file is left alone.
    let g = Folder::new("no-progress");
    accept(&g, false);
    let text = std::fs::read_to_string(g.review()).unwrap();
    assert!(!text.contains("STATUS"), "{text}");
}

#[test]
fn a_session_with_only_decisions_is_set_aside_and_they_come_back() {
    for line_above in [false, true] {
        let f = Folder::new(&format!("aside-{line_above}"));
        accept(&f, true);
        let saved = std::fs::read_to_string(f.session()).unwrap();
        f.final_draft_save(line_above);

        let mut notes = vec![];
        let mut o = Opened::open(&f.script(), &[], &mut notes).unwrap();
        assert!(notes.iter().any(|n| n.contains("held only decisions")), "{notes:?}");
        // Back from the review file, on the new bytes (with `line_above`,
        // on another id: `@para` differences don't count).
        assert_eq!(dialogue(&o), "I am going to town tomorrow.");
        assert_eq!(o.session.progress(&o.doc, 0).accepted, 1);
        assert!(f.set_aside().is_empty(), "nothing moves until a save");

        o.save().unwrap();
        let aside = f.set_aside();
        assert_eq!(aside.len(), 1, "{aside:?}");
        assert_eq!(std::fs::read_to_string(f.0.join(&aside[0])).unwrap(), saved);
        let mut notes = vec![];
        Opened::open(&f.script(), &[], &mut notes).unwrap();
        assert!(notes.is_empty(), "the new session fits: {notes:?}");
    }
}

#[test]
fn typing_in_the_session_is_refused() {
    let f = Folder::new("typing");
    let mut o = accept(&f, true);
    let laura = Selection::cursor(Pos::new(ParaId(1), "Laura smiles".len()));
    o.doc.insert_text(laura, " and waves").unwrap();
    o.save().unwrap();
    f.final_draft_save(false);
    let before = std::fs::read_to_string(f.session()).unwrap();

    let err = Opened::open(&f.script(), &[], &mut vec![]).err().unwrap();
    assert!(err.contains("holds typing"), "{err}");
    assert!(err.contains("Scriptorium app"), "{err}");
    assert_eq!(std::fs::read_to_string(f.session()).unwrap(), before, "left as it was");
    assert!(f.set_aside().is_empty());
    // Read-only opens still show the script as written.
    let mut notes = vec![];
    Opened::open_read_only(&f.script(), &mut notes).unwrap();
}

#[test]
fn typing_undone_holds_nothing() {
    let f = Folder::new("undone");
    let mut o = accept(&f, true);
    let laura = Selection::cursor(Pos::new(ParaId(1), "Laura smiles".len()));
    o.doc.insert_text(laura, "!").unwrap();
    o.doc.undo();
    o.save().unwrap();
    f.final_draft_save(false);
    Opened::open(&f.script(), &[], &mut vec![]).unwrap();
}

#[test]
fn decisions_not_in_the_review_files_are_refused() {
    let f = Folder::new("unwritten");
    accept(&f, false);
    f.final_draft_save(false);
    let err = Opened::open(&f.script(), &[], &mut vec![]).err().unwrap();
    assert!(err.contains("changed outside Scriptorium"), "{err}");
}

#[test]
fn a_session_changed_meanwhile_is_not_set_aside() {
    let f = Folder::new("meanwhile");
    accept(&f, true);
    f.final_draft_save(false);
    let mut o = Opened::open(&f.script(), &[], &mut vec![]).unwrap();
    std::fs::write(f.session(), "{}").unwrap();
    let err = o.save().unwrap_err();
    assert!(err.contains("changed while this command ran"), "{err}");
    assert_eq!(std::fs::read_to_string(f.session()).unwrap(), "{}");
    assert!(f.set_aside().is_empty());
}

fn open(f: &Folder) -> Result<Opened, String> {
    Opened::open(&f.script(), &[], &mut vec![])
}

#[test]
fn a_rewrite_not_in_the_review_is_refused() {
    let f = Folder::new("rewrite");
    f.made(true);
    let mut o = Opened::open(&f.script(), &[&f.review()], &mut vec![]).unwrap();
    let sel = o.sel();
    o.session.decide(&mut o.doc, sel, 0, 0, Decision::Rewrite("I am off to the city.".into())).unwrap();
    o.save().unwrap();
    assert!(std::fs::read_to_string(f.review()).unwrap().contains("MINE: I am off to the city."));
    // Rewritten again, in a save that writes no progress.
    f.made(false);
    let mut o = open(&f).unwrap();
    let sel = o.sel();
    o.session.decide(&mut o.doc, sel, 0, 0, Decision::Undecided).unwrap();
    o.session.decide(&mut o.doc, sel, 0, 0, Decision::Rewrite("I am off to the coast.".into())).unwrap();
    o.save().unwrap();
    f.final_draft_save(false);
    let err = open(&f).err().unwrap();
    assert!(err.contains("decisions that aren't saved in a review file"), "{err}");

    // A rewrite in a session whose progress is written, undecided and
    // rewritten in the same session, is set aside.
    let g = Folder::new("rewrite-saved");
    g.made(true);
    let mut o = Opened::open(&g.script(), &[&g.review()], &mut vec![]).unwrap();
    o.save().unwrap();
    let mut o = open(&g).unwrap();
    let sel = o.sel();
    o.session.decide(&mut o.doc, sel, 0, 0, Decision::Rewrite("I am off to the city.".into())).unwrap();
    o.session.decide(&mut o.doc, sel, 0, 0, Decision::Undecided).unwrap();
    o.session.decide(&mut o.doc, sel, 0, 0, Decision::Rewrite("I am off to the coast.".into())).unwrap();
    o.save().unwrap();
    g.final_draft_save(true);
    let o = open(&g).unwrap();
    assert_eq!(dialogue(&o), "I am off to the coast.");

    // Rewritten again in a session that began from the file's rewrite (its
    // starting point, not its own history), even with progress: the earlier
    // rewrite's text is gone from the file, so the session can't be checked.
    let h = Folder::new("rewrite-twice");
    h.made(true);
    let mut o = Opened::open(&h.script(), &[&h.review()], &mut vec![]).unwrap();
    let sel = o.sel();
    o.session.decide(&mut o.doc, sel, 0, 0, Decision::Rewrite("I am off to the city.".into())).unwrap();
    o.save().unwrap();
    // A new session: the review's decisions are its starting point.
    std::fs::remove_file(h.session()).unwrap();
    Opened::open(&h.script(), &[&h.review()], &mut vec![]).unwrap().save().unwrap();
    let mut o = open(&h).unwrap();
    let sel = o.sel();
    o.session.decide(&mut o.doc, sel, 0, 0, Decision::Undecided).unwrap();
    o.session.decide(&mut o.doc, sel, 0, 0, Decision::Rewrite("I am off to the coast.".into())).unwrap();
    o.save().unwrap();
    h.final_draft_save(false);
    assert!(open(&h).is_err());
}

#[test]
fn a_rewrite_typed_in_place_counts_as_its_decision() {
    // The app's rewrite: typing in the line, then the decision recorded from
    // its text, merged into one undo step (`finish_rewrite`).
    let f = Folder::new("in-place");
    f.made(true);
    let mut o = Opened::open(&f.script(), &[&f.review()], &mut vec![]).unwrap();
    let depth = o.doc.undo_depth();
    let end = Selection::cursor(Pos::new(ParaId(3), "I am go to the town tomorrow".len()));
    let sel = o.doc.insert_text(end, " at dawn").unwrap();
    o.session.decide_from_text(&mut o.doc, sel, 0, 0).unwrap();
    o.doc.merge_undo_since(depth);
    o.save().unwrap();
    f.final_draft_save(false);
    let o = open(&f).unwrap();
    assert_eq!(dialogue(&o), "I am go to the town tomorrow at dawn.");

    // A line split elsewhere, merged into the same step, is typing.
    let g = Folder::new("in-place-split");
    g.made(true);
    let mut o = Opened::open(&g.script(), &[&g.review()], &mut vec![]).unwrap();
    let depth = o.doc.undo_depth();
    let laura = Selection::cursor(Pos::new(ParaId(1), "Laura".len()));
    o.doc.insert_text(laura, "\n").unwrap();
    let sel = o.sel();
    o.session.decide(&mut o.doc, sel, 0, 0, Decision::Accept(0)).unwrap();
    o.doc.merge_undo_since(depth);
    o.save().unwrap();
    g.final_draft_save(false);
    let err = open(&g).err().unwrap();
    assert!(err.contains("typing that isn't in any file"), "{err}");
}

#[test]
fn progress_is_written_only_where_it_can_be() {
    // Unchanged progress: the file isn't rewritten.
    let f = Folder::new("unchanged");
    accept(&f, true);
    let text = std::fs::read_to_string(f.review()).unwrap();
    let mut o = open(&f).unwrap();
    std::fs::write(f.review(), format!("{text}\n")).unwrap();
    o.session.reviews[0].wrote_progress(format!("{text}\n")).unwrap();
    o.save().unwrap();
    assert_eq!(std::fs::read_to_string(f.review()).unwrap(), format!("{text}\n"));

    // Changed on disk since it was read (an agent appended): refused.
    let g = Folder::new("appended");
    g.made(true);
    // Attached (and its id pinned) first.
    Opened::open(&g.script(), &[&g.review()], &mut vec![]).unwrap().save().unwrap();
    let mut o = open(&g).unwrap();
    let sel = o.sel();
    o.session.decide(&mut o.doc, sel, 0, 0, Decision::Accept(0)).unwrap();
    let pinned = std::fs::read_to_string(g.review()).unwrap();
    let appended = format!("{pinned}\n---\nBEFORE: Laura smiles.\nAFTER: Laura grins.\n");
    std::fs::write(g.review(), &appended).unwrap();
    let err = o.save().unwrap_err();
    assert!(err.contains("changed on disk"), "{err}");
    assert_eq!(std::fs::read_to_string(g.review()).unwrap(), appended);

    // A review outside the script's folder: left alone, the session saved.
    let h = Folder::new("outside");
    h.made(true);
    let outside = std::env::temp_dir().join(format!("scriptorium-fsio-outside-{}.review.txt", std::process::id()));
    std::fs::write(&outside, REVIEW).unwrap();
    let mut o = Opened::open(&h.script(), &[outside.to_str().unwrap()], &mut vec![]).unwrap();
    let sel = o.sel();
    o.session.decide(&mut o.doc, sel, 0, 0, Decision::Accept(0)).unwrap();
    o.save().unwrap();
    assert!(!std::fs::read_to_string(&outside).unwrap().contains("STATUS"));
    std::fs::remove_file(outside).ok();
}

#[test]
fn a_links_file_listed_as_a_review_is_left_out() {
    let f = Folder::new("listed-links");
    accept(&f, true);
    let json = std::fs::read_to_string(f.session()).unwrap();
    std::fs::write(f.session(), json.replace("\"film.review.txt\"", "\"film.links.txt\"")).unwrap();
    let mut notes = vec![];
    let o = Opened::open(&f.script(), &[], &mut notes).unwrap();
    assert!(o.session.reviews.is_empty());
    assert!(notes.iter().any(|n| n.contains("is a links file")), "{notes:?}");
}

#[test]
fn an_old_session_with_nothing_in_it_is_set_aside() {
    let f = Folder::new("v1-empty");
    std::fs::write(f.session(), r#"{"version":1,"script_hash":"0000000000000000","events":[]}"#).unwrap();
    let mut notes = vec![];
    let mut o = Opened::open(&f.script(), &[], &mut notes).unwrap();
    assert!(notes.iter().any(|n| n.contains("held only decisions")), "{notes:?}");
    o.save().unwrap();
    assert_eq!(f.set_aside().len(), 1);
}

/// From the verifier: the second rewrite starts with the first's text, so
/// undoing the session's typing meets every edit; only where it ends (not
/// the line's text when the session began) tells.
#[test]
fn coincident_retyping_after_undecide() {
    let f = Folder::new("atk1");
    f.made(true);
    let mut o = Opened::open(&f.script(), &[&f.review()], &mut vec![]).unwrap();
    let depth = o.doc.undo_depth();
    let sel = o.doc.insert_text(Selection::cursor(Pos::new(ParaId(3), 0)), "X").unwrap();
    o.session.decide_from_text(&mut o.doc, sel, 0, 0).unwrap();
    o.doc.merge_undo_since(depth);
    o.save().unwrap();
    assert!(std::fs::read_to_string(f.review()).unwrap().contains("MINE: XI am go"));
    // No progress from here on.
    f.made(false);
    let mut o = open(&f).unwrap();
    let sel = o.sel();
    o.session.decide(&mut o.doc, sel, 0, 0, Decision::Undecided).unwrap();
    assert_eq!(dialogue(&o), "I am go to the town tomorrow.");
    let depth = o.doc.undo_depth();
    let sel = o.doc.insert_text(Selection::cursor(Pos::new(ParaId(3), 0)), "XI").unwrap();
    o.session.decide_from_text(&mut o.doc, sel, 0, 0).unwrap();
    o.doc.merge_undo_since(depth);
    o.save().unwrap();
    f.final_draft_save(false);
    let err = open(&f).err().expect("refused, not set aside");
    assert!(err.contains("decisions that aren't saved"), "{err}");
}

/// From the verifier: typing merged into an accept's undo step, on the
/// accepted line itself.
#[test]
fn accept_plus_typing_same_group() {
    let f = Folder::new("atk2");
    f.made(true);
    let mut o = Opened::open(&f.script(), &[&f.review()], &mut vec![]).unwrap();
    let sel = o.sel();
    o.session.decide(&mut o.doc, sel, 0, 0, Decision::Accept(0)).unwrap();
    o.save().unwrap();
    f.made(false);
    let mut o = open(&f).unwrap();
    let sel = o.sel();
    let depth = o.doc.undo_depth();
    o.session.decide(&mut o.doc, sel, 0, 0, Decision::Undecided).unwrap();
    o.session.decide(&mut o.doc, sel, 0, 0, Decision::Accept(0)).unwrap();
    o.doc.insert_text(Selection::cursor(Pos::new(ParaId(3), 0)), "Well, ").unwrap();
    o.doc.merge_undo_since(depth);
    o.save().unwrap();
    f.final_draft_save(false);
    let err = open(&f).err().expect("refused, not set aside");
    assert!(err.contains("changed outside Scriptorium"), "{err}");
}

#[test]
fn a_broken_links_file_naming_the_script_keeps_progress_on() {
    let f = Folder::new("broken-links");
    std::fs::write(f.0.join("film.links.txt"), "SCRIPTORIUM LINKS v1\nId: lk-x\na: original.fdx\nb: film.fdx\n---\n")
        .unwrap();
    let mut notes = vec![];
    let mut o = Opened::open(&f.script(), &[&f.review()], &mut notes).unwrap();
    assert!(o.pair().is_none());
    assert!(notes.iter().any(|n| n.contains("film.links.txt names film.fdx but can't be read")), "{notes:?}");
    let sel = o.sel();
    o.session.decide(&mut o.doc, sel, 0, 0, Decision::Accept(0)).unwrap();
    o.save().unwrap();
    assert!(std::fs::read_to_string(f.review()).unwrap().contains("STATUS: accepted"));
}

#[test]
fn of_several_links_files_the_made_one_is_used() {
    let f = Folder::new("several-links");
    let links = |made: &str| {
        format!(
            "SCRIPTORIUM LINKS v1\nId: lk-x\na: original.fdx\na hash: 0000000000000000\nb: film.fdx\n\
             b hash: 0000000000000000\n{made}---\n"
        )
    };
    std::fs::write(f.0.join("a.links.txt"), links("")).unwrap();
    std::fs::write(f.0.join("b.links.txt"), links("b created: version new\n")).unwrap();
    let mut notes = vec![];
    let o = Opened::open(&f.script(), &[], &mut notes).unwrap();
    let pair = o.pair().unwrap();
    assert!(pair.is_made && pair.links.ends_with("b.links.txt"), "{pair:?}");
    assert!(notes.is_empty(), "{notes:?}");

    // Two that say the same: the first by name, with a note.
    std::fs::write(f.0.join("b.links.txt"), links("")).unwrap();
    let mut notes = vec![];
    let o = Opened::open(&f.script(), &[], &mut notes).unwrap();
    assert!(o.pair().unwrap().links.ends_with("a.links.txt"));
    assert!(notes.iter().any(|n| n.contains("several links files name film.fdx")), "{notes:?}");
}
