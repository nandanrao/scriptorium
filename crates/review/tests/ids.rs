//! Stable review ids (`Id:` header), the `PARA:` field, appending suggestions,
//! and saving progress in file order. The v2 fixture in `fixtures/v2` was made
//! with the `scriptorium` CLI before review ids existed; it must keep replaying.

use scriptorium_core::{Document, ParaId, Selection};
use scriptorium_fdx::FdxFile;
use scriptorium_review::file::{append_block, insert_id};
use scriptorium_review::sessionfile::{self, Resumed};
use scriptorium_review::{Decision, Entry, FileStatus, Line, Review, ReviewFile, Session, Status};

const V2_FDX: &str = include_str!("fixtures/v2/sample.fdx");
const V2_REVIEW: &str = include_str!("fixtures/v2/a.review.txt");
const V2_SESSION: &str = include_str!("fixtures/v2/sample.scriptorium.json");

fn open_review(fdx: &FdxFile, path: &str, text: &str) -> Review {
    Review::open(path, text, &fdx.to_script(), |k| fdx.is_caps(k.name()), "sample.fdx").with_path(path)
}

/// A review already opened (e.g. to read its id), to open again with a session.
fn source(r: &Review) -> sessionfile::ReviewSource {
    sessionfile::ReviewSource { name: r.name.clone(), path: r.path.clone(), text: r.text().to_string() }
}

fn resume(sources: Vec<sessionfile::ReviewSource>, fdx: &FdxFile, saved: Option<&str>) -> sessionfile::ResumedSession {
    sessionfile::resume(
        sources,
        fdx.to_script(),
        Some(&fdx.legacy_script()),
        fdx.raw(),
        saved,
        &|k| fdx.is_caps(k.name()),
        "sample.fdx",
        None,
    )
}

fn statuses(o: &sessionfile::ResumedSession, r: usize) -> Vec<Status> {
    (0..o.session.reviews[r].items.len()).map(|i| o.session.status(&o.doc, r, i)).collect()
}

fn texts(doc: &Document) -> Vec<String> {
    doc.paragraphs().iter().map(|p| p.text()).collect()
}

#[test]
fn v2_session_replays_identically() {
    let fdx = FdxFile::parse(V2_FDX.to_string()).unwrap();
    let review = open_review(&fdx, "a.review.txt", V2_REVIEW);
    assert_eq!(review.id, "e9a0739b58ed");
    let mut o = resume(vec![source(&review)], &fdx, Some(V2_SESSION));
    assert_eq!(o.how, Resumed::Resumed { events: 4 });
    assert!(o.notes.is_empty(), "{:?}", o.notes);
    assert_eq!(statuses(&o, 0), [Status::Accepted(1), Status::Rejected, Status::Rewritten, Status::Pending]);
    let expected = [
        "EXT. FARM HOUSE - NIGHT",
        "Laura grins.",
        "CESAR",
        "I'm going to town tomorrow.",
        "They are playing truco (Argentinian cards game).",
        "LAURA",
        "Thanks, really.",
        "She say hello to everybody and leaves.",
    ];
    assert_eq!(texts(&o.doc), expected);
    assert_eq!(o.doc.undo_depth(), 2);
    assert!(o.doc.can_redo());

    // One undo takes back the hand edit; the next takes back the rewrite.
    o.doc.undo().unwrap();
    assert_eq!(o.doc.paragraph(ParaId(1)).unwrap().text(), "Laura smiles.");
    o.doc.undo().unwrap();
    assert_eq!(statuses(&o, 0), [Status::Accepted(1), Status::Rejected, Status::Pending, Status::Pending]);
    assert_eq!(o.doc.undo_depth(), 0);
}

fn fdx() -> FdxFile {
    FdxFile::parse(V2_FDX.to_string()).unwrap()
}

fn entry(before: &str, options: &[&str]) -> Entry {
    Entry::single(before, options)
}

/// Entries in file order (not script order): truco (¶4), go (¶3), thanks (¶6).
const HEADERLESS: &str = "SCRIPT REVIEW v1
Script: sample.fdx

# from the chat
---
BEFORE: They are playing truco (Argentinian cards game).
AFTER: They are playing truco (an Argentinian
  card game).
---
BEFORE: I am go to the town tomorrow.
AFTER: I am going to town tomorrow.
AFTER: I'm going to town tomorrow.
---
BEFORE: Thank you very much.
AFTER: Thanks a lot.
";

