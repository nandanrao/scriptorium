//! `ROW:` entries (suggestions for rows of a pair of versions) are placed
//! only by the host's resolver, here a fake one standing in for
//! `versions::Resolver`: never by `PARA:` or `BEFORE`.

use scriptorium_core::{Document, ElementKind, ParaId, Paragraph, Pos, Script, Selection};
use scriptorium_review::sessionfile::{self, ResumedSession, ReviewSource, RowResolverFactory};
use scriptorium_review::{Decision, Entry, Line, NO_LINKS, Review, RowRef, RowResolver, Session, Status, Unplaced};

const LINKS_ID: &str = "lk-1";
const CONTENTS: &str = "the version's file";
const OTHER_PAIR: &str = "this suggestion is for another pair of versions";
const CHANGED: &str = "the English line changed since this suggestion";
const READS_NONE: &str = "the line no longer reads what this suggestion was made for";

/// The rows: the original's side (its texts) | the version's cell.
fn rows() -> Vec<(Vec<&'static str>, Vec<u64>)> {
    vec![
        (vec!["INT. FERRY TERMINAL - NIGHT"], vec![0]),
        (vec!["TOMÁS"], vec![1]),
        (vec!["You're not getting on that boat."], vec![2]),
        (vec!["LUCÍA"], vec![3]),
        (vec!["Watch me."], vec![4]),
        (vec!["TOMÁS"], vec![5]),
        (vec!["Yes."], vec![6]),
        (vec!["LUCÍA"], vec![7]),
        (vec!["Yes."], vec![8]),
    ]
}

/// The version: a placeholder (headings and names copied, the rest blank).
fn version(dialogue: &[(u64, &str)]) -> Script {
    use ElementKind::*;
    let mut ps = vec![
        Paragraph::plain(ParaId(0), SceneHeading, "INT. FERRY TERMINAL - NIGHT"),
        Paragraph::plain(ParaId(1), Character, "TOMÁS"),
        Paragraph::plain(ParaId(2), Dialogue, ""),
        Paragraph::plain(ParaId(3), Character, "LUCÍA"),
        Paragraph::plain(ParaId(4), Dialogue, ""),
        Paragraph::plain(ParaId(5), Character, "TOMÁS"),
        Paragraph::plain(ParaId(6), Dialogue, ""),
        Paragraph::plain(ParaId(7), Character, "LUCÍA"),
        Paragraph::plain(ParaId(8), Dialogue, ""),
    ];
    for &(id, text) in dialogue {
        ps[id as usize] = Paragraph::plain(ParaId(id), Dialogue, text);
    }
    Script { paragraphs: ps, id_floor: 9, ..Script::default() }
}

fn caps(k: &ElementKind) -> bool {
    matches!(k, ElementKind::SceneHeading | ElementKind::Character)
}

/// A fake `versions::Resolver` over `doc`, following its contract: rows whose
/// original side reads `ROW TEXT` and whose version side reads one of the
/// texts; those reading the first text first; within each group, the cell
/// holding the preferred ids first.
fn fake(doc: &Document) -> Box<RowResolver<'static>> {
    let live: Vec<(ParaId, String)> = doc.paragraphs().iter().map(|p| (p.id, p.text())).collect();
    Box::new(move |row: &RowRef, texts: &[Vec<Line>], prefer: &[ParaId]| {
        if row.id != LINKS_ID {
            return Err(OTHER_PAIR.to_string());
        }
        let rows: Vec<Vec<u64>> = rows()
            .into_iter()
            .filter(|(o, _)| o.iter().map(|t| t.to_string()).collect::<Vec<_>>() == row.texts)
            .map(|(_, cell)| cell)
            .collect();
        if rows.is_empty() {
            return Err(CHANGED.to_string());
        }
        let reads = |want: &[Line], cell: &[u64]| {
            want.len() == cell.len()
                && want
                    .iter()
                    .zip(cell)
                    .all(|(l, id)| live.iter().any(|(p, t)| p.0 == *id && t.trim() == l.text.trim()))
        };
        let (mut first, mut rest) = (vec![], vec![]);
        for cell in rows {
            match texts.iter().position(|t| reads(t, &cell)) {
                Some(0) => first.push(cell.into_iter().map(ParaId).collect::<Vec<_>>()),
                Some(_) => rest.push(cell.into_iter().map(ParaId).collect()),
                None => {}
            }
        }
        for group in [&mut first, &mut rest] {
            if let Some(i) = group.iter().position(|c| !prefer.is_empty() && prefer.iter().all(|p| c.contains(p))) {
                let c = group.remove(i);
                group.insert(0, c);
            }
        }
        first.extend(rest);
        if first.is_empty() { Err(READS_NONE.to_string()) } else { Ok(first) }
    })
}

