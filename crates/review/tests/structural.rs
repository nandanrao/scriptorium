//! A7: existing one-line suggestions when their lines are removed, joined,
//! split or retyped — in the session, and after saving and reopening it.

use scriptorium_core::doc::DocError;
use scriptorium_core::{ElementKind, Group, NewPara, ParaId, Pos, Selection};
use scriptorium_fdx::FdxFile;
use scriptorium_review::sessionfile::{self, ReviewSource};
use scriptorium_review::{Decision, Entry, FileStatus, LineChange, Refusal, ReviewFile, Status};

const FDX: &str = include_str!("fixtures/v2/sample.fdx");
// ¶0 EXT. FARM HOUSE - NIGHT · ¶1 Laura smiles. · ¶2 CESAR · ¶3 I am go to the town tomorrow.
// ¶4 They are playing truco (…). · ¶5 LAURA · ¶6 Thank you very much. · ¶7 She say hello to everybody and leaves.

/// Items in script order: ¶1, ¶2, ¶3, ¶6, ¶7.
const REVIEW: &str = "Id: s\n\
    ---\nBEFORE: Laura smiles.\nPARA: 1\nAFTER: Laura grins.\n\
    ---\nBEFORE: CESAR\nPARA: 2\nAFTER: CÉSAR\n\
    ---\nBEFORE: I am go to the town tomorrow.\nPARA: 3\nAFTER: I am going to the town tomorrow.\n\
    ---\nBEFORE: Thank you very much.\nPARA: 6\nAFTER: Thanks a lot.\n\
    ---\nBEFORE: She say hello to everybody and leaves.\nPARA: 7\nAFTER: She says hello to everybody and leaves.\n";

type Opened = sessionfile::ResumedSession;

fn fdx() -> FdxFile {
    FdxFile::parse(FDX.to_string()).unwrap()
}

fn resume(text: &str, saved: Option<&str>) -> Opened {
    let f = fdx();
    let source = ReviewSource { name: "a.review.txt".into(), path: "a.review.txt".into(), text: text.into() };
    sessionfile::resume(
        vec![source],
        f.to_script(),
        Some(&f.legacy_script()),
        f.raw(),
        saved,
        &|k| f.is_caps(k.name()),
        "sample.fdx",
        None,
    )
}

fn open() -> Opened {
    resume(REVIEW, None)
}

/// Save the session and open it again from the saved file.
fn reopen(o: &Opened) -> Opened {
    let saved = o.to_file(fdx().raw()).to_json();
    let back = resume(o.session.reviews[0].text(), Some(&saved));
    assert_eq!(back.doc.paragraphs(), o.doc.paragraphs(), "the reopened document is the same");
    back
}

fn sel(o: &Opened) -> Selection {
    Selection::cursor(o.doc.start())
}

/// The item on paragraph `para`.
fn item(o: &Opened, para: u64) -> usize {
    o.session.reviews[0].items.iter().position(|it| it.para == ParaId(para)).expect("an item on the paragraph")
}

fn decide(o: &mut Opened, para: u64, d: Decision) -> Result<Selection, DocError> {
    let (s, i) = (sel(o), item(o, para));
    o.session.decide(&mut o.doc, s, 0, i, d)
}

fn status(o: &Opened, para: u64) -> Status {
    o.session.status(&o.doc, 0, item(o, para))
}

fn change(o: &Opened, para: u64) -> Option<LineChange> {
    o.session.line_change(&o.doc, 0, item(o, para))
}

fn can_apply(o: &Opened, para: u64, d: &Decision) -> Result<(), String> {
    o.session.can_apply(&o.doc, 0, item(o, para), d)
}

fn outdated(o: &Opened, para: u64) -> bool {
    o.session.is_outdated(&o.doc, 0, item(o, para))
}

fn remove(o: &mut Opened, para: u64) {
    let s = sel(o);
    o.doc
        .edit(s, Group::Other, |e| {
            e.remove_para(ParaId(para))?;
            Ok((Selection::cursor(Pos::new(ParaId(0), 0)), ()))
        })
        .unwrap();
}

