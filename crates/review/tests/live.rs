//! Live matching: `PARA:` against the document as edited, `BEFORE` against
//! the original, and resuming a saved session (replay, then match, then the
//! saved decisions of reviews the session didn't know).

use scriptorium_core::{Document, ParaId, Pos, Selection};
use scriptorium_fdx::FdxFile;
use scriptorium_review::sessionfile::{self, ReviewSource};
use scriptorium_review::{Decision, Entry, Review, Session, Status};

const FDX: &str = include_str!("fixtures/v2/sample.fdx");
// ¶0 EXT. FARM HOUSE - NIGHT · ¶1 Laura smiles. · ¶2 CESAR · ¶3 I am go to the town tomorrow.
// ¶4 They are playing truco (…). · ¶5 LAURA · ¶6 Thank you very much. · ¶7 She say hello to everybody and leaves.

fn fdx() -> FdxFile {
    FdxFile::parse(FDX.to_string()).unwrap()
}

fn sel() -> Selection {
    Selection::cursor(Pos::new(ParaId(0), 0))
}

fn edit(doc: &mut Document, para: u64, text: &str) {
    doc.replace_text(sel(), ParaId(para), text, &[]).unwrap();
}

fn open_live(text: &str, doc: &Document) -> Review {
    let f = fdx();
    Review::open_live("a.review.txt", text, &f.to_script(), doc, |k| f.is_caps(k.name()), "sample.fdx", None)
        .with_path("a.review.txt")
}

fn source(text: &str) -> ReviewSource {
    ReviewSource { name: "a.review.txt".into(), path: "a.review.txt".into(), text: text.into() }
}

fn resume(text: &str, saved: Option<&str>) -> sessionfile::ResumedSession {
    let f = fdx();
    sessionfile::resume(
        vec![source(text)],
        f.to_script(),
        Some(&f.legacy_script()),
        f.raw(),
        saved,
        &|k| f.is_caps(k.name()),
        "sample.fdx",
        None,
    )
}

/// A session with no reviews attached.
fn empty() -> sessionfile::ResumedSession {
    let f = fdx();
    sessionfile::resume(
        vec![],
        f.to_script(),
        Some(&f.legacy_script()),
        f.raw(),
        None,
        &|k| f.is_caps(k.name()),
        "sample.fdx",
        None,
    )
}

fn saved(o: &sessionfile::ResumedSession) -> String {
    o.to_file(fdx().raw()).to_json()
}

fn entry(before: &str, para: Option<u64>, options: &[&str]) -> Entry {
    Entry { para: para.map(ParaId).into_iter().collect(), ..Entry::single(before, options) }
}

/// (paragraph, item.before) per item, in item order.
fn placed(r: &Review) -> Vec<(u64, &str)> {
    r.items.iter().map(|it| (it.para.0, it.before.as_str())).collect()
}

fn statuses(o: &sessionfile::ResumedSession, r: usize) -> Vec<Status> {
    (0..o.session.reviews[r].items.len()).map(|i| o.session.status(&o.doc, r, i)).collect()
}

fn decide(o: &mut sessionfile::ResumedSession, i: usize, d: Decision) {
    let s = Selection::cursor(o.doc.start());
    o.session.decide(&mut o.doc, s, 0, i, d).unwrap();
}

fn split_7(doc: &mut Document) -> ParaId {
    // "She say hello" | " to everybody and leaves."
    doc.enter(Selection::cursor(Pos::new(ParaId(7), 13))).unwrap();
    let new = doc.paragraphs()[8].id;
    assert_eq!(doc.paragraph(new).unwrap().text(), " to everybody and leaves.");
    new
}

#[test]
fn para_honoured_when_before_matches() {
    let mut doc = Document::new(fdx().to_script());
    edit(&mut doc, 1, "Laura grins.");
    edit(&mut doc, 7, "Thank you very much.");
    let text = "---\nBEFORE: Laura smiles.\nPARA: 1\nAFTER: Laura beams.\n\
                ---\nBEFORE: Thank you very much.\nPARA: 7\nAFTER: Thanks.\n\
                ---\nBEFORE: Thank you very much.\nAFTER: Many thanks.\n";
    let r = open_live(text, &doc);
    // ¶1 by its original text, ¶7 by its live text; the PARA-less entry by BEFORE on the original.
    assert_eq!(placed(&r), [(1, "Laura smiles."), (6, "Thank you very much."), (7, "Thank you very much.")]);
    assert_eq!(r.items.iter().map(|it| it.entry).collect::<Vec<_>>(), [0, 2, 1]);
    assert!(r.unmatched.is_empty());
}