fn row_entry(row_text: &str, options: &[&str]) -> Entry {
    Entry { row: Some(RowRef { id: LINKS_ID.into(), texts: vec![row_text.into()] }), ..Entry::single("", options) }
}

fn file(blocks: &[&str]) -> String {
    let mut s = "SCRIPT REVIEW v1\nScript: PURPLE LIST.es.fdx\nId: tr-1\n".to_string();
    for b in blocks {
        s.push_str("\n---\n");
        s.push_str(b);
        s.push('\n');
    }
    s
}

fn open(text: &str, doc: &Document) -> Review {
    let r = fake(doc);
    Review::open_live("t.review.txt", text, &version(&[]), doc, caps, "", Some(&*r))
}

fn resume(script: Script, text: &str, saved: Option<&str>) -> ResumedSession {
    let factory: &RowResolverFactory = &|doc: &Document| fake(doc);
    sessionfile::resume(
        vec![ReviewSource { name: "t.review.txt".into(), path: "t.review.txt".into(), text: text.into() }],
        script,
        None,
        CONTENTS,
        saved,
        &caps,
        "",
        Some(factory),
    )
}

fn placed(r: &Review) -> Vec<(usize, u64, &str)> {
    r.items.iter().map(|it| (it.entry, it.para.0, it.before.as_str())).collect()
}

fn why(r: &Review) -> Vec<(usize, Option<Unplaced>)> {
    r.unmatched.iter().map(|u| (u.entry, u.why.clone())).collect()
}

fn line(doc: &Document, id: u64) -> String {
    doc.paragraph(ParaId(id)).unwrap().text()
}

fn decide(o: &mut ResumedSession, i: usize, d: Decision) {
    let s = Selection::cursor(o.doc.start());
    o.session.decide(&mut o.doc, s, 0, i, d).unwrap();
}

#[test]
fn placed_where_the_resolver_says_never_by_para_or_before() {
    let doc = Document::new(version(&[]));
    let r = open(
        &file(&[
            // PARA names another blank line: the row decides.
            "BEFORE: [empty]\nPARA: 8\nAFTER: Mirá cómo lo hago.\nAFTER: Ya vas a ver.\nWHY:\nROW: lk-1\nROW TEXT: Watch me.",
            "BEFORE: [empty]\nAFTER: No vas a subir a ese barco.\nWHY:\nROW: lk-1\nROW TEXT: You're not getting on that boat.",
            // An ordinary entry is still matched by its text.
            "BEFORE: LUCÍA\nPARA: 7\nAFTER: LUCIA\nWHY: plain",
        ]),
        &doc,
    );
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert_eq!(placed(&r), [(1, 2, ""), (0, 4, ""), (2, 7, "LUCÍA")]);
    assert_eq!(r.items[1].options, ["Mirá cómo lo hago.", "Ya vas a ver."]);
    assert_eq!(r.items[1].row.as_ref().unwrap().texts, ["Watch me."]);
    assert!(r.unmatched.is_empty());
}

#[test]
fn not_found_says_why() {
    let doc = Document::new(version(&[(4, "Lo que sea.")]));
    let text = file(&[
        "BEFORE: [empty]\nAFTER: Algo.\nWHY:\nROW: lk-1\nROW TEXT: A line reworded since.",
        "BEFORE: [empty]\nAFTER: Algo.\nWHY:\nROW: lk-other\nROW TEXT: Watch me.",
        // Typed into by hand since: the line reads none of its texts.
        "BEFORE: [empty]\nAFTER: Mirá cómo lo hago.\nWHY:\nROW: lk-1\nROW TEXT: Watch me.",
        // A ROW entry whose BEFORE reads a line elsewhere still isn't placed by it.
        "BEFORE: TOMÁS\nAFTER: TOMAS\nWHY:\nROW: lk-1\nROW TEXT: Someone else.",
    ]);
    let r = open(&text, &doc);
    assert!(r.items.is_empty(), "{:?}", placed(&r));
    let no = |s: &str| Some(Unplaced::NoRow(s.to_string()));
    assert_eq!(why(&r), [(0, no(CHANGED)), (1, no(OTHER_PAIR)), (2, no(READS_NONE)), (3, no(CHANGED))]);
    let session = Session::new(vec![r]);
    assert_eq!(session.unplaced_label(&Unplaced::NoRow(CHANGED.into())), CHANGED);

    // Without a resolver (existing callers), ROW entries aren't placed at all.
    let r = Review::open_live("t.review.txt", &text, &version(&[]), &doc, caps, "", None);
    assert!(r.items.is_empty());
    assert!(r.unmatched.iter().all(|u| u.why == Some(Unplaced::NoRow(NO_LINKS.into()))));
}

