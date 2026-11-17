//! B3: block suggestions — matching N lines, decisions as one transaction,
//! statuses, overlap rules, and everything surviving save and reopen.

use scriptorium_core::{Document, ElementKind, Group, ParaId, Paragraph, Pos, Script, Selection};
use scriptorium_fdx::FdxFile;
use scriptorium_review::sessionfile::{self, ReviewSource};
use scriptorium_review::{CurrentRun, Decision, Entry, Line, Refusal, Review, Shape, Status, Unplaced};

const FDX: &str = include_str!("fixtures/v2/sample.fdx");
// ¶0 EXT. FARM HOUSE - NIGHT · ¶1 Laura smiles. · ¶2 CESAR · ¶3 I am go to the town tomorrow.
// ¶4 They are playing truco (Argentinian cards game). · ¶5 LAURA · ¶6 Thank you very much.
// ¶7 She say hello to everybody and leaves.

const ORIGINAL: [&str; 8] = [
    "Scene Heading: EXT. FARM HOUSE - NIGHT",
    "Action: Laura smiles.",
    "Character: CESAR",
    "Dialogue: I am go to the town tomorrow.",
    "Action: They are playing truco (Argentinian cards game).",
    "Character: LAURA",
    "Dialogue: Thank you very much.",
    "Action: She say hello to everybody and leaves.",
];

/// Entries: e0 N→M (¶5,6 → 3 lines, or a cut), e1 N→0 with a second option
/// (¶4), e2 insertion 1→1+M (¶1), e3 typed 1→1 (¶7).
const REVIEW: &str = "SCRIPT REVIEW v2\nId: b\n\
---\nPARA: 5, 6\nBEFORE:\n  CHARACTER: LAURA\n  DIALOGUE: Thank you very much.\n\
AFTER:\n  CHARACTER: LAURA\n  DIALOGUE: Thanks.\n  ACTION: She waves.\nAFTER: [cut]\nWHY: shorter\n\
---\nPARA: 4\nBEFORE:\n  ACTION: They are playing truco (Argentinian cards game).\nAFTER: [cut]\n\
AFTER:\n  ACTION: They play cards.\n\
---\nPARA: 1\nBEFORE:\n  ACTION: Laura smiles.\nAFTER:\n  ACTION: Laura smiles.\n  ACTION: Cesar nods.\n\
---\nPARA: 7\nBEFORE:\n  ACTION: She say hello to everybody and leaves.\n\
AFTER:\n  ACTION: She says hello to everybody and leaves.\n";

type Opened = sessionfile::ResumedSession;

fn fdx() -> FdxFile {
    FdxFile::parse(FDX.to_string()).unwrap()
}

fn resume_many(texts: &[&str], saved: Option<&str>) -> Opened {
    let f = fdx();
    let sources = texts
        .iter()
        .enumerate()
        .map(|(n, t)| ReviewSource { name: format!("r{n}"), path: format!("r{n}.review.txt"), text: t.to_string() })
        .collect();
    sessionfile::resume(
        sources,
        f.to_script(),
        Some(&f.legacy_script()),
        f.raw(),
        saved,
        &|k| f.is_caps(k.name()),
        "sample.fdx",
        None,
    )
}

fn open(text: &str) -> Opened {
    resume_many(&[text], None)
}

/// Save the session and open it again from the saved file.
fn reopen(o: &Opened) -> Opened {
    let saved = o.to_file(fdx().raw()).to_json();
    let texts: Vec<&str> = o.session.reviews.iter().map(|r| r.text()).collect();
    let back = resume_many(&texts, Some(&saved));
    assert_eq!(back.doc.paragraphs(), o.doc.paragraphs(), "the reopened document is the same");
    back
}

fn lines(doc: &Document) -> Vec<String> {
    doc.paragraphs().iter().map(|p| format!("{}: {}", p.kind.name(), p.text())).collect()
}

fn sel(o: &Opened) -> Selection {
    Selection::cursor(o.doc.start())
}

/// The item of review `r` made from entry `e`.
fn item(o: &Opened, r: usize, e: usize) -> usize {
    o.session.reviews[r].items.iter().position(|it| it.entry == e).expect("an item for the entry")
}

fn decide(o: &mut Opened, r: usize, e: usize, d: Decision) -> Result<Selection, scriptorium_core::DocError> {
    let (s, i) = (sel(o), item(o, r, e));
    o.session.decide(&mut o.doc, s, r, i, d)
}

fn status(o: &Opened, r: usize, e: usize) -> Status {
    o.session.status(&o.doc, r, item(o, r, e))
}

fn with(base: &[&str], at: usize, remove: usize, insert: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = base.iter().map(|s| s.to_string()).collect();
    v.splice(at..at + remove, insert.iter().map(|s| s.to_string()));
    v
}

/// Decide, check the text and status; undo is one step back to pending
/// and the original; redo comes back; save and reopen keeps it.
fn round_trip(e: usize, d: Decision, want: &[String], want_status: Status) {
    let mut o = open(REVIEW);
    assert_eq!(status(&o, 0, e), Status::Pending);
    decide(&mut o, 0, e, d).unwrap();
    assert_eq!(lines(&o.doc), want);
    assert_eq!(status(&o, 0, e), want_status);
    o.doc.undo().unwrap();
    assert_eq!(lines(&o.doc), ORIGINAL, "one undo step takes it all back");
    assert_eq!(status(&o, 0, e), Status::Pending);
    o.doc.redo().unwrap();
    assert_eq!(lines(&o.doc), want);
    let back = reopen(&o);
    assert_eq!(status(&back, 0, e), want_status, "the status survives reopening");
    assert_eq!(back.session.reviews[0].items.len(), 4, "every block is placed again");
}

#[test]
fn every_block_is_placed_by_para() {
    let o = open(REVIEW);
    let r = &o.session.reviews[0];
    assert!(r.unmatched.is_empty(), "{:?}", r.unmatched);
    let it = &r.items[item(&o, 0, 0)];
    assert!(it.block);
    assert_eq!(it.paras, vec![ParaId(5), ParaId(6)]);
    assert_eq!(it.para(), ParaId(5));
    assert_eq!(it.before_ids(), vec![ParaId(5), ParaId(6)]);
    assert_eq!(it.before, "LAURA / Thank you very much.");
    assert_eq!(it.options, vec!["LAURA / Thanks. / She waves.".to_string(), String::new()]);
    assert_eq!(it.option_lines[1], Vec::<Line>::new());
    assert_eq!(it.before_paras[1].kind, ElementKind::Dialogue);
    // Script order.
    let order: Vec<usize> = r.items.iter().map(|it| it.entry).collect();
    assert_eq!(order, vec![2, 1, 0, 3]);
}

