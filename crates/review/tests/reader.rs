//! Sessions saved by an older script reader (A1): the session file records
//! the reader version, and a session whose paragraph ids the new reader would
//! give to other lines is discarded rather than replayed onto the wrong lines.

use std::collections::HashSet;

use scriptorium_core::{ParaId, Pos, Selection};
use scriptorium_fdx::FdxFile;
use scriptorium_review::sessionfile::{self, READER, Resumed, SessionFile};

fn inner(kind: &str, text: &str) -> String {
    format!("        <Paragraph Type=\"{kind}\">\n          <Text>{text}</Text>\n        </Paragraph>\n")
}

/// A script with one DualDialogue block; `self_closing` makes its first inner
/// child `<Paragraph Type="Character"/>`, which the old reader misnumbered.
fn script(self_closing: bool) -> String {
    let first = if self_closing {
        "        <Paragraph Type=\"Character\"/>\n".to_string()
    } else {
        inner("Character", "MARIA")
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<FinalDraft DocumentType=\"Script\" Version=\"5\">\n  <Content>\n\
         {}    <Paragraph>\n      <DualDialogue>\n{first}{}{}{}      </DualDialogue>\n    </Paragraph>\n{}  </Content>\n</FinalDraft>\n",
        inner("Scene Heading", "INT. KITCHEN - DAY").replace("        ", "    "),
        inner("Dialogue", "Go!"),
        inner("Character", "TOMAS"),
        inner("Dialogue", "Stay!"),
        inner("Action", "They stare.").replace("        ", "    "),
    )
}

fn resume(f: &FdxFile, saved: Option<&str>) -> sessionfile::ResumedSession {
    sessionfile::resume(
        vec![],
        f.to_script(),
        Some(&f.legacy_script()),
        f.raw(),
        saved,
        &|k| f.is_caps(k.name()),
        "s.fdx",
        None,
    )
}

/// A session with one edit to the last line (the action), as `reader` would save it.
fn saved_session(f: &FdxFile, reader: Option<u32>) -> String {
    saved_edit(f, reader, ParaId(f.to_script().paragraphs.len() as u64 - 1))
}

/// A session with one edit (`Then ` typed at its start) to line `para`, as `reader` would save it.
fn saved_edit(f: &FdxFile, reader: Option<u32>, para: ParaId) -> String {
    let mut o = resume(f, None);
    o.doc.insert_text(Selection::cursor(Pos::new(para, 0)), "Then ").unwrap();
    let mut file = o.to_file(f.raw());
    assert_eq!(file.reader, READER);
    let mut json: serde_json::Value = serde_json::from_str(&file.to_json()).unwrap();
    match reader {
        Some(r) => file.reader = r,
        None => {
            json.as_object_mut().unwrap().remove("reader");
            return json.to_string();
        }
    }
    file.to_json()
}

#[test]
fn reader_version_matches_the_fdx_crate() {
    assert_eq!(READER, scriptorium_fdx::READER_VERSION);
}

#[test]
fn files_without_a_reader_are_reader_1() {
    let f = FdxFile::parse(script(false)).unwrap();
    assert_eq!(SessionFile::from_json(&saved_session(&f, None)).unwrap().reader, 1);
}

#[test]
fn an_old_session_is_kept_when_its_ids_mean_the_same_lines() {
    let f = FdxFile::parse(script(false)).unwrap();
    for saved in [saved_session(&f, None), saved_session(&f, Some(1)), saved_session(&f, Some(READER))] {
        let o = resume(&f, Some(&saved));
        assert_eq!(o.how, Resumed::Resumed { events: 1 });
        assert_eq!(o.doc.paragraphs().last().unwrap().text(), "Then They stare.");
        // Saving again records the current reader.
        assert_eq!(o.to_file(f.raw()).reader, READER);
    }
}

#[test]
fn an_old_session_is_discarded_when_its_ids_would_shift() {
    let f = FdxFile::parse(script(true)).unwrap();
    assert!(!sessionfile::same_ids(&f.legacy_script(), &f.to_script(), &HashSet::new()));
    // Saved by the new reader: fine.
    let o = resume(&f, Some(&saved_session(&f, Some(READER))));
    assert_eq!(o.how, Resumed::Resumed { events: 1 });
    // Saved by the old reader (with or without the field): set aside, with a reason.
    for saved in [saved_session(&f, None), saved_session(&f, Some(1))] {
        let o = resume(&f, Some(&saved));
        let Resumed::Discarded(why) = &o.how else { panic!("kept: {:?}", o.how) };
        assert!(why.contains("older Scriptorium"), "{why}");
        assert_eq!(o.doc.paragraphs().last().unwrap().text(), "They stare.");
    }
}

#[test]
fn a_session_from_a_newer_reader_is_discarded() {
    let f = FdxFile::parse(script(false)).unwrap();
    let o = resume(&f, Some(&saved_session(&f, Some(READER + 1))));
    assert!(matches!(o.how, Resumed::Discarded(_)));
}