fn open_with(text: &str, saved: Option<&str>) -> sessionfile::ResumedSession {
    let f = fdx();
    resume(vec![source(&open_review(&f, "a.review.txt", text))], &f, saved)
}

fn saved(o: &sessionfile::ResumedSession) -> String {
    o.to_file(fdx().raw()).to_json()
}

fn decide(o: &mut sessionfile::ResumedSession, r: usize, i: usize, d: Decision) {
    let sel = Selection::cursor(o.doc.start());
    o.session.decide(&mut o.doc, sel, r, i, d).unwrap();
}

#[test]
fn id_header_parses_serializes_and_overrides_hash() {
    let text = HEADERLESS.replace("Script: sample.fdx\n", "Script: sample.fdx\nId: thread-42\n");
    let file = ReviewFile::parse(&text);
    assert_eq!(file.id.as_deref(), Some("thread-42"));
    assert!(file.serialize().starts_with("SCRIPT REVIEW v1\nScript: sample.fdx\nId: thread-42\n"));
    assert_eq!(ReviewFile::parse(&file.serialize()), file);
    assert_eq!(open_review(&fdx(), "a", &text).id, "thread-42");
    // The id survives an edit to the file's body.
    assert_eq!(open_review(&fdx(), "a", &(text.clone() + "# note\n")).id, "thread-42");

    // Only before the first block, and only a valid token.
    let late = ReviewFile::parse("---\nBEFORE: a\nAFTER: b\n---\nId: late\n");
    assert_eq!(late.id, None);
    let bad = ReviewFile::parse("SCRIPT REVIEW v1\nId: has spaces\n");
    assert_eq!(bad.id, None);
    assert_eq!(bad.warnings.len(), 1);
}

#[test]
fn header_less_file_keeps_hash_id() {
    let r = open_review(&fdx(), "a", HEADERLESS);
    assert_eq!(r.file.id, None);
    assert_eq!(r.id, sessionfile::hash(HEADERLESS)[..12]);
}

#[test]
fn para_field_round_trips() {
    let file = ReviewFile::parse("---\nBEFORE: a\nPARA: 341\nAFTER: b\n---\nBEFORE: c\nPARA: x\nAFTER: d\n");
    assert_eq!(file.entries[0].para, [ParaId(341)]);
    assert_eq!(file.entries[0].mine, None, "PARA has its own slot");
    assert!(file.entries[1].para.is_empty());
    assert_eq!(file.warnings.len(), 1);
    let text = file.serialize();
    assert!(text.contains("BEFORE: a\nPARA: 341\nAFTER: b\n"), "{text}");

    // Only `PARA:` in capitals: a wrapped Spanish line starting "Para:" is text.
    let es = ReviewFile::parse("---\nBEFORE: Lo dice\nPara: todos.\nAFTER: Lo dice para todos.\n");
    assert_eq!(es.entries[0].before, [Line::untyped("Lo dice Para: todos.")]);
    assert!(es.entries[0].para.is_empty());
    assert_eq!(ReviewFile::parse(&text).entries, file.entries);
}