#[test]
fn n_to_m_accept_keep_undecide() {
    let want = with(&ORIGINAL, 5, 2, &["Character: LAURA", "Dialogue: Thanks.", "Action: She waves."]);
    round_trip(0, Decision::Accept(0), &want, Status::Accepted(0));

    let mut o = open(REVIEW);
    decide(&mut o, 0, 0, Decision::Accept(0)).unwrap();
    let key = o.session.key(0, item(&o, 0, 0));
    let value = o.doc.meta(&key).unwrap().to_string();
    assert!(value.starts_with("accepted:0@5,6=>5,"), "{value}");
    assert_eq!(value.matches(',').count(), 3, "{value}");
    decide(&mut o, 0, 0, Decision::KeepMine).unwrap();
    assert_eq!(lines(&o.doc), ORIGINAL);
    assert_eq!(o.doc.paragraphs()[5].id, ParaId(5), "the lines come back with their ids");
    assert_eq!(o.doc.paragraphs()[6].id, ParaId(6));
    assert_eq!(status(&o, 0, 0), Status::Rejected);
    assert_eq!(o.doc.meta(&key), Some("rejected@5,6"));
    decide(&mut o, 0, 0, Decision::Accept(0)).unwrap();
    decide(&mut o, 0, 0, Decision::Undecided).unwrap();
    assert_eq!(lines(&o.doc), ORIGINAL);
    assert_eq!(status(&o, 0, 0), Status::Pending);
    // Pending keep: the decision only.
    decide(&mut o, 0, 0, Decision::KeepMine).unwrap();
    assert_eq!(lines(&o.doc), ORIGINAL);
    assert_eq!(status(&o, 0, 0), Status::Rejected);
    assert_eq!(reopen(&o).session.status(&o.doc, 0, item(&o, 0, 0)), Status::Rejected);
}

#[test]
fn n_to_zero_cut() {
    let want = with(&ORIGINAL, 5, 2, &[]);
    round_trip(0, Decision::Accept(1), &want, Status::Accepted(1));

    let mut o = open(REVIEW);
    decide(&mut o, 0, 0, Decision::Accept(1)).unwrap();
    let i = item(&o, 0, 0);
    assert_eq!(o.doc.meta(&o.session.key(0, i)), Some("accepted:1@5,6=>after:4"));
    assert_eq!(o.session.status_label(&o.doc, 0, i), "accepted 2");
    assert_eq!(o.session.line_label(&o.doc, 0, i), Some("line cut"));
    assert!(o.session.live_run(&o.doc, 0, i).is_empty());
    // Reopened, the cut is placed where the lines were (C1).
    let back = reopen(&o);
    let bi = item(&back, 0, 0);
    assert!(back.session.reviews[0].items[bi].paras.is_empty());
    assert_eq!(back.session.reviews[0].items[bi].index, 5);
    let mut back = back;
    decide(&mut back, 0, 0, Decision::KeepMine).unwrap();
    assert_eq!(lines(&back.doc), ORIGINAL, "keep mine after reopening puts the lines back");
}

#[test]
fn switching_options_after_a_cut() {
    let mut o = open(REVIEW);
    decide(&mut o, 0, 1, Decision::Accept(0)).unwrap();
    assert_eq!(lines(&o.doc), with(&ORIGINAL, 4, 1, &[]));
    assert_eq!(status(&o, 0, 1), Status::Accepted(0));
    decide(&mut o, 0, 1, Decision::Accept(1)).unwrap();
    assert_eq!(lines(&o.doc), with(&ORIGINAL, 4, 1, &["Action: They play cards."]));
    assert_eq!(status(&o, 0, 1), Status::Accepted(1));
    let back = reopen(&o);
    assert_eq!(status(&back, 0, 1), Status::Accepted(1));
    let mut back = back;
    decide(&mut back, 0, 1, Decision::Accept(0)).unwrap();
    assert_eq!(lines(&back.doc), with(&ORIGINAL, 4, 1, &[]));
    decide(&mut back, 0, 1, Decision::Undecided).unwrap();
    assert_eq!(lines(&back.doc), ORIGINAL);
    assert_eq!(back.doc.paragraphs()[4].id, ParaId(4));
}

#[test]
fn insertion_one_to_one_plus_m() {
    let want = with(&ORIGINAL, 2, 0, &["Action: Cesar nods."]);
    round_trip(2, Decision::Accept(0), &want, Status::Accepted(0));
    let mut o = open(REVIEW);
    decide(&mut o, 0, 2, Decision::Accept(0)).unwrap();
    assert_eq!(o.doc.paragraphs()[1].id, ParaId(1), "the anchor keeps its id");
    decide(&mut o, 0, 2, Decision::Undecided).unwrap();
    assert_eq!(lines(&o.doc), ORIGINAL);
}

#[test]
fn typed_one_to_one() {
    let want = with(&ORIGINAL, 7, 1, &["Action: She says hello to everybody and leaves."]);
    round_trip(3, Decision::Accept(0), &want, Status::Accepted(0));
    let mut o = open(REVIEW);
    decide(&mut o, 0, 3, Decision::Accept(0)).unwrap();
    assert_eq!(o.doc.paragraphs()[7].id, ParaId(7));
}

#[test]
fn rewrite_and_progress_with_mine_blocks() {
    let mine = vec![Line::typed(ElementKind::Action, "Laura nods."), Line::untyped("You're welcome.")];
    let want = with(&ORIGINAL, 5, 2, &["Action: Laura nods.", "Dialogue: You're welcome."]);
    round_trip(0, Decision::RewriteLines(mine.clone()), &want, Status::Rewritten);

    let mut o = open(REVIEW);
    decide(&mut o, 0, 0, Decision::RewriteLines(mine)).unwrap();
    decide(&mut o, 0, 1, Decision::Accept(0)).unwrap();
    decide(&mut o, 0, 3, Decision::KeepMine).unwrap();
    let progress = o.session.save_progress(&o.doc, 0);
    assert!(progress.contains("MINE:\n  ACTION: Laura nods.\n  DIALOGUE: You're welcome."), "{progress}");
    // Opening the progress afresh applies it again.
    let fresh = open(&progress);
    assert_eq!(lines(&fresh.doc), lines(&o.doc));
    assert_eq!(status(&fresh, 0, 0), Status::Rewritten);
    assert_eq!(status(&fresh, 0, 1), Status::Accepted(0));
    assert_eq!(status(&fresh, 0, 3), Status::Rejected);
    assert_eq!(fresh.session.save_progress(&fresh.doc, 0), progress, "progress round-trips");
}