#[test]
fn two_entries_never_on_one_cell() {
    let doc = Document::new(version(&[]));
    let r = open(
        &file(&[
            "BEFORE: [empty]\nAFTER: Mirá.\nWHY:\nROW: lk-1\nROW TEXT: Watch me.",
            "BEFORE: [empty]\nAFTER: Ya vas a ver.\nWHY:\nROW: lk-1\nROW TEXT: Watch me.",
            // Two rows read the same: each entry its own, PARA picking first.
            "BEFORE: [empty]\nAFTER: Sí.\nWHY:\nROW: lk-1\nROW TEXT: Yes.",
            "BEFORE: [empty]\nPARA: 8\nAFTER: Sí.\nWHY:\nROW: lk-1\nROW TEXT: Yes.",
            "BEFORE: [empty]\nAFTER: Sí.\nWHY:\nROW: lk-1\nROW TEXT: Yes.",
        ]),
        &doc,
    );
    assert_eq!(placed(&r), [(0, 4, ""), (2, 6, ""), (3, 8, "")]);
    let taken = |entry| Some(Unplaced::Taken { entry, item: Some(0) });
    assert_eq!(why(&r)[0], (1, taken(0)));
    assert_eq!(why(&r)[1].0, 4);
    assert!(matches!(why(&r)[1].1, Some(Unplaced::Taken { .. })));
}

#[test]
fn accept_undo_redo_undecide_on_a_blank() {
    let text = file(&[
        "BEFORE: [empty]\nAFTER: Mirá cómo lo hago.\nAFTER: Ya vas a ver.\nWHY:\nROW: lk-1\nROW TEXT: Watch me.",
    ]);
    let mut o = resume(version(&[]), &text, None);
    assert_eq!(placed(&o.session.reviews[0]), [(0, 4, "")]);
    assert_eq!(o.session.status(&o.doc, 0, 0), Status::Pending);
    decide(&mut o, 0, Decision::Accept(1));
    assert_eq!(line(&o.doc, 4), "Ya vas a ver.");
    assert_eq!(o.session.status(&o.doc, 0, 0), Status::Accepted(1));
    o.doc.undo().unwrap();
    assert_eq!((line(&o.doc, 4), o.session.status(&o.doc, 0, 0)), (String::new(), Status::Pending));
    o.doc.redo().unwrap();
    assert_eq!(o.session.status(&o.doc, 0, 0), Status::Accepted(1));
    decide(&mut o, 0, Decision::Undecided);
    assert_eq!((line(&o.doc, 4), o.session.status(&o.doc, 0, 0)), (String::new(), Status::Pending));
    decide(&mut o, 0, Decision::Rewrite("Mirame.".into()));
    assert_eq!(o.session.status(&o.doc, 0, 0), Status::Rewritten);
}

#[test]
fn decided_status_comes_from_the_metadata_after_resuming() {
    let text = file(&[
        "BEFORE: [empty]\nAFTER: Mirá cómo lo hago.\nAFTER: Ya vas a ver.\nWHY:\nROW: lk-1\nROW TEXT: Watch me.",
        "BEFORE: [empty]\nAFTER: Sí.\nWHY:\nROW: lk-1\nROW TEXT: Yes.",
        "BEFORE: [empty]\nAFTER: Sí.\nWHY:\nROW: lk-1\nROW TEXT: Yes.",
        "BEFORE: [empty]\nAFTER: No vas a subir.\nWHY:\nROW: lk-1\nROW TEXT: You're not getting on that boat.",
    ]);
    let mut o = resume(version(&[]), &text, None);
    assert_eq!(placed(&o.session.reviews[0]), [(3, 2, ""), (0, 4, ""), (1, 6, ""), (2, 8, "")]);
    decide(&mut o, 1, Decision::Accept(1));
    // The second "Yes." row: the first stays pending and blank.
    decide(&mut o, 3, Decision::Accept(0));
    decide(&mut o, 0, Decision::Rewrite("No subís.".into()));
    let saved = o.to_file(CONTENTS).to_json();

    let back = resume(version(&[]), &text, Some(&saved));
    assert!(matches!(back.how, sessionfile::Resumed::Resumed { .. }), "{:?}", back.how);
    let r = &back.session.reviews[0];
    assert!(r.unmatched.is_empty(), "{:?}", why(r));
    // The decided "Yes." keeps its row; the pending one takes the blank one.
    assert_eq!(placed(r), [(3, 2, ""), (0, 4, ""), (1, 6, ""), (2, 8, "")]);
    let statuses: Vec<Status> = (0..4).map(|i| back.session.status(&back.doc, 0, i)).collect();
    assert_eq!(statuses, [Status::Rewritten, Status::Accepted(1), Status::Pending, Status::Accepted(0)]);
    assert_eq!(line(&back.doc, 8), "Sí.");
    assert_eq!(line(&back.doc, 6), "");

    // Kept, then typed into by hand: not found any more.
    let mut o = resume(version(&[]), &text, Some(&saved));
    decide(&mut o, 1, Decision::KeepMine);
    let s = Selection::cursor(Pos::new(ParaId(4), 0));
    o.doc.replace_text(s, ParaId(4), "Lo escribí yo.", &[]).unwrap();
    let back = resume(version(&[]), &text, Some(&o.to_file(CONTENTS).to_json()));
    assert_eq!(why(&back.session.reviews[0]), [(0, Some(Unplaced::NoRow(READS_NONE.into())))]);
}