#[test]
fn append_to_headerless_file_inserts_id_keeps_comments() {
    let e = Entry { para: vec![ParaId(7)], why: "agreement".into(), ..entry("She say hello.", &["She says hello."]) };
    let out = append_block(HEADERLESS, &e, "abc123");
    let expected_head = "SCRIPT REVIEW v1\nScript: sample.fdx\nId: abc123\n\n# from the chat\n";
    assert!(out.starts_with(expected_head), "{out}");
    assert!(out.contains("AFTER: They are playing truco (an Argentinian\n  card game).\n"));
    assert!(out.ends_with("---\nBEFORE: She say hello.\nPARA: 7\nAFTER: She says hello.\nWHY: agreement\n"), "{out}");
    let parsed = ReviewFile::parse(&out);
    assert_eq!(parsed.id.as_deref(), Some("abc123"));
    assert_eq!(parsed.entries.len(), 4);
    assert_eq!(parsed.entries[3], e);
    assert_eq!(parsed.entries[..3], ReviewFile::parse(HEADERLESS).entries[..]);

    // An existing Id is kept; a second append inserts nothing.
    let again = append_block(&out, &entry("x", &["y"]), "other");
    assert_eq!(again.matches("Id:").count(), 1);
    assert!(again.starts_with(&out));

    // No trailing newline, CRLF, fences, no Script: line, no header at all.
    let crlf = "```\r\nSCRIPT REVIEW v1\r\n---\r\nBEFORE: a\r\nAFTER: b\r\n```";
    let out = append_block(crlf, &entry("c", &["d"]), "k");
    assert_eq!(
        out,
        "```\r\nSCRIPT REVIEW v1\r\nId: k\r\n---\r\nBEFORE: a\r\nAFTER: b\r\n```\r\n---\r\nBEFORE: c\r\nAFTER: d\r\n"
    );
    assert_eq!(ReviewFile::parse(&out).entries.len(), 2);
    assert_eq!(insert_id("BEFORE: a\nAFTER: b\n", "k"), "Id: k\nBEFORE: a\nAFTER: b\n");
    assert_eq!(insert_id("SCRIPT REVIEW v1\nScript: s.fdx", "k"), "SCRIPT REVIEW v1\nScript: s.fdx\nId: k\n");
    assert_eq!(append_block("", &entry("a", &["b"]), "k"), "SCRIPT REVIEW v1\nId: k\n\n---\nBEFORE: a\nAFTER: b\n");
}

#[test]
fn attach_splices_id_and_keeps_decisions() {
    let mut o = open_with(HEADERLESS, None);
    let id = o.session.reviews[0].id.clone();
    decide(&mut o, 0, 0, Decision::Accept(1));
    let pinned = o.session.reviews[0].pin_id().expect("header-less file gets an Id");
    assert_eq!(pinned, insert_id(HEADERLESS, &id));
    assert_eq!(o.session.reviews[0].pin_id(), None, "only once");
    assert_eq!(o.session.reviews[0].id, id);
    let json = saved(&o);

    // Hand-edit the pinned file: the decisions still belong to it.
    let edited = pinned.replace("# from the chat", "# from the chat, tidied");
    let back = open_with(&edited, Some(&json));
    assert!(back.notes.is_empty(), "{:?}", back.notes);
    assert_eq!(back.session.reviews[0].id, id);
    assert_eq!(statuses(&back, 0), [Status::Accepted(1), Status::Pending, Status::Pending]);
}

#[test]
fn duplicate_id_refused() {
    let f = fdx();
    let a = HEADERLESS.replace("Script: sample.fdx\n", "Script: sample.fdx\nId: same\n");
    let b = a.replace("Thanks a lot.", "Many thanks.");
    let mut session = Session::default();
    session.add(open_review(&f, "a.review.txt", &a)).unwrap();
    let err = session.add(open_review(&f, "b.review.txt", &b)).unwrap_err();
    assert!(err.contains("a.review.txt") && err.contains("b.review.txt") && err.contains("share `Id: same`"), "{err}");
    assert_eq!(session.reviews.len(), 1);
}

#[test]
fn append_entry_keeps_current_item_indices() {
    let mut o = open_with(HEADERLESS, None);
    let before: Vec<_> = o.session.reviews[0].items.clone();
    // ¶1 comes before every existing item in the script.
    let doc = o.doc.clone();
    let a = o.session.append_entry(&doc, 0, entry("Laura smiles.", &["Laura grins."]), |k| fdx().is_caps(k.name()));
    let a = a.unwrap();
    assert_eq!(a.item, Some(3));
    let review = &o.session.reviews[0];
    assert_eq!(review.items[..3], before[..]);
    assert_eq!((review.items[3].para, review.items[3].entry), (ParaId(1), 3));
    assert_eq!(review.id, sessionfile::hash(HEADERLESS)[..12], "id unchanged");
    assert!(a.file_text.starts_with(&insert_id(HEADERLESS, &review.id)));

    // A paragraph this review already covers isn't matched again; nor is an invisible change.
    let dup = o.session.append_entry(&doc, 0, entry("Thank you very much.", &["Ta."]), |k| fdx().is_caps(k.name()));
    let dup = dup.unwrap();
    assert_eq!(dup.item, None);
    assert_eq!(dup.file_text, a.file_text, "an unmatched entry isn't written");
    let caps = o.session.append_entry(&doc, 0, entry("LAURA", &["Laura"]), |k| fdx().is_caps(k.name()));
    assert_eq!(caps.unwrap().item, None);
    assert!(o.session.reviews[0].unmatched.is_empty());
    assert_eq!(o.session.reviews[0].invisible, [4]);
    assert!(o.session.append_entry(&doc, 0, entry("", &["x"]), |_| false).is_err());
}