#[test]
fn hand_edits_after_accept_show_rewritten_and_keep_restores() {
    let mut o = open(REVIEW);
    decide(&mut o, 0, 0, Decision::Accept(0)).unwrap();
    let s = sel(&o);
    let last = o.doc.paragraphs()[7].id;
    o.doc.replace_text(s, last, "She waves twice.", &[]).unwrap();
    assert_eq!(status(&o, 0, 0), Status::Rewritten);
    let i = item(&o, 0, 0);
    let mine: Vec<String> = o.session.mine(&o.doc, 0, i).into_iter().map(|l| l.text).collect();
    assert_eq!(mine, vec!["LAURA", "Thanks.", "She waves twice."]);
    assert!(o.session.save_progress(&o.doc, 0).contains("  ACTION: She waves twice."));
    decide(&mut o, 0, 0, Decision::KeepMine).unwrap();
    assert_eq!(lines(&o.doc), ORIGINAL);
}

#[test]
fn keep_mine_restores_the_lines_the_suggestion_was_made_on() {
    // The suggestion was written against a hand-edited ¶6.
    let mut o = open(REVIEW);
    let s = sel(&o);
    o.doc.replace_text(s, ParaId(6), "Thank you so much.", &[]).unwrap();
    let text = "Id: c\n---\nPARA: 5, 6\nBEFORE:\n  CHARACTER: LAURA\n  DIALOGUE: Thank you so much.\n\
                AFTER: [cut]\n";
    let review = Review::open_live("c", text, &fdx().to_script(), &o.doc, |_| false, "", None);
    let r = o.session.add(review).unwrap();
    assert_eq!(o.session.reviews[r].items.len(), 1);
    decide(&mut o, r, 0, Decision::Accept(0)).unwrap();
    decide(&mut o, r, 0, Decision::KeepMine).unwrap();
    assert_eq!(o.doc.paragraph(ParaId(6)).unwrap().text(), "Thank you so much.");
}

/// A script of plain lines.
fn script(ls: &[(ElementKind, &str)]) -> Script {
    let paragraphs =
        ls.iter().enumerate().map(|(i, (k, t))| Paragraph::plain(ParaId(i as u64), k.clone(), *t)).collect();
    Script { paragraphs, id_floor: ls.len() as u64, ..Script::default() }
}

fn block(before: &[(ElementKind, &str)]) -> String {
    let mut out = "---\nBEFORE:\n".to_string();
    for (k, t) in before {
        out.push_str(&format!("  {}: {t}\n", k.name().to_uppercase()));
    }
    out.push_str("AFTER: [cut]\n");
    out
}

#[test]
fn before_matching_exact_fuzzy_and_ambiguous() {
    use ElementKind::{Action, Character, Dialogue};
    let s = script(&[
        (Character, "ANA"),
        (Dialogue, "We should leave before the storm comes."),
        (Action, "Thunder."),
        (Character, "BEN"),
        (Dialogue, "Not yet."),
        (Character, "BEN"),
        (Dialogue, "Not yet."),
        (Character, "CARLA"),
        (Dialogue, "I keep thinking about the old lighthouse."),
        (Character, "CARLA"),
        (Dialogue, "I keep thinking about the old lighthouses."),
    ]);
    let entries = [
        // e0: exact, unique.
        block(&[(Character, "ANA"), (Dialogue, "We should leave before the storm comes.")]),
        // e1: fuzzy on every line.
        block(&[(Dialogue, "We should leave before the storm come"), (Action, "Thunder!")]),
        // e2: one line too far off: no match.
        block(&[(Character, "ANA"), (Dialogue, "Something else entirely.")]),
        // e3: two exact windows.
        block(&[(Character, "BEN"), (Dialogue, "Not yet.")]),
        // e4: two fuzzy windows, neither clearly best.
        block(&[(Character, "CARLA"), (Dialogue, "I keep thinking about that old lighthouse")]),
        // e5: kinds must match where typed.
        block(&[(Action, "ANA"), (Dialogue, "We should leave before the storm comes.")]),
    ];
    let text = format!("SCRIPT REVIEW v2\n{}", entries.concat());
    let r = Review::open("x", &text, &s, |_| false, "");
    let at = |e: usize| r.items.iter().find(|it| it.entry == e).map(|it| (it.paras.clone(), it.fuzzy));
    let ids = |v: &[u64]| v.iter().copied().map(ParaId).collect::<Vec<_>>();
    assert_eq!(at(0), Some((ids(&[0, 1]), false)));
    // e1's first window overlaps e0's: within a review no line is shared.
    assert_eq!(at(1), None);
    let why = |e: usize| r.unmatched.iter().find(|u| u.entry == e).and_then(|u| u.why.clone());
    assert_eq!(why(1), Some(Unplaced::Missing));
    assert_eq!(why(2), Some(Unplaced::Missing));
    assert_eq!(why(3), Some(Unplaced::Ambiguous));
    assert_eq!(why(4), Some(Unplaced::Ambiguous));
    assert_eq!(why(5), Some(Unplaced::Missing));

    // Fuzzy on its own, and a clear fuzzy winner.
    let text = format!(
        "{}{}",
        block(&[(Dialogue, "We should leave before the storm come"), (Action, "Thunder!")]),
        block(&[(Character, "CARLA"), (Dialogue, "I keep  thinking about the old lighthouses.")])
    );
    let r = Review::open("y", &text, &s, |_| false, "");
    let at = |e: usize| r.items.iter().find(|it| it.entry == e).map(|it| (it.paras.clone(), it.fuzzy));
    assert_eq!(at(0), Some((ids(&[1, 2]), true)));
    assert_eq!(at(1), Some((ids(&[9, 10]), false)), "an exact window wins outright");
    // Two fuzzy windows, one clearly closer (by more than the margin).
    let mut s = s;
    s.paragraphs[10] = Paragraph::plain(ParaId(10), Dialogue, "I keep thinking about the old lighthouse, every night.");
    let text = block(&[(Character, "CARLA"), (Dialogue, "I keep thinking about the old lighthouse every night")]);
    let r = Review::open("z", &text, &s, |_| false, "");
    assert_eq!(r.items.first().map(|it| it.paras.clone()), Some(ids(&[9, 10])), "{:?}", r.unmatched);
    // What a fuzzy match was made against is the script's own text.
    assert_eq!(r.items[0].before_paras[1].text(), "I keep thinking about the old lighthouse, every night.");
}