fn join(o: &mut Opened, first: u64, second: u64) {
    let s = sel(o);
    o.doc
        .edit(s, Group::Other, |e| {
            let at = e.join(ParaId(first), ParaId(second))?;
            Ok((Selection::cursor(at), ()))
        })
        .unwrap();
}

/// Enter at byte `at` of `para`; returns the tail's id.
fn split(o: &mut Opened, para: u64, at: usize) -> ParaId {
    o.doc.enter(Selection::cursor(Pos::new(ParaId(para), at))).unwrap();
    let i = o.doc.index_of(ParaId(para)).unwrap();
    o.doc.paragraphs()[i + 1].id
}

fn text(o: &Opened, para: ParaId) -> String {
    o.doc.paragraph(para).unwrap().text()
}

/// The paragraph ids of the items, in item order (the order numbers follow
/// for items matched together).
fn item_paras(o: &Opened) -> Vec<u64> {
    o.session.reviews[0].items.iter().map(|it| it.para.0).collect()
}

/// The entry saved as progress for the item on `para`.
fn progress_entry(o: &Opened, para: u64) -> Entry {
    let e = o.session.reviews[0].items[item(o, para)].entry;
    ReviewFile::parse(&o.session.save_progress(&o.doc, 0)).entries[e].clone()
}

#[test]
fn pending_line_removed_is_removed_and_cant_be_decided() {
    let mut o = open();
    remove(&mut o, 3);
    for o in [&o.clone(), &reopen(&o)] {
        assert_eq!(item_paras(o), [1, 2, 3, 6, 7], "the item stays, in its place");
        assert_eq!(status(o, 3), Status::Removed);
        assert_eq!(change(o, 3), Some(LineChange::Removed));
        assert!(!outdated(o, 3));
        for d in [Decision::Accept(0), Decision::KeepMine, Decision::Rewrite("x".into()), Decision::Undecided] {
            let e = can_apply(o, 3, &d).unwrap_err();
            assert!(e.starts_with("line removed"), "{e}");
        }
        let p = o.session.progress(&o.doc, 0);
        assert_eq!((p.total, p.pending, p.removed, p.done()), (5, 4, 1, 0));
        assert_eq!(o.session.next_pending(&o.doc, 0, Some(item(o, 2))), Some(item(o, 6)), "skipped as not pending");
        let e = progress_entry(o, 3);
        assert_eq!((e.status, e.mine), (FileStatus::Pending, None));
    }
    let before = o.doc.clone();
    assert_eq!(decide(&mut o, 3, Decision::Accept(0)), Err(DocError::MissingParagraph(ParaId(3))));
    assert_eq!(o.doc.log(), before.log(), "nothing recorded");
}

#[test]
fn decided_line_removed_is_rewritten_with_mine_cut() {
    let mut o = open();
    decide(&mut o, 3, Decision::Accept(0)).unwrap();
    decide(&mut o, 6, Decision::KeepMine).unwrap();
    decide(&mut o, 7, Decision::Rewrite("She leaves.".into())).unwrap();
    for p in [3, 6, 7] {
        remove(&mut o, p);
    }
    for o in [&o.clone(), &reopen(&o)] {
        assert_eq!(item_paras(o), [1, 2, 3, 6, 7]);
        for p in [3, 6, 7] {
            assert_eq!(status(o, p), Status::Rewritten, "¶{p}: its text was cut");
            assert!(can_apply(o, p, &Decision::Undecided).unwrap_err().starts_with("line removed"));
            let e = progress_entry(o, p);
            assert_eq!(
                (e.status, e.mine.as_deref()),
                (FileStatus::Rewritten, Some([].as_slice())),
                "¶{p}: never an empty MINE"
            );
            assert_eq!(o.session.status_label(&o.doc, 0, item(o, p)), "line cut");
            assert_eq!(o.session.line_label(&o.doc, 0, item(o, p)), Some("line cut"));
        }
        let p = o.session.progress(&o.doc, 0);
        assert_eq!((p.rewritten, p.removed, p.pending), (3, 0, 2));
    }
    // Progress written over the file reopens with the lines still cut, and
    // `[cut]` is never written in as text.
    let progress = o.session.save_progress(&o.doc, 0);
    assert!(progress.contains("MINE: [cut]"), "{progress}");
    let back = resume(&progress, None);
    assert_eq!(back.doc.paragraphs().len(), fdx().to_script().paragraphs.len());
    assert_eq!(text_of(&back, 6), "Thank you very much.");
}