#[test]
fn para_mismatch_falls_back_to_before() {
    let doc = Document::new(fdx().to_script());
    let text = "---\nBEFORE: Thank you very much.\nPARA: 3\nAFTER: Thanks.\n\
                ---\nBEFORE: Laura smiles.\nPARA: 999\nAFTER: Laura grins.\n\
                ---\nBEFORE: CESAR\nPARA: 2\nAFTER: Cesar Romero\n";
    let r = open_live(text, &doc);
    assert_eq!(placed(&r), [(1, "Laura smiles."), (2, "CESAR"), (6, "Thank you very much.")]);
}

#[test]
fn para_on_paragraph_created_by_split() {
    let mut o = empty();
    let new = split_7(&mut o.doc);
    let json = saved(&o);
    let text = format!("Id: s\n---\nBEFORE: to everybody and leaves.\nPARA: {}\nAFTER: to all and leaves.\n", new.0);
    let back = resume(&text, Some(&json));
    let r = &back.session.reviews[0];
    assert!(r.unmatched.is_empty(), "{:?}", r.unmatched);
    assert_eq!(placed(r), [(new.0, " to everybody and leaves.")]);
    assert_eq!(r.items[0].index, 8);
    assert_eq!(r.items[0].options, [" to all and leaves."]);
    assert!(!back.session.is_outdated(&back.doc, 0, 0));
}

#[test]
fn decided_para_entry_still_matches_after_reopen() {
    let mut o = empty();
    let new = split_7(&mut o.doc);
    let text = format!("Id: s\n---\nBEFORE: to everybody and leaves.\nPARA: {}\nAFTER: to all and leaves.\n", new.0);
    // Attached after the split (as an agent's file would be).
    let r = o.session.add(open_live(&text, &o.doc)).unwrap();
    assert_eq!(r, 0);
    decide(&mut o, 0, Decision::Accept(0));
    assert_eq!(o.doc.paragraph(new).unwrap().text(), " to all and leaves.");
    let back = resume(&text, Some(&saved(&o)));
    assert!(back.notes.is_empty(), "{:?}", back.notes);
    assert_eq!(statuses(&back, 0), [Status::Accepted(0)]);
    // Never the post-decision text.
    assert_eq!(placed(&back.session.reviews[0]), [(new.0, " to everybody and leaves.")]);
}

#[test]
fn undecide_restores_before_text() {
    // A suggestion on a hand-edited line (BEFORE = live text), decided, then reopened.
    let mut o = resume("Id: s\n", None);
    edit(&mut o.doc, 1, "Laura grins.");
    let doc = o.doc.clone();
    let f = fdx();
    let a = o.session.append_entry(&doc, 0, entry("Laura grins.", Some(1), &["Laura beams."]), |k| f.is_caps(k.name()));
    let a = a.unwrap();
    decide(&mut o, a.item.unwrap(), Decision::Accept(0));
    let mut back = resume(&a.file_text, Some(&saved(&o)));
    assert_eq!(statuses(&back, 0), [Status::Accepted(0)]);
    decide(&mut back, 0, Decision::Undecided);
    assert_eq!(back.doc.paragraph(ParaId(1)).unwrap().text(), "Laura grins.");
    assert_eq!(statuses(&back, 0), [Status::Pending]);

    // An entry decided on its original text goes back to the original.
    let mut o = resume("Id: t\n---\nBEFORE: Laura smiles.\nPARA: 1\nAFTER: Laura beams.\n", None);
    decide(&mut o, 0, Decision::Accept(0));
    let json = saved(&o);
    let mut back = resume("Id: t\n---\nBEFORE: Laura smiles.\nPARA: 1\nAFTER: Laura beams.\n", Some(&json));
    decide(&mut back, 0, Decision::Undecided);
    assert_eq!(back.doc.paragraph(ParaId(1)).unwrap().text(), "Laura smiles.");
}