#[test]
fn decisions_in_the_file_apply_on_a_fresh_open() {
    let text = file(&[
        "BEFORE: [empty]\nAFTER: Mirá cómo lo hago.\nAFTER: Ya vas a ver.\nWHY:\nROW: lk-1\nROW TEXT: Watch me.\nSTATUS: accepted\nCHOSEN: 2",
        "BEFORE: [empty]\nAFTER: Sí.\nWHY:\nROW: lk-1\nROW TEXT: Yes.",
        "BEFORE: [empty]\nAFTER: Sí.\nWHY:\nROW: lk-1\nROW TEXT: Yes.\nSTATUS: accepted\nCHOSEN: 1",
    ]);
    // The version's file doesn't have them yet: applied on the blank lines.
    let o = resume(version(&[]), &text, None);
    assert_eq!(placed(&o.session.reviews[0]), [(0, 4, ""), (2, 6, ""), (1, 8, "")]);
    assert_eq!(line(&o.doc, 4), "Ya vas a ver.");
    assert_eq!(o.session.status(&o.doc, 0, 0), Status::Accepted(1));

    // Final Draft saved them into the file (C2): found where they read, the
    // pending "Yes." on the other row, and undeciding goes back to blank.
    let mut o = resume(version(&[(4, "Ya vas a ver."), (8, "Sí.")]), &text, None);
    assert_eq!(placed(&o.session.reviews[0]), [(0, 4, ""), (1, 6, ""), (2, 8, "")]);
    assert_eq!(o.session.status(&o.doc, 0, 0), Status::Accepted(1));
    assert_eq!(o.session.status(&o.doc, 0, 2), Status::Accepted(0));
    assert_eq!(o.session.status(&o.doc, 0, 1), Status::Pending);
    decide(&mut o, 0, Decision::Undecided);
    assert_eq!(line(&o.doc, 4), "");
}

#[test]
fn appending_places_by_row_and_allows_a_blank_before() {
    let doc = Document::new(version(&[]));
    let mut session = Session::new(vec![Review::open("t.review.txt", &file(&[]), &version(&[]), caps, "")]);
    let entries = vec![
        Entry { para: vec![ParaId(8)], ..row_entry("Watch me.", &["Mirá cómo lo hago."]) },
        row_entry("Watch me.", &["Otra."]),
        row_entry("Not a line of the original.", &["Algo."]),
    ];
    // Without a resolver, a blank BEFORE is refused as before.
    assert!(session.clone().append_entries(&doc, 0, entries.clone(), true, caps, None).is_err());

    let rows = fake(&doc);
    let done = session.append_entries(&doc, 0, entries, true, caps, Some(&*rows)).unwrap();
    assert_eq!(done.results[0].item, Some(0));
    assert!(matches!(done.results[1].unplaced, Some(Unplaced::Taken { entry: 0, .. })));
    assert_eq!(done.results[2].unplaced, Some(Unplaced::NoRow(CHANGED.into())));
    // Written with the row's line as its PARA, BEFORE still [empty].
    assert!(
        done.file_text
            .contains("BEFORE: [empty]\nPARA: 4\nAFTER: Mirá cómo lo hago.\nWHY:\nROW: lk-1\nROW TEXT: Watch me.\n"),
        "{}",
        done.file_text
    );
    assert_eq!(done.file_text.matches("---").count(), 1);
    let back = open(&done.file_text, &doc);
    assert_eq!(placed(&back), [(0, 4, "")]);
}

