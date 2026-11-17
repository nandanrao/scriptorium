//! Before a script is written over its own file (versions A7b):
//! `Session::save_progress_at` (progress with `PARA` at the written file's
//! ids) and `Session::blocking_write` (suggestions whose lines were split,
//! joined or removed).

use std::collections::HashMap;

use scriptorium_core::{Group, ParaId, Pos, Selection};
use scriptorium_fdx::FdxFile;
use scriptorium_review::sessionfile::{self, ReviewSource};
use scriptorium_review::{Decision, LineChange, ReviewFile};

const FDX: &str = include_str!("fixtures/v2/sample.fdx");
// ¶0 EXT. FARM HOUSE - NIGHT · ¶1 Laura smiles. · ¶2 CESAR · ¶3 I am go to the town tomorrow.
// ¶4 They are playing truco (…). · ¶5 LAURA · ¶6 Thank you very much. · ¶7 She say hello to everybody and leaves.

/// Entries 0-2 on ¶1, ¶3, ¶6; entry 3 for a line that isn't there, with a
/// `PARA`.
const REVIEW: &str = "Id: s\n\
    ---\nBEFORE: Laura smiles.\nPARA: 1\nAFTER: Laura grins.\n\
    ---\nBEFORE: I am go to the town tomorrow.\nPARA: 3\nAFTER: I am going to the town tomorrow.\n\
    ---\nBEFORE: Thank you very much.\nPARA: 6\nAFTER: Thanks a lot.\n\
    ---\nBEFORE: Nobody says this.\nPARA: 5\nAFTER: Nor this.\n";

type Opened = sessionfile::ResumedSession;

fn open() -> Opened {
    let f = FdxFile::parse(FDX.to_string()).unwrap();
    let source = ReviewSource { name: "a.review.txt".into(), path: "a.review.txt".into(), text: REVIEW.into() };
    sessionfile::resume(
        vec![source],
        f.to_script(),
        Some(&f.legacy_script()),
        f.raw(),
        None,
        &|k| f.is_caps(k.name()),
        "sample.fdx",
        None,
    )
}

fn item(o: &Opened, para: u64) -> usize {
    o.session.reviews[0].items.iter().position(|it| it.para == ParaId(para)).unwrap()
}

fn positions(o: &Opened) -> HashMap<ParaId, ParaId> {
    o.doc.paragraphs().iter().enumerate().map(|(i, p)| (p.id, ParaId(i as u64))).collect()
}

fn paras(text: &str) -> Vec<Vec<u64>> {
    ReviewFile::parse(text).entries.iter().map(|e| e.para.iter().map(|p| p.0).collect()).collect()
}

fn remove(o: &mut Opened, para: u64) {
    let s = Selection::cursor(o.doc.start());
    o.doc
        .edit(s, Group::Other, |e| {
            e.remove_para(ParaId(para))?;
            Ok((Selection::cursor(Pos::new(ParaId(0), 0)), ()))
        })
        .unwrap();
}

#[test]
fn para_follows_the_lines_to_their_new_positions_and_unplaced_entries_lose_theirs() {
    let mut o = open();
    assert_eq!(o.session.reviews[0].unmatched.len(), 1);
    // Two lines added after ¶0: every later line moves down two.
    for _ in 0..2 {
        let at = Pos::new(ParaId(0), o.doc.paragraph(ParaId(0)).unwrap().text().len());
        o.doc.enter(Selection::cursor(at)).unwrap();
    }
    let (s, i) = (Selection::cursor(o.doc.start()), item(&o, 3));
    o.session.decide(&mut o.doc, s, 0, i, Decision::Accept(0)).unwrap();
    let text = o.session.save_progress_at(&o.doc, 0, &positions(&o));
    assert_eq!(paras(&text), vec![vec![3], vec![5], vec![8], vec![]]);
    assert!(text.contains("STATUS: accepted"));
    // Without positions, as before: the file's PARA kept.
    assert_eq!(paras(&o.session.save_progress(&o.doc, 0)), vec![vec![1], vec![3], vec![6], vec![5]]);
}

#[test]
fn a_gone_line_drops_its_para_and_blocks_the_write() {
    let mut o = open();
    assert!(o.session.blocking_write(&o.doc).is_empty());
    let (pending, accepted) = (item(&o, 6), item(&o, 1));
    let s = Selection::cursor(o.doc.start());
    o.session.decide(&mut o.doc, s, 0, accepted, Decision::Accept(0)).unwrap();
    remove(&mut o, 6);
    remove(&mut o, 1);
    let text = o.session.save_progress_at(&o.doc, 0, &positions(&o));
    assert_eq!(paras(&text)[0], Vec::<u64>::new());
    assert_eq!(paras(&text)[2], Vec::<u64>::new());
    let mut blocking = o.session.blocking_write(&o.doc);
    blocking.sort_by_key(|b| b.1);
    let mut want = vec![(0, pending, LineChange::Removed), (0, accepted, LineChange::Removed)];
    want.sort_by_key(|b| b.1);
    assert_eq!(blocking, want);
}

#[test]
fn a_pending_suggestion_on_a_split_line_blocks_the_write_a_decided_one_doesnt() {
    let mut o = open();
    let (i3, i6) = (item(&o, 3), item(&o, 6));
    let s = Selection::cursor(o.doc.start());
    o.session.decide(&mut o.doc, s, 0, i6, Decision::KeepMine).unwrap();
    o.doc.enter(Selection::cursor(Pos::new(ParaId(3), 4))).unwrap();
    o.doc.enter(Selection::cursor(Pos::new(ParaId(6), 5))).unwrap();
    assert_eq!(o.session.blocking_write(&o.doc), vec![(0, i3, LineChange::Split)]);
}
