//! Matching, decisions and saving progress, on a small hand-written script.
//! Cases ported from script-review's match.test.js and review.test.js.

use scriptorium_core::{Document, ParaId, Selection};
use scriptorium_fdx::FdxFile;
use scriptorium_review::matching::{Para, match_entries};
use scriptorium_review::{Decision, Entry, FileStatus, Review, ReviewFile, Session, Status};

fn para(kind: &str, runs: &[&str]) -> String {
    let runs: Vec<String> = runs
        .iter()
        .map(|r| match r.strip_prefix("I:") {
            Some(t) => format!("      <Text Style=\"Italic\">{t}</Text>"),
            None => format!("      <Text>{r}</Text>"),
        })
        .collect();
    format!("    <Paragraph Type=\"{kind}\">\n{}\n    </Paragraph>\n", runs.join("\n"))
}

fn sample() -> String {
    let body = [
        para("Scene Heading", &["EXT. FARM HOUSE - NIGHT"]),
        para("Action", &["They are playing ", "I:truco", " (Argentinian cards game)."]),
        para("Character", &["CESAR"]),
        para("Dialogue", &["I am go to the town tomorrow."]),
        para("Parenthetical", &["(smiling)"]),
        para("Dialogue", &["Thank you."]),
        para("Action", &["Laura smiles. "]),
        para("Character", &["LAURA"]),
        para("Dialogue", &["Thank you."]),
        para("Action", &["She say “hello” to everybody &amp; leaves."]),
    ]
    .concat();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\" ?>\n<FinalDraft DocumentType=\"Script\" Template=\"No\" Version=\"5\">\n\n  <Content>\n{body}  </Content>\n</FinalDraft>\n"
    )
}

const REVIEW: &str = "SCRIPT REVIEW v1
Script: sample.fdx

---
BEFORE: They are playing truco (Argentinian cards game).
AFTER: They are playing truco (an Argentinian card game).
WHY: article; \"card game\"
---
BEFORE: I am go to the town tomorrow.
AFTER: I am going to town tomorrow.
AFTER: I'm going to town tomorrow.
WHY: verb form
---
BEFORE: Not in this script.
AFTER: Whatever.
---
BEFORE: She say \"hello\" to everybody & leaves.
AFTER: She says \"hello\" to everybody & leaves.
";

fn e(before: &str, options: &[&str]) -> Entry {
    Entry { before: before.into(), options: options.iter().map(|s| s.to_string()).collect(), ..Entry::default() }
}

/// (index, fuzzy, options, option_src) per entry.
type Found = Option<(usize, bool, Vec<String>, Vec<usize>)>;

fn run_match(lower_characters: bool, entries: &[Entry]) -> Vec<Found> {
    let fdx = FdxFile::parse(sample()).unwrap();
    let script = fdx.to_script();
    let texts: Vec<String> = script
        .paragraphs
        .iter()
        .map(|p| if lower_characters && p.kind.name() == "Character" { p.text().to_lowercase() } else { p.text() })
        .collect();
    let paras: Vec<Para> = script
        .paragraphs
        .iter()
        .zip(&texts)
        .map(|(p, t)| Para { id: p.id, text: t, editable: p.kind.is_editable(), caps: fdx.is_caps(p.kind.name()) })
        .collect();
    match_entries(&paras, entries).into_iter().map(|m| m.map(|m| (m.index, m.fuzzy, m.options, m.option_src))).collect()
}

fn indexes(ms: &[Found]) -> Vec<Option<usize>> {
    ms.iter().map(|m| m.as_ref().map(|m| m.0)).collect()
}

#[test]
fn exact_match_keeps_edge_whitespace() {
    let ms = run_match(
        false,
        &[
            e("Laura smiles.", &["Laura smiles warmly."]),
            e("She say \"hello\" to everybody & leaves.", &["She says \"hello\" to everybody & leaves."]),
        ],
    );
    assert_eq!(indexes(&ms), [Some(6), Some(9)]);
    assert_eq!(ms[0].as_ref().unwrap().2, ["Laura smiles warmly. "]);
    assert!(!ms[1].as_ref().unwrap().1);
}

#[test]
fn duplicates_match_in_script_order_after_the_previous_match() {
    assert_eq!(
        indexes(&run_match(false, &[e("Thank you.", &["Thanks."]), e("Thank you.", &["Thank you so much."])])),
        [Some(5), Some(8)]
    );
    assert_eq!(
        indexes(&run_match(false, &[e("Laura smiles.", &["Laura grins."]), e("Thank you.", &["Thanks!"])])),
        [Some(6), Some(8)]
    );
    assert_eq!(
        indexes(&run_match(false, &[e("(smiling)", &["(grinning)"]), e("(smiling)", &["(laughing)"])])),
        [Some(4), None]
    );
}