#[test]
fn decided_block_without_para_reopens_from_its_decision() {
    let text = "Id: n\n---\nBEFORE:\n  CHARACTER: LAURA\n  DIALOGUE: Thank you very much.\n\
                AFTER:\n  DIALOGUE: Thanks!\n";
    let mut o = open(text);
    decide(&mut o, 0, 0, Decision::Accept(0)).unwrap();
    assert_eq!(status(&o, 0, 0), Status::Accepted(0));
    let back = reopen(&o);
    assert_eq!(status(&back, 0, 0), Status::Accepted(0));
}

#[test]
fn accepted_lines_block_another_entry() {
    let mut o = open(REVIEW);
    decide(&mut o, 0, 0, Decision::Accept(0)).unwrap();
    let new = o.doc.paragraphs()[7].id; // "She waves." (a fresh id)
    let entry = Entry { para: vec![new], ..Entry::single("She waves.", ["She waves slowly."]) };
    let many = o.session.append_entries(&o.doc, 0, vec![entry.clone()], true, |_| false, None).unwrap();
    assert!(matches!(many.results[0].unplaced, Some(Unplaced::Taken { entry: 0, .. })), "{:?}", many.results);
    // Reopened, the accepted block reserves its lines before other entries match.
    let back = reopen(&o);
    let mut back = back;
    let doc = back.doc.clone();
    let many = back.session.append_entries(&doc, 0, vec![entry], true, |_| false, None).unwrap();
    assert!(matches!(many.results[0].unplaced, Some(Unplaced::Taken { entry: 0, .. })));
}

#[test]
fn append_block_entries() {
    let mut o = open("Id: a\n");
    let entry = Entry {
        para: vec![ParaId(2), ParaId(3)],
        before: vec![
            Line::typed(ElementKind::Character, "CESAR"),
            Line::typed(ElementKind::Dialogue, "I am go to the town tomorrow."),
        ],
        options: vec![vec![
            Line::typed(ElementKind::Character, "CESAR"),
            Line::typed(ElementKind::Dialogue, "I'm going to town tomorrow."),
        ]],
        ..Entry::default()
    };
    let bad = Entry { options: vec![vec![Line::untyped("a"), Line::untyped("b")]], ..entry.clone() };
    let doc = o.doc.clone();
    let many = o.session.append_entries(&doc, 0, vec![entry.clone(), bad, entry], true, |_| false, None).unwrap();
    assert_eq!(many.results[0].entry, Some(0));
    assert!(
        matches!(&many.results[1].unplaced, Some(Unplaced::Invalid(why)) if !why.is_empty()),
        "can't round-trip: refused"
    );
    assert!(matches!(many.results[2].unplaced, Some(Unplaced::Taken { entry: 0, .. })));
    assert!(many.file_text.starts_with("SCRIPT REVIEW v2"), "{}", many.file_text);
    assert!(many.file_text.contains("PARA: 2, 3"), "{}", many.file_text);
    decide(&mut o, 0, 0, Decision::Accept(0)).unwrap();
    assert_eq!(lines(&o.doc), with(&ORIGINAL, 3, 1, &["Dialogue: I'm going to town tomorrow."]));
    assert_eq!(o.doc.paragraphs()[2].id, ParaId(2), "the unchanged line keeps its id");
}

#[test]
fn overlap_within_a_review_is_taken() {
    let text = "Id: o\n---\nPARA: 5, 6\nBEFORE:\n  CHARACTER: LAURA\n  DIALOGUE: Thank you very much.\nAFTER: [cut]\n\
                ---\nPARA: 6, 7\nBEFORE:\n  DIALOGUE: Thank you very much.\n  ACTION: She say hello to everybody and leaves.\n\
                AFTER: [cut]\n";
    let o = open(text);
    let r = &o.session.reviews[0];
    assert_eq!(r.items.len(), 1);
    assert_eq!(r.unmatched[0].why, Some(Unplaced::Taken { entry: 0, item: Some(0) }));
}

/// Changes ¶6 and ¶7 slightly, so both keep their ids when accepted.
const OTHER: &str = "Id: other\n---\nPARA: 6, 7\nBEFORE:\n  DIALOGUE: Thank you very much.\n  \
    ACTION: She say hello to everybody and leaves.\nAFTER:\n  DIALOGUE: Thank you very much!\n  \
    ACTION: She says hello to everybody and leaves.\n";

#[test]
fn accepting_over_lines_another_review_decided_is_refused() {
    let mut o = resume_many(&[REVIEW, OTHER], None);
    assert_eq!(o.session.reviews[1].items.len(), 1, "across reviews lines may overlap");
    decide(&mut o, 1, 0, Decision::Accept(0)).unwrap();
    let i = item(&o, 0, 0);
    let refused = o.session.check(&o.doc, 0, i, &Decision::Accept(0)).unwrap_err();
    assert!(matches!(&refused, Refusal::DecidedElsewhere { review, .. } if review == "r1"), "{refused:?}");
    assert!(o.session.can_apply(&o.doc, 0, i, &Decision::Accept(0)).unwrap_err().contains("undecide it there first"));
    assert!(decide(&mut o, 0, 0, Decision::Accept(0)).is_err());
    // Undecided there, it can be accepted here.
    decide(&mut o, 1, 0, Decision::Undecided).unwrap();
    assert_eq!(lines(&o.doc), ORIGINAL);
    decide(&mut o, 0, 0, Decision::Accept(1)).unwrap();
    // Its lines are gone now: the other review can't decide.
    let j = item(&o, 1, 0);
    assert_eq!(o.session.status(&o.doc, 1, j), Status::Pending, "¶7 is still there");
    let refused = o.session.check(&o.doc, 1, j, &Decision::Accept(0)).unwrap_err();
    assert!(matches!(refused, Refusal::DecidedElsewhere { .. } | Refusal::Restructured { .. }), "{refused:?}");
}