#[test]
fn line_joined_away_is_removed() {
    let mut o = open();
    // ¶3 joined onto ¶2: ¶3 is gone, ¶2 has the next line joined on.
    join(&mut o, 2, 3);
    for o in [&o.clone(), &reopen(&o)] {
        assert_eq!(status(o, 3), Status::Removed);
        assert_eq!(change(o, 2), Some(LineChange::Joined));
        assert!(outdated(o, 2));
        // Accepting "CÉSAR" would drop the joined text.
        assert!(can_apply(o, 2, &Decision::Accept(0)).unwrap_err().starts_with("line changed"));
        assert_eq!(can_apply(o, 2, &Decision::KeepMine), Ok(()));
        assert_eq!(can_apply(o, 2, &Decision::Rewrite("CÉSAR".into())), Ok(()));
    }
    // Undoing the join brings both back as they were.
    o.doc.undo().unwrap();
    assert_eq!(status(&o, 3), Status::Pending);
    assert_eq!(change(&o, 2), None);
    assert!(!outdated(&o, 2));
}

#[test]
fn pending_split_stays_on_head_outdated_and_refuses_accept() {
    let mut o = open();
    let tail = split(&mut o, 7, 13); // "She say hello" | " to everybody and leaves."
    for o in [&o.clone(), &reopen(&o)] {
        assert_eq!(item_paras(o), [1, 2, 3, 6, 7], "on the head");
        assert_eq!(status(o, 7), Status::Pending);
        assert_eq!(change(o, 7), Some(LineChange::Split));
        assert!(outdated(o, 7));
        let e = can_apply(o, 7, &Decision::Accept(0)).unwrap_err();
        assert!(e.starts_with("line changed") && e.contains("rewrite"), "{e}");
        assert_eq!(can_apply(o, 7, &Decision::KeepMine), Ok(()));
    }
    let before = o.doc.paragraphs().to_vec();
    assert!(matches!(decide(&mut o, 7, Decision::Accept(0)), Err(DocError::Mismatch(_))));
    assert_eq!(o.doc.paragraphs(), before);
    // Keeping it keeps the head as it is.
    decide(&mut o, 7, Decision::KeepMine).unwrap();
    assert_eq!(status(&o, 7), Status::Rejected);
    assert_eq!((text(&o, ParaId(7)), text(&o, tail)), ("She say hello".into(), " to everybody and leaves.".into()));
    // A rewrite goes into the head only.
    decide(&mut o, 7, Decision::Rewrite("She says hello".into())).unwrap();
    assert_eq!((text(&o, ParaId(7)), text(&o, tail)), ("She says hello".into(), " to everybody and leaves.".into()));
}

/// The review's repro: deciding on a split head wrote the whole-line text
/// back into it, giving "CESAR" + "SAR".
#[test]
fn decided_split_head_never_duplicates_the_tail() {
    let mut o = open();
    decide(&mut o, 2, Decision::Accept(0)).unwrap();
    let tail = split(&mut o, 2, "CÉ".len()); // "CÉ" | "SAR"
    for o in [&o.clone(), &reopen(&o)] {
        assert_eq!(status(o, 2), Status::Rewritten, "the line has changed since it was accepted");
        assert_eq!(change(o, 2), Some(LineChange::Split));
        for d in [Decision::Accept(0), Decision::KeepMine] {
            assert!(can_apply(o, 2, &d).unwrap_err().starts_with("line changed"), "{d:?}");
        }
    }
    assert!(decide(&mut o, 2, Decision::KeepMine).is_err());
    assert!(decide(&mut o, 2, Decision::Accept(0)).is_err());
    assert_eq!((text(&o, ParaId(2)), text(&o, tail)), ("CÉ".into(), "SAR".into()));
    // Back to undecided clears the decision and leaves the text alone.
    decide(&mut o, 2, Decision::Undecided).unwrap();
    assert_eq!((text(&o, ParaId(2)), text(&o, tail)), ("CÉ".into(), "SAR".into()));
    assert_eq!(status(&o, 2), Status::Pending);
    assert!(outdated(&o, 2));
    let o = reopen(&o);
    assert_eq!(status(&o, 2), Status::Pending);
}