#[test]
fn decided_in_the_document_before_decided_only_in_the_file() {
    let pending = "BEFORE: [empty]\nAFTER: Sí.\nWHY:\nROW: lk-1\nROW TEXT: Yes.";
    let mut o = resume(version(&[]), &file(&[pending, pending]), None);
    assert_eq!(placed(&o.session.reviews[0]), [(0, 6, ""), (1, 8, "")]);
    decide(&mut o, 1, Decision::Accept(0));
    let saved = o.to_file(CONTENTS).to_json();
    // The file says entry 0 was accepted (an older progress write); the
    // session holds entry 1's acceptance on ¶8. Entry 0 must not take ¶8.
    let in_file = "BEFORE: [empty]\nAFTER: Sí.\nWHY:\nROW: lk-1\nROW TEXT: Yes.\nSTATUS: accepted\nCHOSEN: 1";
    let back = resume(version(&[]), &file(&[in_file, pending]), Some(&saved));
    let r = &back.session.reviews[0];
    assert_eq!(placed(r), [(0, 6, ""), (1, 8, "")]);
    let i8 = r.items.iter().position(|it| it.entry == 1).unwrap();
    assert_eq!(back.session.status(&back.doc, 0, i8), Status::Accepted(0));
}

/// A resolver that always gives `cells`.
fn fixed(cells: &[&[u64]]) -> Box<RowResolver<'static>> {
    let cells: Vec<Vec<ParaId>> = cells.iter().map(|c| c.iter().copied().map(ParaId).collect()).collect();
    Box::new(move |_: &RowRef, _: &[Vec<Line>], _: &[ParaId]| Ok(cells.clone()))
}

fn open_with(text: &str, doc: &Document, rows: &RowResolver) -> Review {
    Review::open_live("t.review.txt", text, &version(&[]), doc, caps, "", Some(rows))
}

#[test]
fn candidates_that_dont_fit_say_why() {
    let doc = Document::new(version(&[]));
    let text = file(&["BEFORE: [empty]\nAFTER: Sí.\nWHY:\nROW: lk-1\nROW TEXT: Yes."]);
    let no = |r: &Review| match &r.unmatched[..] {
        [u] => match &u.why {
            Some(Unplaced::NoRow(why)) => why.clone(),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    };
    assert!(no(&open_with(&text, &doc, &*fixed(&[&[6, 7]]))).contains("several lines"));
    let block = file(&[
        "BEFORE:\n  DIALOGUE: \n  CHARACTER: LUCÍA\nPARA: 6, 7\nAFTER:\n  DIALOGUE: Sí.\n  CHARACTER: LUCÍA\nWHY:\nROW: lk-1\nROW TEXT: Yes.\nROW TEXT: LUCÍA",
    ]);
    assert!(no(&open_with(&block, &doc, &*fixed(&[&[6]]))).contains("different number of lines"));
}

#[test]
fn a_decided_block_stays_only_on_the_lines_its_decision_left() {
    let block = file(&[
        "BEFORE:\n  DIALOGUE: \n  CHARACTER: LUCÍA\nAFTER:\n  DIALOGUE: Sí.\n  CHARACTER: LUCÍA\nWHY:\nROW: lk-1\nROW TEXT: Yes.\nROW TEXT: LUCÍA",
    ]);
    let mut doc = Document::new(version(&[]));
    let r = open_with(&block, &doc, &*fixed(&[&[6, 7]]));
    assert_eq!(placed(&r), [(0, 6, " / LUCÍA")]);
    let session = Session::new(vec![r]);
    let s = Selection::cursor(doc.start());
    session.decide(&mut doc, s, 0, 0, Decision::Accept(0)).unwrap();
    let now = session.live_run(&doc, 0, 0);
    assert_eq!(now.iter().map(|&id| line(&doc, id.0)).collect::<Vec<_>>(), ["Sí.", "LUCÍA"]);
    let now_u: Vec<u64> = now.iter().map(|p| p.0).collect();

    // On the row holding what it left: placed, accepted.
    let back = Session::new(vec![open_with(&block, &doc, &*fixed(&[&now_u]))]);
    assert_eq!(back.reviews[0].items.len(), 1, "{:?}", why(&back.reviews[0]));
    assert_eq!(back.status(&doc, 0, 0), Status::Accepted(0));
    // The resolver gives another row (its own was typed into): not found.
    let other = open_with(&block, &doc, &*fixed(&[&[2, 3]]));
    assert!(other.items.is_empty());
    assert!(matches!(&other.unmatched[0].why, Some(Unplaced::NoRow(w)) if w.contains("decision left")));
}