#[test]
fn lines_removed_by_another_review() {
    let other = "Id: other\n---\nPARA: 4\nBEFORE:\n  ACTION: They are playing truco (Argentinian cards game).\n\
                 AFTER:\n  ACTION: They play.\n  ACTION: Loudly.\n";
    let mut o = resume_many(&[REVIEW, other], None);
    // Cut ¶4 in the first review: the other review's block has no line left.
    decide(&mut o, 0, 1, Decision::Accept(0)).unwrap();
    let j = item(&o, 1, 0);
    assert_eq!(o.session.status(&o.doc, 1, j), Status::Removed);
    assert_eq!(o.session.status_label(&o.doc, 1, j), "line removed");
    assert_eq!(o.session.removed_by(&o.doc, 1, j), Some("r0"));
    let refused = o.session.check(&o.doc, 1, j, &Decision::Accept(0)).unwrap_err();
    assert!(matches!(&refused, Refusal::RemovedBy { review, .. } if review == "r0"), "{refused:?}");
}

#[test]
fn split_or_partly_removed_runs() {
    let mut o = open(REVIEW);
    let s = sel(&o);
    o.doc
        .edit(s, Group::Other, |e| {
            e.remove_para(ParaId(5))?;
            Ok((Selection::cursor(Pos::new(ParaId(6), 0)), ()))
        })
        .unwrap();
    let i = item(&o, 0, 0);
    assert_eq!(status(&o, 0, 0), Status::Pending);
    assert!(o.session.is_outdated(&o.doc, 0, i));
    let refused = o.session.check(&o.doc, 0, i, &Decision::Accept(0)).unwrap_err();
    assert_eq!(refused, Refusal::Restructured { para: ParaId(5) });
    assert!(o.session.check(&o.doc, 0, i, &Decision::KeepMine).is_ok());
    // Reopened, it is the same: an outdated item that can be kept, not accepted.
    let mut back = reopen(&o);
    assert!(back.session.reviews[0].unmatched.is_empty(), "{:?}", back.session.reviews[0].unmatched);
    let i = item(&back, 0, 0);
    assert_eq!(back.session.item(0, i).paras, vec![ParaId(5), ParaId(6)]);
    assert_eq!(status(&back, 0, 0), Status::Pending);
    assert!(back.session.is_outdated(&back.doc, 0, i));
    let refused = back.session.check(&back.doc, 0, i, &Decision::Accept(0)).unwrap_err();
    assert_eq!(refused, Refusal::Restructured { para: ParaId(5) });
    decide(&mut back, 0, 0, Decision::KeepMine).unwrap();
    assert_eq!(status(&back, 0, 0), Status::Rejected);
}

#[test]
fn partly_edited_runs_match_and_show_outdated() {
    let mut o = open(REVIEW);
    let s = sel(&o);
    o.doc.replace_text(s, ParaId(6), "Thank you so very much.", &[]).unwrap();
    let back = reopen(&o);
    let i = item(&back, 0, 0);
    assert_eq!(back.session.status(&back.doc, 0, i), Status::Pending);
    assert!(back.session.is_outdated(&back.doc, 0, i));
    let mut back = back;
    decide(&mut back, 0, 0, Decision::Accept(0)).unwrap();
    decide(&mut back, 0, 0, Decision::KeepMine).unwrap();
    assert_eq!(back.doc.paragraph(ParaId(6)).unwrap().text(), "Thank you very much.", "BEFORE as written");
}

#[test]
fn shapes_of_options() {
    let o = open(REVIEW);
    let it = |e: usize| o.session.item(0, item(&o, 0, e));
    assert_eq!(it(0).shape(0), Shape::Replace { n: 2, m: 3 });
    assert_eq!(it(0).shape(1), Shape::Cut { n: 2 });
    assert_eq!(it(1).shape(0), Shape::Cut { n: 1 });
    assert_eq!(it(1).shape(1), Shape::Replace { n: 1, m: 1 });
    assert_eq!(it(2).shape(0), Shape::Add { after: ParaId(1), n: 1 }, "first line is the anchor unchanged");
    assert_eq!(it(3).shape(0), Shape::Replace { n: 1, m: 1 });
    // The anchor changed (or retyped): a replacement, not an addition.
    let changed = "Id: c\n---\nPARA: 1\nBEFORE:\n  ACTION: Laura smiles.\nAFTER:\n  ACTION: Laura grins.\n  \
                   ACTION: Cesar nods.\nAFTER:\n  CHARACTER: Laura smiles.\n  ACTION: Cesar nods.\n";
    let c = open(changed);
    assert_eq!(c.session.item(0, 0).shape(0), Shape::Replace { n: 1, m: 2 });
    assert_eq!(c.session.item(0, 0).shape(1), Shape::Replace { n: 1, m: 2 });
    let line = open("Id: l\n---\nPARA: 7\nBEFORE: She say hello to everybody and leaves.\nAFTER: She says hello.\n");
    assert_eq!(line.session.item(0, 0).shape(0), Shape::Line);
}

#[test]
fn current_run_places_cuts() {
    let mut o = open(REVIEW);
    let i = item(&o, 0, 0);
    assert_eq!(
        o.session.current_run(&o.doc, 0, i),
        CurrentRun { ids: vec![ParaId(5), ParaId(6)], after: Some(ParaId(4)) },
        "pending: its lines"
    );
    // Cut ¶4: no lines, shown after ¶3.
    decide(&mut o, 0, 1, Decision::Accept(0)).unwrap();
    let j = item(&o, 0, 1);
    assert_eq!(o.session.live_run(&o.doc, 0, j), vec![]);
    assert_eq!(o.session.current_run(&o.doc, 0, j), CurrentRun { ids: vec![], after: Some(ParaId(3)) });
    // The anchor removed by hand: the line now before where it was.
    let s = sel(&o);
    o.doc
        .edit(s, Group::Other, |e| {
            e.remove_para(ParaId(3))?;
            Ok((Selection::cursor(Pos::new(ParaId(2), 0)), ()))
        })
        .unwrap();
    assert_eq!(o.session.current_run(&o.doc, 0, j), CurrentRun { ids: vec![], after: Some(ParaId(2)) });
    // A cut at the very start.
    let mut first = open("Id: f\n---\nPARA: 0\nBEFORE:\n  SCENE HEADING: EXT. FARM HOUSE - NIGHT\nAFTER: [cut]\n");
    decide(&mut first, 0, 0, Decision::Accept(0)).unwrap();
    assert_eq!(first.session.current_run(&first.doc, 0, 0), CurrentRun { ids: vec![], after: None });
}