#[test]
fn enter_at_end_of_a_line_is_not_a_split() {
    let mut o = open();
    let new = split(&mut o, 6, "Thank you very much.".len());
    let s = sel(&o);
    o.doc.replace_text(s, new, "Another line.", &[]).unwrap();
    for o in [&o.clone(), &reopen(&o)] {
        assert_eq!(change(o, 6), None);
        assert!(!outdated(o, 6));
        assert_eq!(can_apply(o, 6, &Decision::Accept(0)), Ok(()));
    }
    decide(&mut o, 6, Decision::Accept(0)).unwrap();
    assert_eq!((text(&o, ParaId(6)), text(&o, new)), ("Thanks a lot.".into(), "Another line.".into()));
}

#[test]
fn undone_split_is_no_change() {
    let mut o = open();
    split(&mut o, 7, 13);
    o.doc.undo().unwrap();
    assert_eq!(change(&o, 7), None);
    assert!(!outdated(&o, 7));
    let o = reopen(&o);
    assert_eq!(change(&o, 7), None);
}

#[test]
fn retyped_line_stays_and_is_outdated() {
    let mut o = open();
    let s = Selection::cursor(Pos::new(ParaId(1), 0));
    o.doc.set_kind(s, ElementKind::General).unwrap();
    for o in [&o.clone(), &reopen(&o)] {
        assert_eq!(item_paras(o), [1, 2, 3, 6, 7]);
        assert_eq!(status(o, 1), Status::Pending);
        assert_eq!(change(o, 1), Some(LineChange::Retyped));
        assert!(outdated(o, 1));
        assert_eq!(can_apply(o, 1, &Decision::Accept(0)), Ok(()));
    }
    decide(&mut o, 1, Decision::Accept(0)).unwrap();
    assert_eq!(status(&o, 1), Status::Accepted(0));
    // Retyped back: no change.
    let s = Selection::cursor(Pos::new(ParaId(1), 0));
    o.doc.set_kind(s, ElementKind::Action).unwrap();
    assert_eq!(change(&o, 1), None);
}

#[test]
fn suggestion_on_an_added_line_survives_its_removal_and_reopening() {
    let mut o = open();
    let s = sel(&o);
    let new = o
        .doc
        .edit(s, Group::Other, |e| {
            let id = e.insert_para(5, NewPara::plain(ElementKind::Action, "Cesar laughs."))?;
            Ok((Selection::cursor(Pos::new(id, 0)), id))
        })
        .unwrap();
    let entry = Entry { para: vec![new], ..Entry::single("Cesar laughs.", ["César laughs."]) };
    let caps = |_: &ElementKind| false;
    let added = o.session.append_entry(&o.doc, 0, entry, caps).unwrap();
    let i = added.item.unwrap();
    remove(&mut o, new.0);
    assert_eq!(o.session.status(&o.doc, 0, i), Status::Removed);
    let back = reopen(&o);
    assert_eq!(item_paras(&back), [1, 2, 3, new.0, 6, 7], "kept, between its neighbours");
    assert_eq!(status(&back, new.0), Status::Removed);
}

#[test]
fn reopened_removed_line_keeps_item_order() {
    // Removing the first and last suggested lines keeps every item in place.
    let mut o = open();
    remove(&mut o, 1);
    remove(&mut o, 7);
    let back = reopen(&o);
    assert_eq!(item_paras(&back), [1, 2, 3, 6, 7]);
    assert_eq!(status(&back, 1), Status::Removed);
    assert_eq!(status(&back, 7), Status::Removed);
}

#[test]
fn cut_marker_in_a_progress_file_is_never_written_in_as_text() {
    let text = REVIEW.replace("AFTER: Thanks a lot.\n", "AFTER: Thanks a lot.\nSTATUS: rewritten\nMINE: [cut]\n");
    let o = resume(&text, None);
    assert_eq!(text_of(&o, 6), "Thank you very much.");
    assert_eq!(status(&o, 6), Status::Pending);
}

fn text_of(o: &Opened, para: u64) -> String {
    text(o, ParaId(para))
}