#[test]
fn suggestion_on_live_text_matches_after_restart() {
    let mut o = resume("Id: s\n", None);
    edit(&mut o.doc, 6, "Thanks, really.");
    let json = saved(&o);
    // A tool writes the live text with PARA; without PARA the original text wouldn't match.
    let text = "Id: s\n---\nBEFORE: Thanks, really.\nPARA: 6\nAFTER: Thanks, truly.\n\
                ---\nBEFORE: Thanks, really.\nAFTER: Cheers.\n";
    let back = resume(text, Some(&json));
    let r = &back.session.reviews[0];
    assert_eq!(placed(r), [(6, "Thanks, really.")]);
    assert_eq!(r.unmatched.len(), 1);
    assert_eq!(r.unmatched[0].entry, 1);
    assert!(!back.session.is_outdated(&back.doc, 0, 0));
}

#[test]
fn para_on_deleted_paragraph_stays_on_it_as_removed() {
    let mut doc = Document::new(fdx().to_script());
    let end6 = Pos::new(ParaId(6), doc.paragraph(ParaId(6)).unwrap().text().len());
    let end7 = Pos::new(ParaId(7), doc.paragraph(ParaId(7)).unwrap().text().len());
    doc.delete_backward(Selection { anchor: end6, head: end7 }, false).unwrap();
    assert!(doc.paragraph(ParaId(7)).is_none());
    let text = "---\nBEFORE: She say hello to everybody and leaves.\nPARA: 7\nAFTER: She says hello.\n\
                ---\nBEFORE: She say hello to everybody and leaves.\nAFTER: She says hello.\n";
    // A7: the entry stays on its (gone) line, never on another line with its
    // text; the second entry's only line is taken, so it isn't found.
    let r = open_live(text, &doc);
    assert_eq!(placed(&r), [(7, "She say hello to everybody and leaves.")]);
    assert_eq!(r.unmatched.iter().map(|u| u.entry).collect::<Vec<_>>(), [1]);
    let s = Session::new(vec![r]);
    assert_eq!(s.status(&doc, 0, 0), Status::Removed);
}

#[test]
fn append_entry_uses_para_for_repeated_lines() {
    let mut o = resume("Id: s\n", None);
    edit(&mut o.doc, 1, "Yes.");
    edit(&mut o.doc, 6, "Yes.");
    let doc = o.doc.clone();
    let f = fdx();
    let caps = |k: &scriptorium_core::ElementKind| f.is_caps(k.name());
    let a = o.session.append_entry(&doc, 0, entry("Yes.", Some(6), &["Yes!"]), caps).unwrap();
    assert_eq!(o.session.item(0, a.item.unwrap()).para, ParaId(6));
    let b = o.session.append_entry(&doc, 0, entry("Yes.", Some(1), &["Yes?"]), caps).unwrap();
    assert_eq!(o.session.item(0, b.item.unwrap()).para, ParaId(1));
    // Both lines taken: a third "Yes." has nowhere to go.
    let c = o.session.append_entry(&doc, 0, entry("Yes.", None, &["Yep."]), caps).unwrap();
    assert_eq!(c.item, None);

    // And they come back on the same lines after a restart.
    let back = resume(&c.file_text, Some(&saved(&o)));
    assert_eq!(placed(&back.session.reviews[0]), [(1, "Yes."), (6, "Yes.")]);
}

#[test]
fn known_review_status_disagreement_session_wins_with_note() {
    let text =
        "Id: k\n---\nBEFORE: Laura smiles.\nAFTER: Laura grins.\n---\nBEFORE: Thank you very much.\nAFTER: Thanks.\n";
    let mut o = resume(text, None);
    decide(&mut o, 0, Decision::Accept(0));
    let progress = o.session.save_progress(&o.doc, 0);
    // The session moves on: ¶1 kept as it was, ¶6 accepted.
    decide(&mut o, 0, Decision::KeepMine);
    decide(&mut o, 1, Decision::Accept(0));
    let back = resume(&progress, Some(&saved(&o)));
    assert_eq!(back.session.reviews[0].id, "k");
    assert_eq!(statuses(&back, 0), [Status::Rejected, Status::Accepted(0)]);
    assert_eq!(back.notes.len(), 1, "{:?}", back.notes);
    assert!(back.notes[0].contains("1 decision saved in the file differs"), "{:?}", back.notes);
    assert_eq!(back.doc.undo_depth(), o.doc.undo_depth(), "nothing applied from the file");
}