/// `script(false)` with a scene summary on its heading, after (or before) the
/// heading text: reader 1 read the summary as the heading's text.
fn with_summary(raw: &str, before: bool) -> String {
    let summary = "      <SceneProperties>\n        <Summary>\n          <Paragraph>\n            \
                   <Text>They argue.</Text>\n          </Paragraph>\n        </Summary>\n      </SceneProperties>\n";
    let text = "      <Text>INT. KITCHEN - DAY</Text>\n";
    let both = if before { format!("{summary}{text}") } else { format!("{text}{summary}") };
    let out = raw.replacen(text, &both, 1);
    assert_ne!(out, raw);
    out
}

#[test]
fn an_old_session_on_a_script_with_scene_summaries_is_kept() {
    for before in [true, false] {
        let f = FdxFile::parse(with_summary(&script(false), before)).unwrap();
        assert_eq!(f.to_script().paragraphs[0].text(), "INT. KITCHEN - DAY");
        assert_ne!(f.legacy_script().paragraphs[0].text(), "INT. KITCHEN - DAY");
        assert!(sessionfile::same_ids(&f.legacy_script(), &f.to_script(), &HashSet::new()));
        for saved in [saved_session(&f, None), saved_session(&f, Some(1))] {
            let o = resume(&f, Some(&saved));
            assert_eq!(o.how, Resumed::Resumed { events: 1 });
            assert_eq!(o.doc.paragraphs().last().unwrap().text(), "Then They stare.");
        }
    }
}

/// No DualDialogue, but a scene summary of two paragraphs: reader 1 ended the
/// heading at the summary's first `</Paragraph>` and read the second summary
/// paragraph as a line of its own, so every later id shifted. Callers always
/// pass the legacy script (not only for files with DualDialogue).
#[test]
fn an_old_session_on_a_two_paragraph_summary_is_discarded() {
    let raw = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<FinalDraft DocumentType=\"Script\" Version=\"5\">\n  <Content>\n\
               \x20   <Paragraph Type=\"Scene Heading\">\n      <SceneProperties>\n        <Summary>\n\
               \x20         <Paragraph>\n            <Text>They argue.</Text>\n          </Paragraph>\n\
               \x20         <Paragraph>\n            <Text>She leaves.</Text>\n          </Paragraph>\n\
               \x20       </Summary>\n      </SceneProperties>\n      <Text>INT. KITCHEN - DAY</Text>\n    </Paragraph>\n\
               \x20   <Paragraph Type=\"Action\">\n      <Text>They stare.</Text>\n    </Paragraph>\n  </Content>\n</FinalDraft>\n";
    let f = FdxFile::parse(raw).unwrap();
    assert!(f.wrappers().is_empty());
    assert_eq!(f.to_script().paragraphs.len(), 2);
    assert_ne!(f.legacy_script().paragraphs.len(), 2, "reader 1 numbered this file differently");
    for saved in [saved_session(&f, None), saved_session(&f, Some(1))] {
        let o = resume(&f, Some(&saved));
        assert!(matches!(o.how, Resumed::Discarded(_)), "kept: {:?}", o.how);
        assert_eq!(o.doc.paragraphs().last().unwrap().text(), "They stare.", "nothing misapplied");
    }
}

/// The scene-summary exception covers only headings the saved session never
/// edited: an edit to one was made on reader 1's text (summary included), so
/// replaying it onto reader 2's text would put it in the wrong place.
#[test]
fn an_old_session_that_edited_a_summarised_heading_is_discarded() {
    let f = FdxFile::parse(with_summary(&script(false), true)).unwrap();
    let heading = ParaId(0);
    assert!(!sessionfile::same_ids(&f.legacy_script(), &f.to_script(), &HashSet::from([heading])));
    for saved in [saved_edit(&f, None, heading), saved_edit(&f, Some(1), heading)] {
        let o = resume(&f, Some(&saved));
        let Resumed::Discarded(why) = &o.how else { panic!("kept: {:?}", o.how) };
        assert!(why.contains("scene summaries"), "{why}");
        assert_eq!(o.doc.paragraphs()[0].text(), "INT. KITCHEN - DAY", "nothing misapplied");
    }
    // Saved by reader 2, or with the heading's text the same under both readers: kept.
    let o = resume(&f, Some(&saved_edit(&f, Some(READER), heading)));
    assert_eq!(o.how, Resumed::Resumed { events: 1 });
    let plain = FdxFile::parse(script(false)).unwrap();
    let o = resume(&plain, Some(&saved_edit(&plain, Some(1), heading)));
    assert_eq!(o.how, Resumed::Resumed { events: 1 });
    assert_eq!(o.doc.paragraphs()[0].text(), "Then INT. KITCHEN - DAY");
}

#[test]
fn touched_ids_are_the_lines_the_ops_name() {
    let f = FdxFile::parse(script(false)).unwrap();
    let saved = SessionFile::from_json(&saved_edit(&f, Some(1), ParaId(2))).unwrap();
    let events = saved.base.iter().flatten().chain(&saved.events);
    assert_eq!(sessionfile::touched_ids(events), HashSet::from([ParaId(2)]));
}