#[test]
fn decide_append_reopen_keeps_both_statuses() {
    let mut o = open_with(HEADERLESS, None);
    decide(&mut o, 0, 0, Decision::KeepMine); // ¶3 (entry 1)
    let doc = o.doc.clone();
    let e = entry("She say hello to everybody and leaves.", &["She says hello to everybody and leaves."]);
    let a = o.session.append_entry(&doc, 0, e, |k| fdx().is_caps(k.name())).unwrap();
    decide(&mut o, 0, a.item.unwrap(), Decision::Accept(0));
    let json = saved(&o);

    let back = open_with(&a.file_text, Some(&json));
    assert!(back.notes.is_empty(), "{:?}", back.notes);
    assert_eq!(statuses(&back, 0), [Status::Rejected, Status::Pending, Status::Pending, Status::Accepted(0)]);
    assert_eq!(texts(&back.doc), texts(&o.doc));
}

#[test]
fn save_progress_keeps_file_order() {
    let text = HEADERLESS.replace(
        "---\nBEFORE: I am go",
        "---\nBEFORE: Not in this script.\nAFTER: x\nSTATUS: rejected\n---\nBEFORE: I am go",
    );
    let mut o = open_with(&text, None);
    assert_eq!(o.session.reviews[0].unmatched.len(), 1);
    decide(&mut o, 0, 1, Decision::Rewrite("Truco time.".into())); // ¶4 = entry 0
    let out = o.session.save_progress(&o.doc, 0);
    let file = ReviewFile::parse(&out);
    assert_eq!(file.id, Some(o.session.reviews[0].id.clone()));
    let mines: Vec<Option<String>> = file.entries.iter().map(|e| e.mine.as_deref().map(Line::join)).collect();
    let befores: Vec<String> = file.entries.iter().map(|e| Line::join(&e.before)).collect();
    let got: Vec<_> =
        file.entries.iter().enumerate().map(|(i, e)| (befores[i].as_str(), e.status, mines[i].as_deref())).collect();
    assert_eq!(
        got,
        [
            ("They are playing truco (Argentinian cards game).", FileStatus::Rewritten, Some("Truco time.")),
            ("Not in this script.", FileStatus::Rejected, None),
            ("I am go to the town tomorrow.", FileStatus::Pending, None),
            ("Thank you very much.", FileStatus::Pending, None),
        ]
    );
}

#[test]
fn restore_by_entry_index() {
    // Entry 0 (¶4) is item 1 in script order; entry 1 (¶3) is item 0.
    let text = HEADERLESS
        .replace("card game).\n", "card game).\nSTATUS: rejected\n")
        .replace("tomorrow.\n---", "tomorrow.\nSTATUS: accepted\nCHOSEN: 2\n---");
    let o = open_with(&text, None);
    assert_eq!(o.session.reviews[0].restored(), 2);
    assert_eq!(statuses(&o, 0), [Status::Accepted(1), Status::Rejected, Status::Pending]);
}

#[test]
fn progress_round_trip() {
    let mut o = open_with(HEADERLESS, None);
    decide(&mut o, 0, 0, Decision::Accept(0));
    decide(&mut o, 0, 2, Decision::Rewrite("Thanks!".into()));
    let progress = o.session.save_progress(&o.doc, 0);
    let want = statuses(&o, 0);

    // No session: the file alone gives the same statuses and text.
    let fresh = open_with(&progress, None);
    assert_eq!(statuses(&fresh, 0), want);
    assert_eq!(texts(&fresh.doc), texts(&o.doc));

    // Written over the same file with a session: same id, nothing applied twice.
    let json = saved(&o);
    let back = open_with(&progress, Some(&json));
    assert!(back.notes.is_empty(), "{:?}", back.notes);
    assert_eq!(back.session.reviews[0].id, o.session.reviews[0].id);
    assert_eq!(statuses(&back, 0), want);
    assert_eq!(back.doc.undo_depth(), o.doc.undo_depth());
    assert_eq!(back.doc.log().len(), o.doc.log().len());
}