#[test]
fn hand_deleted_block_orphans_decisions() {
    let text = "Id: k\n---\nBEFORE: Laura smiles.\nAFTER: Laura grins.\n\
                ---\nBEFORE: I am go to the town tomorrow.\nAFTER: I'm going to town tomorrow.\n\
                ---\nBEFORE: Thank you very much.\nAFTER: Thanks.\n";
    let mut o = resume(text, None);
    decide(&mut o, 1, Decision::KeepMine); // entry 1, ¶3
    assert_eq!(o.doc.meta("review/k/1"), Some("rejected@3"));
    // Block 0 deleted by hand: ¶6's entry is now entry 1, where ¶3's decision is stored.
    let edited = text.replace("---\nBEFORE: Laura smiles.\nAFTER: Laura grins.\n", "");
    let back = resume(&edited, Some(&saved(&o)));
    assert_eq!(placed(&back.session.reviews[0]).iter().map(|p| p.0).collect::<Vec<_>>(), [3, 6]);
    assert_eq!(statuses(&back, 0), [Status::Pending, Status::Pending]);
}

#[test]
fn restored_counts_only_decisions_that_apply() {
    // CHOSEN 2 only changes letter case in a capitals element: not an option, so not restored.
    let text = "---\nBEFORE: CESAR\nAFTER: Cesar Romero\nAFTER: Cesar\nSTATUS: accepted\nCHOSEN: 2\n\
                ---\nBEFORE: Laura smiles.\nAFTER: Laura grins.\nSTATUS: rejected\n\
                ---\nBEFORE: Nowhere in the script.\nAFTER: x\nSTATUS: rejected\n";
    let f = fdx();
    let r = Review::open("a", text, &f.to_script(), |k| f.is_caps(k.name()), "");
    assert_eq!(r.restored(), 1);
    let s = Session::new(vec![r]);
    let (doc, _) = s.open_document_with_base(f.to_script());
    assert_eq!(s.status(&doc, 0, 0), Status::Rejected);
    assert_eq!(s.status(&doc, 0, 1), Status::Pending);
}

#[test]
fn append_entries_matches_the_batch_and_writes_once() {
    let mut o = resume("Id: s\n---\nBEFORE: Laura smiles.\nAFTER: Laura grins.\n", None);
    edit(&mut o.doc, 6, "Laura smiles.");
    let doc = o.doc.clone();
    let f = fdx();
    let before = o.session.reviews[0].text().to_string();
    let batch = vec![
        // ¶1 is the file's already; ¶6 goes to the PARA entry below.
        entry("Laura smiles.", None, &["Laura beams."]),
        entry("Laura smiles.", Some(6), &["Laura laughs."]),
        // ¶6 claimed earlier in the batch.
        entry("Laura smiles.", Some(6), &["Laura giggles."]),
        entry("Nowhere in the script.", None, &["x"]),
        // Invisible: written, but no item.
        entry("CESAR", Some(2), &["Cesar"]),
        entry("I am go to the town tomorrow.", None, &["I'm going to town tomorrow."]),
    ];
    let out = o.session.append_entries(&doc, 0, batch, false, |k| f.is_caps(k.name()), None).unwrap();
    let got: Vec<_> = out.results.iter().map(|r| (r.item, r.entry)).collect();
    assert_eq!(
        got,
        [(None, None), (Some(1), Some(1)), (None, None), (None, None), (None, Some(2)), (Some(2), Some(3))]
    );
    let r = &o.session.reviews[0];
    assert_eq!(r.items.iter().map(|it| it.para.0).collect::<Vec<_>>(), [1, 6, 3]);
    assert_eq!(r.invisible, [2]);
    // One splice of the written entries onto the file as it was; nothing unmatched written.
    assert!(out.file_text.starts_with(&before));
    assert_eq!(out.file_text.matches("BEFORE:").count(), 4);
    assert!(
        !out.file_text.contains("Nowhere") && !out.file_text.contains("giggles") && !out.file_text.contains("beams")
    );
    assert_eq!(r.text(), out.file_text);
    assert_eq!(r.file.entries.len(), 4);

    // Reopened: the same items on the same lines.
    let back = resume(&out.file_text, Some(&saved(&o)));
    let mut paras: Vec<u64> = back.session.reviews[0].items.iter().map(|it| it.para.0).collect();
    paras.sort();
    assert_eq!(paras, [1, 3, 6]);
}