/// The review's repro: an accepted line made shorter, then Enter at its new
/// end, is not a split (the cut was at the end of the text as it was then).
#[test]
fn enter_at_end_of_an_accepted_shorter_line_is_not_a_split() {
    let mut o = open();
    decide(&mut o, 6, Decision::Accept(0)).unwrap(); // "Thank you very much." -> "Thanks a lot."
    let new = split(&mut o, 6, "Thanks a lot.".len());
    for o in [&o.clone(), &reopen(&o)] {
        assert_eq!(change(o, 6), None);
        assert_eq!(o.session.line_label(&o.doc, 0, item(o, 6)), None);
        assert_eq!(status(o, 6), Status::Accepted(0));
        assert_eq!(can_apply(o, 6, &Decision::KeepMine), Ok(()));
    }
    decide(&mut o, 6, Decision::KeepMine).unwrap();
    assert_eq!((text(&o, ParaId(6)), text(&o, new)), ("Thank you very much.".into(), String::new()));
    decide(&mut o, 6, Decision::Undecided).unwrap();
    assert_eq!(status(&o, 6), Status::Pending);
    assert!(!outdated(&o, 6));
}

/// The same for a pending line shortened by hand; a cut inside the shorter
/// text still counts.
#[test]
fn enter_at_end_of_a_line_shortened_by_hand_is_not_a_split() {
    let mut o = open();
    let s = sel(&o);
    o.doc.replace_text(s, ParaId(7), "She leaves.", &[]).unwrap();
    split(&mut o, 7, "She leaves.".len());
    assert_eq!(change(&o, 7), None);
    assert!(outdated(&o, 7), "its text changed");
    assert_eq!(can_apply(&o, 7, &Decision::Accept(0)), Ok(()));
    split(&mut o, 7, "She".len());
    assert_eq!(change(&o, 7), Some(LineChange::Split));
}

/// Split, then the two lines joined back by hand (not undone): no change.
#[test]
fn split_joined_back_is_no_change() {
    let mut o = open();
    let tail = split(&mut o, 7, 13);
    join(&mut o, 7, tail.0);
    for o in [&o.clone(), &reopen(&o)] {
        assert_eq!(change(o, 7), None);
        assert!(!outdated(o, 7));
        assert_eq!(can_apply(o, 7, &Decision::Accept(0)), Ok(()));
    }
    // Split again: a split again.
    split(&mut o, 7, 13);
    assert_eq!(change(&o, 7), Some(LineChange::Split));
}

#[test]
fn refusal_is_phrased_per_host() {
    let mut o = open();
    split(&mut o, 7, 13);
    let e = o.session.check(&o.doc, 0, item(&o, 7), &Decision::Accept(0)).unwrap_err();
    assert_eq!(e, Refusal::LineChanged { para: ParaId(7), how: LineChange::Split });
    assert!(e.message("press E to rewrite this line").ends_with("; press E to rewrite this line"));
    assert!(e.to_string().ends_with("; rewrite it instead"));
    let e = o.session.check(&o.doc, 0, item(&o, 7), &Decision::Accept(3)).unwrap_err();
    assert_eq!(e.to_string(), "no option 4 (1 to 1)");
}

/// The line-change cache must not hand one document's answers to another
/// whose log length, next id and paragraph count happen to match.
#[test]
fn line_change_cache_is_not_fooled_by_an_equal_looking_document() {
    let mut o = open();
    remove(&mut o, 1);
    assert_eq!(change(&o, 1), Some(LineChange::Removed)); // cached for this document
    let mut other = open();
    remove(&mut other, 2); // same log length, next id and paragraph count
    assert_eq!(
        (o.doc.log().len(), o.doc.next_id(), o.doc.paragraphs().len()),
        (other.doc.log().len(), other.doc.next_id(), other.doc.paragraphs().len())
    );
    // Same session (and so cache), different document.
    assert_eq!(o.session.line_change(&other.doc, 0, item(&other, 1)), None);
    // A rebased document is a new start too.
    let rebased = other.doc.clone().rebase();
    assert_ne!(rebased.origin(), other.doc.origin());
    assert_eq!(o.session.line_change(&rebased, 0, item(&other, 2)), Some(LineChange::Removed));
}