#[test]
fn fuzzy_and_unmatched() {
    let ms = run_match(
        false,
        &[e("I am go to town tomorrow.", &["I am going to town tomorrow."]), e("Not here at all, no.", &["x"])],
    );
    assert_eq!(indexes(&ms), [Some(3), None]);
    assert!(ms[0].as_ref().unwrap().1, "fuzzy");
}

#[test]
fn case_only_changes_in_caps_elements_are_not_changes() {
    let ms = run_match(true, &[e("cesar", &["CESAR"]), e("laura", &["Laura", "LAURA (O.S.)"])]);
    assert!(ms[0].as_ref().unwrap().2.is_empty());
    assert_eq!(ms[1].as_ref().unwrap().2, ["LAURA (O.S.)"]);
    assert_eq!(ms[1].as_ref().unwrap().3, [1]);
    let ms = run_match(false, &[e("Laura smiles.", &["laura smiles."])]);
    assert_eq!(ms[0].as_ref().unwrap().2, ["laura smiles. "], "case matters in Action");
}

fn open(text: &str, name: &str) -> (FdxFile, Session, Document) {
    let fdx = FdxFile::parse(sample()).unwrap();
    let script = fdx.to_script();
    let review = Review::open("r", text, &script, |k| fdx.is_caps(k.name()), name);
    let session = Session::new(vec![review]);
    let doc = session.open_document(script);
    (fdx, session, doc)
}

fn sel() -> Selection {
    Selection::cursor(scriptorium_core::Pos::new(ParaId(0), 0))
}

#[test]
fn open_builds_items_in_script_order() {
    let (_, s, _) = open(REVIEW, "sample.fdx");
    let r = &s.reviews[0];
    assert_eq!(r.items.iter().map(|i| i.index).collect::<Vec<_>>(), [1, 3, 9]);
    assert_eq!(r.items[0].why, "article; \"card game\"");
    assert_eq!(r.unmatched.len(), 1);
    assert!(r.warnings.is_empty());
    assert_eq!(r.restored(), 0);
    let (_, s, _) = open(REVIEW, "other.fdx");
    assert_eq!(s.reviews[0].warnings.len(), 1);
}

#[test]
fn decisions_change_text_and_undo_together() {
    let (fdx, s, mut doc) = open(REVIEW, "sample.fdx");
    s.decide(&mut doc, sel(), 0, 0, Decision::Accept(0)).unwrap();
    s.decide(&mut doc, sel(), 0, 1, Decision::KeepMine).unwrap();
    s.decide(&mut doc, sel(), 0, 2, Decision::Rewrite("She says hello and leaves.".into())).unwrap();
    assert_eq!(s.status(&doc, 0, 0), Status::Accepted(0));
    assert_eq!(s.status(&doc, 0, 1), Status::Rejected);
    assert_eq!(s.status(&doc, 0, 2), Status::Rewritten);
    let p = s.progress(&doc, 0);
    assert_eq!((p.accepted, p.rejected, p.rewritten, p.pending), (1, 1, 1, 0));

    let out = FdxFile::parse(fdx.write_script(doc.script()).unwrap()).unwrap();
    let texts: Vec<String> = out.paragraphs().iter().map(|p| p.text()).collect();
    assert_eq!(texts[1], "They are playing truco (an Argentinian card game).");
    assert_eq!(texts[3], "I am go to the town tomorrow.");
    assert_eq!(texts[9], "She says hello and leaves.");
    assert!(out.raw().contains("<Text Style=\"Italic\">truco</Text>"), "italic run survives");

    doc.undo();
    assert_eq!(s.status(&doc, 0, 2), Status::Pending);
    assert_eq!(doc.paragraphs()[9].text(), "She say “hello” to everybody & leaves.");
    assert_eq!(s.next_pending(&doc, 0, None), Some(2));
    assert_eq!(s.next_pending(&doc, 0, Some(2)), Some(2), "wraps");
}

#[test]
fn hand_edits_make_suggestions_outdated_and_accepted_lines_rewritten() {
    let (_, s, mut doc) = open(REVIEW, "sample.fdx");
    doc.replace_text(sel(), ParaId(3), "I go to the town tomorrow.", &[]).unwrap();
    assert!(s.is_outdated(&doc, 0, 1));
    // Keeping mine on an outdated line keeps the current text.
    s.decide(&mut doc, sel(), 0, 1, Decision::KeepMine).unwrap();
    assert_eq!(doc.paragraphs()[3].text(), "I go to the town tomorrow.");
    // Accept, then edit by hand: it's a rewrite now.
    s.decide(&mut doc, sel(), 0, 0, Decision::Accept(0)).unwrap();
    doc.replace_text(sel(), ParaId(1), "They play truco.", &[]).unwrap();
    assert_eq!(s.status(&doc, 0, 0), Status::Rewritten);
    // Undecided puts the original back.
    s.decide(&mut doc, sel(), 0, 0, Decision::Undecided).unwrap();
    assert_eq!(s.status(&doc, 0, 0), Status::Pending);
    assert_eq!(doc.paragraphs()[1].text(), "They are playing truco (Argentinian cards game).");
}