#[test]
fn append_entries_para_only_never_lands_on_an_identical_line() {
    let mut o = resume("Id: s\n", None);
    edit(&mut o.doc, 1, "Yes.");
    edit(&mut o.doc, 6, "Yes.");
    let doc = o.doc.clone();
    let f = fdx();
    let batch = vec![
        entry("Yes.", Some(6), &["Yes!"]),
        entry("Yes.", Some(6), &["Yes?"]),                  // ¶6 taken: not moved to ¶1
        entry("Yes.", None, &["Yep."]),                     // no PARA
        entry("Yes.", Some(99), &["Aye."]),                 // no such paragraph
        entry("Laura smiles.", Some(6), &["Laura grins."]), // BEFORE doesn't fit ¶6
    ];
    let out = o.session.append_entries(&doc, 0, batch, true, |k| f.is_caps(k.name()), None).unwrap();
    let got: Vec<_> = out.results.iter().map(|r| (r.item, r.entry)).collect();
    assert_eq!(got, [(Some(0), Some(0)), (None, None), (None, None), (None, None), (None, None)]);
    assert_eq!(o.session.item(0, 0).para, ParaId(6));
    assert_eq!(out.file_text.matches("BEFORE:").count(), 1);
}

#[test]
fn v1_session_that_undecided_a_restored_entry_resumes() {
    // A file decision restored into the starting point, then un-decided.
    let text = "---\nBEFORE: Laura smiles.\nAFTER: Laura grins.\nSTATUS: accepted\nCHOSEN: 1\n";
    let mut o = resume(text, None);
    assert_eq!(statuses(&o, 0), [Status::Accepted(0)]);
    decide(&mut o, 0, Decision::Undecided);
    // As version 1 wrote it: no reviews, no base, values without `@<para>`.
    let mut v: serde_json::Value = serde_json::from_str(&saved(&o)).unwrap();
    v["version"] = 1.into();
    v.as_object_mut().unwrap().remove("base");
    v.as_object_mut().unwrap().remove("reviews");
    let json = v.to_string().replace("accepted:0@1", "accepted:0");
    assert!(json.contains(r#""from":"accepted:0""#), "{json}");

    let back = resume(text, Some(&json));
    assert_eq!(back.how, sessionfile::Resumed::Resumed { events: 1 });
    assert_eq!(statuses(&back, 0), [Status::Pending]);
    assert_eq!(back.doc.paragraph(ParaId(1)).unwrap().text(), "Laura smiles.");
    // Saved again (now as version 2) it still resumes.
    let again = resume(text, Some(&saved(&back)));
    assert_eq!(again.how, sessionfile::Resumed::Resumed { events: 1 });
    assert_eq!(statuses(&again, 0), [Status::Pending]);
}

#[test]
fn append_without_para_on_edited_line_writes_the_matched_para() {
    let mut o = resume("Id: s\n", None);
    edit(&mut o.doc, 1, "Laura grins.");
    let doc = o.doc.clone();
    let f = fdx();
    let a = o.session.append_entry(&doc, 0, entry("Laura grins.", None, &["Laura beams."]), |k| f.is_caps(k.name()));
    let a = a.unwrap();
    assert!(a.file_text.contains("PARA: 1\n"), "{}", a.file_text);
    decide(&mut o, a.item.unwrap(), Decision::Accept(0));
    let back = resume(&a.file_text, Some(&saved(&o)));
    assert!(back.session.reviews[0].unmatched.is_empty());
    assert_eq!(statuses(&back, 0), [Status::Accepted(0)]);
}

#[test]
fn para_fallback_is_written_with_the_paragraph_matched() {
    let mut o = resume("Id: s\n", None);
    let doc = o.doc.clone();
    let f = fdx();
    // PARA 3 doesn't fit; BEFORE finds ¶6.
    let out = o
        .session
        .append_entries(
            &doc,
            0,
            vec![entry("Thank you very much.", Some(3), &["Thanks."])],
            false,
            |k| f.is_caps(k.name()),
            None,
        )
        .unwrap();
    assert_eq!(o.session.item(0, out.results[0].item.unwrap()).para, ParaId(6));
    assert!(out.file_text.contains("PARA: 6\n") && !out.file_text.contains("PARA: 3"), "{}", out.file_text);
    let back = resume(&out.file_text, Some(&saved(&o)));
    assert_eq!(placed(&back.session.reviews[0]), [(6, "Thank you very much.")]);
}

#[test]
fn append_entries_says_why_an_entry_got_no_item() {
    use scriptorium_review::Unplaced;
    let mut o = resume("Id: s\n---\nBEFORE: Laura smiles.\nAFTER: Laura grins.\n", None);
    let doc = o.doc.clone();
    let f = fdx();
    let caps = |k: &scriptorium_core::ElementKind| f.is_caps(k.name());
    let batch = vec![
        entry("Laura smiles.", Some(1), &["Laura beams."]),   // ¶1 is item 0's
        entry("Laura smiles.", None, &["Laura beams."]),      // no PARA
        entry("Laura smiles.", Some(99), &["Laura beams."]),  // no ¶99
        entry("Laura smiles.", Some(6), &["Laura beams."]),   // BEFORE doesn't fit ¶6
        entry("CESAR", Some(2), &["Cesar"]),                  // invisible: written
        entry("CESAR", Some(2), &["Cesar Romero"]),           // ¶2 claimed by the invisible one
        entry("Thank you very much.", Some(6), &["Thanks."]), // placed
        entry("Thank you very much.", Some(6), &["Many thanks."]), // ¶6 claimed by the one above
    ];
    let out = o.session.append_entries(&doc, 0, batch, true, caps, None).unwrap();
    let why: Vec<_> = out.results.iter().map(|r| r.unplaced.clone()).collect();
    assert_eq!(
        why,
        [
            Some(Unplaced::Taken { entry: 0, item: Some(0) }),
            Some(Unplaced::NoPara),
            Some(Unplaced::Missing),
            Some(Unplaced::Mismatch),
            Some(Unplaced::Invisible),
            Some(Unplaced::Taken { entry: 1, item: None }),
            None,
            Some(Unplaced::Taken { entry: 2, item: Some(1) }),
        ]
    );
    assert_eq!(out.results[4].entry, Some(1), "invisible entries are written");
    assert_eq!(out.results[5].entry, None);

    // Without para_only: a BEFORE that fits only a taken line says so; nothing fitting is Missing.
    let more = vec![entry("Laura smiles.", None, &["Laura beams."]), entry("Nowhere at all.", None, &["x"])];
    let out = o.session.append_entries(&doc, 0, more, false, caps, None).unwrap();
    let why: Vec<_> = out.results.iter().map(|r| r.unplaced.clone()).collect();
    assert_eq!(why, [Some(Unplaced::Taken { entry: 0, item: Some(0) }), Some(Unplaced::Missing)]);

    // A BEFORE that only fits a taken line approximately is Taken too, not Missing.
    let fuzzy = vec![entry("Thank you very much!", None, &["Thanks!"])];
    let out = o.session.append_entries(&doc, 0, fuzzy, false, caps, None).unwrap();
    assert_eq!(out.results[0].unplaced, Some(Unplaced::Taken { entry: 2, item: Some(1) }));
    assert_eq!(out.results[0].entry, None);
}

#[test]
fn file_disagreement_noted_only_when_the_file_changed() {
    let text = "Id: k\n---\nBEFORE: Laura smiles.\nAFTER: Laura grins.\nSTATUS: accepted\nCHOSEN: 1\n";
    let mut o = resume(text, None);
    decide(&mut o, 0, Decision::KeepMine);
    let json = saved(&o);
    // Unchanged file: its old STATUS line isn't news.
    let back = resume(text, Some(&json));
    assert!(back.notes.is_empty(), "{:?}", back.notes);
    assert_eq!(statuses(&back, 0), [Status::Rejected]);
    // Changed since (a comment added): compared, and noted.
    let back = resume(&format!("{text}# hand edit\n"), Some(&json));
    assert_eq!(back.notes.len(), 1, "{:?}", back.notes);
    // A session saved before the hash was recorded: always compared.
    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    v["reviews"][0].as_object_mut().unwrap().remove("text_hash");
    let back = resume(text, Some(&v.to_string()));
    assert_eq!(back.notes.len(), 1, "{:?}", back.notes);
}

#[test]
fn progress_written_over_the_file_is_recorded() {
    let text = "Id: k\n---\nBEFORE: Laura smiles.\nAFTER: Laura grins.\n";
    let mut o = resume(text, None);
    decide(&mut o, 0, Decision::Accept(0));
    let progress = o.session.save_progress(&o.doc, 0);
    o.session.reviews[0].wrote_progress(progress.clone()).unwrap();
    assert_eq!(o.session.reviews[0].text(), progress);
    // The session moves on after writing progress; the file is as last written.
    decide(&mut o, 0, Decision::KeepMine);
    let back = resume(&progress, Some(&saved(&o)));
    assert!(back.notes.is_empty(), "{:?}", back.notes);
    assert_eq!(statuses(&back, 0), [Status::Rejected]);
}

#[test]
fn wrote_progress_refuses_a_different_entry_count() {
    let text = "Id: k\n---\nBEFORE: Laura smiles.\nAFTER: Laura grins.\n";
    let mut o = resume(text, None);
    let more = format!("{text}---\nBEFORE: CESAR\nAFTER: Cesar Romero\n");
    assert!(o.session.reviews[0].wrote_progress(more).is_err());
    assert_eq!(o.session.reviews[0].text(), text, "nothing changed");
    assert!(o.session.reviews[0].wrote_progress("Id: k\n".to_string()).is_err());
    assert_eq!(o.session.reviews[0].file.entries.len(), 1);
}

#[test]
fn fuzzy_before_is_written_as_the_line_matched() {
    let mut o = resume("Id: s\n", None);
    let doc = o.doc.clone();
    let f = fdx();
    // No PARA, and BEFORE only fits ¶6 approximately.
    let out = o
        .session
        .append_entries(
            &doc,
            0,
            vec![entry("Thank you very much!", None, &["Thanks."])],
            false,
            |k| f.is_caps(k.name()),
            None,
        )
        .unwrap();
    let item = o.session.item(0, out.results[0].item.unwrap());
    assert!(item.fuzzy);
    assert_eq!(item.para, ParaId(6));
    assert!(out.file_text.contains("PARA: 6\n"), "{}", out.file_text);
    assert!(out.file_text.contains("BEFORE: Thank you very much.\n"), "{}", out.file_text);
    assert!(!out.file_text.contains("very much!"), "{}", out.file_text);
    // Reopening finds the same paragraph, exactly this time.
    let back = resume(&out.file_text, Some(&saved(&o)));
    let r = &back.session.reviews[0];
    assert_eq!(placed(r), [(6, "Thank you very much.")]);
    assert!(!r.items[0].fuzzy);
    // And on a fresh open with no session.
    let fresh = open_live(&out.file_text, &fdx_doc());
    assert_eq!(placed(&fresh), [(6, "Thank you very much.")]);
}

fn fdx_doc() -> Document {
    Document::new(fdx().to_script())
}

#[test]
fn para_on_a_non_editable_paragraph_is_not_editable() {
    use scriptorium_review::Unplaced;
    let xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Action">
      <Text>Laura smiles.</Text>
    </Paragraph>
    <Paragraph Type="New Act">
      <Text>END OF ACT ONE</Text>
    </Paragraph>
  </Content>
</FinalDraft>
"#;
    let f = FdxFile::parse(xml.to_string()).unwrap();
    let caps = |k: &scriptorium_core::ElementKind| f.is_caps(k.name());
    let mut o = sessionfile::resume(vec![source("Id: n\n")], f.to_script(), None, f.raw(), None, &caps, "x.fdx", None);
    let doc = o.doc.clone();
    assert!(!doc.paragraphs()[1].kind.is_editable());
    for para_only in [true, false] {
        let e = entry("END OF ACT ONE", Some(1), &["END OF ACT 1"]);
        let out = o.session.append_entries(&doc, 0, vec![e], para_only, caps, None).unwrap();
        assert_eq!(out.results[0].unplaced, Some(Unplaced::NotEditable), "para_only {para_only}");
        assert_eq!(out.results[0].entry, None);
    }
}