#[test]
fn removed_by_survives_reopen() {
    let other = "Id: other\n---\nPARA: 4\nBEFORE:\n  ACTION: They are playing truco (Argentinian cards game).\n\
                 AFTER:\n  ACTION: They play.\n  ACTION: Loudly.\n";
    let mut o = resume_many(&[REVIEW, other], None);
    decide(&mut o, 0, 1, Decision::Accept(0)).unwrap();
    let back = reopen(&o);
    let r = &back.session.reviews[1];
    assert!(r.items.is_empty(), "its lines are gone, so it can't be placed");
    let why = r.unmatched[0].why.clone().expect("a reason");
    assert_eq!(why, Unplaced::RemovedBy { review: 0 });
    assert_eq!(back.session.unplaced_label(&why), "lines cut by r0");
    // Gone by hand, not by a review: no name.
    let mut h = resume_many(&[other], None);
    let s = sel(&h);
    h.doc
        .edit(s, Group::Other, |e| {
            e.remove_para(ParaId(4))?;
            Ok((Selection::cursor(Pos::new(ParaId(5), 0)), ()))
        })
        .unwrap();
    let back = reopen(&h);
    assert_ne!(back.session.reviews[0].unmatched[0].why, Some(Unplaced::RemovedBy { review: 0 }));
}

// ---- decisions over history: gone anchors, splits, keep, reopen ------------

/// e0 4→2 or 4→2 (¶2–5), e1 a cut of ¶6–7 (or one line), e2 an addition
/// after ¶1, e3 a line on ¶0.
const R2: &str = "SCRIPT REVIEW v2\nId: s\n\
---\nPARA: 2, 3, 4, 5\nBEFORE:\n  CHARACTER: CESAR\n  DIALOGUE: I am go to the town tomorrow.\n  ACTION: They are playing truco (Argentinian cards game).\n  CHARACTER: LAURA\n\
AFTER:\n  CHARACTER: CESAR\n  DIALOGUE: I'm off to town tomorrow.\n\
AFTER:\n  ACTION: They play cards.\n  CHARACTER: LAURA\n\
---\nPARA: 6, 7\nBEFORE:\n  DIALOGUE: Thank you very much.\n  ACTION: She say hello to everybody and leaves.\nAFTER: [cut]\nAFTER:\n  DIALOGUE: Thanks.\n\
---\nPARA: 1\nBEFORE:\n  ACTION: Laura smiles.\nAFTER:\n  ACTION: Laura smiles.\n  ACTION: Cesar nods.\n  ACTION: A dog barks.\n\
---\nPARA: 0\nBEFORE: EXT. FARM HOUSE - NIGHT\nAFTER: EXT. FARM HOUSE - DAY\n";

/// Every item's (entry, status) of review `r`, by entry.
fn statuses(o: &Opened, r: usize) -> Vec<(usize, Status)> {
    let mut v: Vec<(usize, Status)> = o.session.reviews[r]
        .items
        .iter()
        .enumerate()
        .map(|(i, it)| (it.entry, o.session.status(&o.doc, r, i)))
        .collect();
    v.sort_by_key(|x| x.0);
    v
}

/// Reopen and check every status, live run and MINE is the same.
fn check_reopen(o: &Opened, label: &str) -> Opened {
    let back = reopen(o);
    for r in 0..o.session.reviews.len() {
        assert_eq!(statuses(&back, r), statuses(o, r), "statuses after reopening at {label}");
        for (i, it) in o.session.reviews[r].items.iter().enumerate() {
            let j = item(&back, r, it.entry);
            assert_eq!(
                back.session.live_run(&back.doc, r, j),
                o.session.live_run(&o.doc, r, i),
                "{label} e{}",
                it.entry
            );
            assert_eq!(back.session.mine(&back.doc, r, j), o.session.mine(&o.doc, r, i), "{label} e{}", it.entry);
        }
    }
    back
}

#[test]
fn a_cut_anchored_on_a_new_line_that_went_stays_decidable() {
    let mut o = open(R2);
    decide(&mut o, 0, 0, Decision::Accept(0)).unwrap(); // ¶2–5 → [2, 8]
    decide(&mut o, 0, 1, Decision::Accept(0)).unwrap(); // cut ¶6–7, after:8
    assert_eq!(o.doc.meta("review/s/1"), Some("accepted:0@6,7=>after:8"));
    decide(&mut o, 0, 0, Decision::Accept(1)).unwrap(); // 2 and 8 removed
    assert!(o.doc.paragraph(ParaId(8)).is_none());
    let laura = o.doc.paragraphs().last().unwrap().id;
    // The cut is shown after the line now where its anchor was.
    let j = item(&o, 0, 1);
    assert_eq!(o.session.current_run(&o.doc, 0, j), CurrentRun { ids: vec![], after: Some(laura) });
    let mut back = check_reopen(&o, "cut on a gone anchor");
    for o in [&mut o, &mut back] {
        decide(o, 0, 1, Decision::KeepMine).unwrap();
        assert_eq!(
            lines(&o.doc)[3..],
            ["Character: LAURA", "Dialogue: Thank you very much.", "Action: She say hello to everybody and leaves."]
        );
        assert_eq!(status(o, 0, 1), Status::Rejected);
        decide(o, 0, 1, Decision::Accept(1)).unwrap();
        assert_eq!(lines(&o.doc).last().unwrap(), "Dialogue: Thanks.");
        decide(o, 0, 1, Decision::Accept(0)).unwrap();
        decide(o, 0, 1, Decision::Undecided).unwrap();
        assert_eq!(status(o, 0, 1), Status::Pending);
        assert_eq!(
            lines(&o.doc)[3..],
            ["Character: LAURA", "Dialogue: Thank you very much.", "Action: She say hello to everybody and leaves."]
        );
    }
}