#[test]
fn decide_from_text_recognises_options_and_originals() {
    let (_, s, mut doc) = open(REVIEW, "sample.fdx");
    doc.replace_text(sel(), ParaId(3), "I'm going to town tomorrow.", &[]).unwrap();
    s.decide_from_text(&mut doc, sel(), 0, 1).unwrap();
    assert_eq!(s.status(&doc, 0, 1), Status::Accepted(1));
    doc.replace_text(sel(), ParaId(3), "I am go to the town tomorrow.", &[]).unwrap();
    s.decide_from_text(&mut doc, sel(), 0, 1).unwrap();
    assert_eq!(s.status(&doc, 0, 1), Status::Rejected);
}

#[test]
fn save_progress_round_trips_through_open() {
    let (_, s, mut doc) = open(REVIEW, "sample.fdx");
    s.decide(&mut doc, sel(), 0, 1, Decision::Accept(1)).unwrap();
    s.decide(&mut doc, sel(), 0, 2, Decision::Rewrite("She leaves.".into())).unwrap();
    let saved = s.save_progress(&doc, 0);
    let f = ReviewFile::parse(&saved);
    let statuses: Vec<_> = f.entries.iter().map(|e| (e.status, e.chosen, e.mine.clone())).collect();
    assert_eq!(
        statuses,
        [
            (FileStatus::Pending, None, None),
            (FileStatus::Accepted, Some(1), None),
            (FileStatus::Rewritten, None, Some("She leaves.".into())),
            (FileStatus::Pending, None, None), // unmatched, kept at the end
        ]
    );
    assert_eq!(f.entries[3].before, "Not in this script.");

    let (_, s2, doc2) = open(&saved, "sample.fdx");
    assert_eq!(s2.reviews[0].restored(), 2);
    assert_eq!(s2.status(&doc2, 0, 1), Status::Accepted(1));
    assert_eq!(s2.status(&doc2, 0, 2), Status::Rewritten);
    assert_eq!(doc2.paragraphs()[3].text(), "I'm going to town tomorrow.");
    assert!(!doc2.can_undo(), "restored decisions are the starting point");
}

#[test]
fn two_reviews_on_one_line() {
    let fdx = FdxFile::parse(sample()).unwrap();
    let script = fdx.to_script();
    let a =
        Review::open("a", "---\nBEFORE: Laura smiles.\nAFTER: Laura grins.\n", &script, |k| fdx.is_caps(k.name()), "");
    let b =
        Review::open("b", "---\nBEFORE: Laura smiles.\nAFTER: Laura beams.\n", &script, |k| fdx.is_caps(k.name()), "");
    let s = Session::new(vec![a, b]);
    let mut doc = s.open_document(script);
    s.decide(&mut doc, sel(), 0, 0, Decision::Accept(0)).unwrap();
    assert!(s.is_outdated(&doc, 1, 0));
    s.decide(&mut doc, sel(), 1, 0, Decision::Accept(0)).unwrap();
    assert_eq!(doc.paragraphs()[6].text(), "Laura beams. ");
    assert_eq!(s.status(&doc, 0, 0), Status::Rewritten, "the first review's change was replaced");
    doc.undo();
    assert_eq!(s.status(&doc, 0, 0), Status::Accepted(0));
}

#[test]
fn session_file_resumes_decisions_and_survives_review_changes() {
    use scriptorium_review::sessionfile::{Resumed, SessionFile, resume};
    let raw = sample();
    let fdx = FdxFile::parse(raw.clone()).unwrap();
    let open_with = |texts: &[&str]| {
        let script = fdx.to_script();
        Session::new(texts.iter().map(|t| Review::open("r", t, &script, |k| fdx.is_caps(k.name()), "")).collect())
    };
    let extra = "---\nBEFORE: Laura smiles.\nAFTER: Laura grins.\n";
    let s = open_with(&[REVIEW]);
    let (mut doc, how) = resume(&s, fdx.to_script(), &raw, None);
    assert_eq!(how, Resumed::Fresh);
    s.decide(&mut doc, sel(), 0, 0, Decision::Accept(0)).unwrap();
    let saved = SessionFile::new(&raw, doc.log()).to_json();

    // Reopened with another review file in front: the decision is still found.
    let s2 = open_with(&[extra, REVIEW]);
    let (doc2, how) = resume(&s2, fdx.to_script(), &raw, Some(&saved));
    assert_eq!(how, Resumed::Resumed { events: 1 });
    assert_eq!(s2.status(&doc2, 1, 0), Status::Accepted(0));
    assert_eq!(s2.status(&doc2, 0, 0), Status::Pending);

    // A changed script: the session is not used.
    let (_, how) = resume(&s2, fdx.to_script(), &raw.replace("Laura", "Lara"), Some(&saved));
    assert!(matches!(how, Resumed::Discarded(_)));
}