#[test]
fn a_long_sequence_of_decisions_never_gets_stuck_and_survives_reopening() {
    let mut o = open(R2);
    let steps: Vec<(usize, Decision)> = vec![
        (0, Decision::Accept(0)),
        (1, Decision::Accept(0)),
        (2, Decision::Accept(0)),
        (3, Decision::Accept(0)),
        (0, Decision::Accept(1)),
        (1, Decision::KeepMine),
        (2, Decision::Undecided),
        (
            0,
            Decision::RewriteLines(vec![
                Line::typed(ElementKind::Action, "X."),
                Line::untyped("Y."),
                Line::untyped("Z."),
            ]),
        ),
        (1, Decision::Accept(1)),
        (1, Decision::Accept(0)),
        (3, Decision::KeepMine),
        (0, Decision::KeepMine),
        (1, Decision::Undecided),
        (0, Decision::Accept(0)),
        (2, Decision::Accept(0)),
        (0, Decision::Undecided),
        (2, Decision::KeepMine),
        (3, Decision::Undecided),
    ];
    let mut history = vec![lines(&o.doc)];
    for (n, (e, d)) in steps.into_iter().enumerate() {
        let label = format!("step {n}: e{e} {d:?}");
        decide(&mut o, 0, e, d).unwrap_or_else(|err| panic!("{label}: {err:?}"));
        history.push(lines(&o.doc));
        let back = check_reopen(&o, &label);
        if n % 2 == 1 {
            o = back;
        }
    }
    // Undo back through what this document's log holds, and redo.
    let last = history.len() - 1;
    let mut k = last;
    while k > 0 && o.doc.undo().is_some() {
        k -= 1;
        assert_eq!(lines(&o.doc), history[k], "undo to {k}");
    }
    while o.doc.redo().is_some() {
        k += 1;
        assert_eq!(lines(&o.doc), history[k], "redo to {k}");
    }
    assert_eq!(k, last);
}

/// A plain .fdx of `paras` (type, text).
fn fdx_of(paras: &[(&str, &str)]) -> String {
    let mut x = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\" ?>\n<FinalDraft DocumentType=\"Script\" \
         Template=\"No\" Version=\"5\">\n  <Content>\n",
    );
    for (k, t) in paras {
        x.push_str(&format!("    <Paragraph Type=\"{k}\">\n      <Text>{t}</Text>\n    </Paragraph>\n"));
    }
    x.push_str("  </Content>\n</FinalDraft>\n");
    x
}

fn resume_fdx(fx: &str, texts: &[&str], saved: Option<&str>) -> Opened {
    let f = FdxFile::parse(fx.to_string()).unwrap();
    let sources = texts
        .iter()
        .enumerate()
        .map(|(n, t)| ReviewSource { name: format!("r{n}"), path: format!("r{n}.review.txt"), text: t.to_string() })
        .collect();
    sessionfile::resume(
        sources,
        f.to_script(),
        Some(&f.legacy_script()),
        f.raw(),
        saved,
        &|k| f.is_caps(k.name()),
        "x.fdx",
        None,
    )
}

fn reopen_fdx(fx: &str, o: &Opened) -> Opened {
    let saved = o.to_file(fx).to_json();
    let texts: Vec<&str> = o.session.reviews.iter().map(|r| r.text()).collect();
    let back = resume_fdx(fx, &texts, Some(&saved));
    assert_eq!(back.doc.paragraphs(), o.doc.paragraphs());
    back
}

fn placed(o: &Opened) -> Vec<(usize, Vec<ParaId>)> {
    let mut v: Vec<_> = o.session.reviews[0].items.iter().map(|i| (i.entry, i.paras.clone())).collect();
    v.sort();
    v
}

#[test]
fn decided_before_only_blocks_reopen_where_they_were_decided_despite_repeated_lines() {
    let fx = fdx_of(&[
        ("Scene Heading", "INT. ROOM - DAY"),
        ("Character", "BEN"),
        ("Dialogue", "Not yet."),
        ("Action", "Beat."),
        ("Character", "BEN"),
        ("Dialogue", "Not yet."),
        ("Action", "Silence."),
    ]);
    // e0 cuts ¶1–2 by PARA; e1 (no PARA) reads the same, so it lands on ¶4–5.
    let text = "SCRIPT REVIEW v2\nId: q\n---\nPARA: 1, 2\nBEFORE:\n  CHARACTER: BEN\n  DIALOGUE: Not yet.\n\
                AFTER: [cut]\n---\nBEFORE:\n  CHARACTER: BEN\n  DIALOGUE: Not yet.\nAFTER:\n  CHARACTER: BEN\n  \
                DIALOGUE: Now!\n";
    let mut o = resume_fdx(&fx, &[text], None);
    assert_eq!(placed(&o), vec![(0, vec![ParaId(1), ParaId(2)]), (1, vec![ParaId(4), ParaId(5)])]);
    decide(&mut o, 0, 1, Decision::Accept(0)).unwrap();
    decide(&mut o, 0, 0, Decision::Accept(0)).unwrap();
    let back = reopen_fdx(&fx, &o);
    assert!(back.session.reviews[0].unmatched.is_empty(), "{:?}", back.session.reviews[0].unmatched);
    assert_eq!(placed(&back), vec![(0, vec![]), (1, vec![ParaId(4), ParaId(7)])], "placed from the decisions");
    assert_eq!(statuses(&back, 0), vec![(0, Status::Accepted(0)), (1, Status::Accepted(0))]);
    // Pending, with the other block's lines cut: still ¶4–5.
    let mut o = resume_fdx(&fx, &[text], None);
    decide(&mut o, 0, 0, Decision::Accept(0)).unwrap();
    let back = reopen_fdx(&fx, &o);
    assert!(back.session.reviews[0].unmatched.is_empty(), "{:?}", back.session.reviews[0].unmatched);
    assert_eq!(placed(&back)[1], (1, vec![ParaId(4), ParaId(5)]));
    // Decided first on ¶4–5, then the other block undecided: it stays there.
    let mut o = resume_fdx(&fx, &[text], None);
    decide(&mut o, 0, 1, Decision::KeepMine).unwrap();
    let back = reopen_fdx(&fx, &o);
    assert_eq!(placed(&back), placed(&o));
    assert_eq!(statuses(&back, 0), vec![(0, Status::Pending), (1, Status::Rejected)]);
}

#[test]
fn undeciding_after_keep_changes_only_the_decision() {
    // A partly removed block, kept, then a line edited by hand.
    let mut o = open(R2);
    let s = sel(&o);
    o.doc.edit(s, Group::Other, |e| e.remove_para(ParaId(3)).map(|_| (s, ()))).unwrap();
    decide(&mut o, 0, 0, Decision::KeepMine).unwrap();
    o.doc.replace_text(s, ParaId(4), "They play truco.", &[]).unwrap();
    let mut back = check_reopen(&o, "kept");
    for o in [&mut o, &mut back] {
        let before = lines(&o.doc);
        decide(o, 0, 0, Decision::Undecided).unwrap();
        assert_eq!(lines(&o.doc), before, "no line restored, no hand edit reverted");
        assert_eq!(status(o, 0, 0), Status::Pending);
        decide(o, 0, 0, Decision::KeepMine).unwrap();
        decide(o, 0, 0, Decision::KeepMine).unwrap();
        assert_eq!(lines(&o.doc), before);
    }
}

#[test]
fn a_hand_split_of_the_last_accepted_line_stays_in_its_run() {
    let mut o = open(R2);
    decide(&mut o, 0, 0, Decision::Accept(0)).unwrap(); // [2, 8]
    let s = sel(&o);
    let tail = o.doc.edit(s, Group::Other, |e| e.split(Pos::new(ParaId(8), 5)).map(|t| (s, t))).unwrap();
    o.doc.replace_text(s, ParaId(2), "CESAR (V.O.)", &[]).unwrap();
    let i = item(&o, 0, 0);
    assert_eq!(o.session.live_run(&o.doc, 0, i), vec![ParaId(2), ParaId(8), tail]);
    let mine: Vec<String> = o.session.mine(&o.doc, 0, i).into_iter().map(|l| l.text).collect();
    assert_eq!(mine, ["CESAR (V.O.)", "I'm o", "ff to town tomorrow."]);
    assert_eq!(status(&o, 0, 0), Status::Rewritten);
    // Progress applied to a fresh script reads the same.
    let fresh = open(&o.session.save_progress(&o.doc, 0));
    assert_eq!(lines(&fresh.doc), lines(&o.doc));
    assert_eq!(statuses(&fresh, 0), statuses(&o, 0));
    let mut back = check_reopen(&o, "split");
    for o in [&mut o, &mut back] {
        decide(o, 0, 0, Decision::KeepMine).unwrap();
        assert_eq!(lines(&o.doc), ORIGINAL, "the tail goes with the rest");
    }
    // Enter at the end of the line adds a line after the run, not to it.
    let mut o = open(R2);
    decide(&mut o, 0, 0, Decision::Accept(0)).unwrap();
    let s = sel(&o);
    let end = o.doc.paragraph(ParaId(8)).unwrap().text().len();
    o.doc.edit(s, Group::Other, |e| e.split(Pos::new(ParaId(8), end)).map(|_| (s, ()))).unwrap();
    assert_eq!(o.session.live_run(&o.doc, 0, item(&o, 0, 0)), vec![ParaId(2), ParaId(8)]);
}

#[test]
fn every_accepted_line_removed_by_hand_round_trips_through_progress() {
    let mut o = open(R2);
    decide(&mut o, 0, 0, Decision::Accept(0)).unwrap(); // [2, 8]
    let s = sel(&o);
    o.doc
        .edit(s, Group::Other, |e| {
            e.remove_para(ParaId(2))?;
            e.remove_para(ParaId(8))?;
            Ok((s, ()))
        })
        .unwrap();
    assert_eq!(o.session.status_label(&o.doc, 0, item(&o, 0, 0)), "line cut");
    let mut back = check_reopen(&o, "all gone");
    let fresh = open(&back.session.save_progress(&back.doc, 0));
    assert_eq!(lines(&fresh.doc), lines(&o.doc));
    assert_eq!(statuses(&fresh, 0), statuses(&o, 0));
    decide(&mut back, 0, 0, Decision::KeepMine).unwrap();
    assert_eq!(lines(&back.doc), ORIGINAL);
}

#[test]
fn partly_removed_by_another_review_is_named_before_and_after_reopening() {
    // The other review keeps ¶6 and ¶7's ids; this one cuts ¶5–6.
    let mut o = resume_many(&[REVIEW, OTHER], None);
    decide(&mut o, 0, 0, Decision::Accept(1)).unwrap();
    let j = item(&o, 1, 0);
    assert_eq!(o.session.removed_by(&o.doc, 1, j), Some("r0"));
    assert!(o.session.is_outdated(&o.doc, 1, j));
    let back = check_reopen(&o, "partly removed");
    let j = item(&back, 1, 0);
    assert_eq!(back.session.removed_by(&back.doc, 1, j), Some("r0"));
    assert!(back.session.is_outdated(&back.doc, 1, j));
    assert!(back.session.check(&back.doc, 1, j, &Decision::KeepMine).is_ok());
}

#[test]
fn undeciding_over_lines_another_review_decided_since_is_refused() {
    // Review B: a line suggestion on ¶2, which A's accept keeps.
    let b = "Id: bb\n---\nPARA: 2\nBEFORE: CESAR\nAFTER: CESAR (V.O.)\n";
    let mut o = resume_many(&[R2, b], None);
    decide(&mut o, 0, 0, Decision::Accept(0)).unwrap();
    decide(&mut o, 1, 0, Decision::Accept(0)).unwrap();
    let i = item(&o, 0, 0);
    for d in [Decision::Undecided, Decision::KeepMine] {
        let refused = o.session.check(&o.doc, 0, i, &d).unwrap_err();
        assert_eq!(refused, Refusal::DecidedElsewhere { para: ParaId(2), review: "r1".into() });
        assert!(decide(&mut o, 0, 0, d).is_err());
    }
    assert_eq!(status(&o, 1, 0), Status::Accepted(0));
    decide(&mut o, 1, 0, Decision::Undecided).unwrap();
    decide(&mut o, 0, 0, Decision::Undecided).unwrap();
    assert_eq!(lines(&o.doc), ORIGINAL);
}

#[test]
fn typed_mine_lines_are_written_trimmed() {
    // Edge spaces carry nothing in screenplay text: progress trims them, as
    // a one-line MINE does, and the status reads the same.
    let mut o = open(R2);
    decide(&mut o, 0, 0, Decision::Accept(0)).unwrap();
    let s = sel(&o);
    o.doc
        .edit(s, Group::Other, |e| {
            e.insert_para(3, scriptorium_core::NewPara::plain(ElementKind::Action, "  indented "))?;
            Ok((s, ()))
        })
        .unwrap();
    let fresh = open(&o.session.save_progress(&o.doc, 0));
    let trimmed: Vec<String> = lines(&o.doc).into_iter().map(|l| l.replace(":   indented ", ": indented")).collect();
    assert_eq!(lines(&fresh.doc), trimmed);
    assert_eq!(statuses(&fresh, 0), statuses(&o, 0));
}
