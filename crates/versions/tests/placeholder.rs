//! `blank_copy` (plan A3), written with `FdxFile::write_script` on the
//! original's file: what the written placeholder looks like.

use std::path::PathBuf;

use scriptorium_core::{ElementKind, ParaId, Script};
use scriptorium_fdx::{FdxFile, clear_text_offsets};
use scriptorium_versions::links::{self, Links, Member, Stamp};
use scriptorium_versions::placeholder::CREATED_BY_VERSION_NEW;
use scriptorium_versions::{BlankCopyError, blank_copy, stamp_text};

const FONT: &str = r#" Font="Courier Final Draft" Size="12""#;

/// Ids: 0 heading (Number, scene summary), 1 action, 2 character,
/// 3 parenthetical, 4 dialogue (three runs, first italic), 5 empty action,
/// 6–7 dual dialogue A, 8–9 dual dialogue B, 10 transition, 11 End of Act,
/// 12 heading with smart quotes and a lower-case letter. Outside `<Content>`:
/// a title page, spell-check ignored ranges and words, Mores and
/// Continueds settings, script notes.
fn film() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">

  <Content>
    <Paragraph Number="1" Type="Scene Heading">
      <SceneProperties Length="1/8" Page="1" Title="">
        <Summary>
          <Paragraph Alignment="Left">
            <Text Font="Courier">They argue.</Text>
          </Paragraph>
        </Summary>
      </SceneProperties>
      <Text{FONT}>INT. KITCHEN - DAY</Text>
    </Paragraph>
    <Paragraph Alignment="Left" FirstIndent="0.00" LeftIndent="1.50" Type="Action">
      <Text{FONT}>Maria cooks &amp; hums.</Text>
    </Paragraph>
    <Paragraph Type="Character">
      <Text{FONT}>MARIA (V.O.)</Text>
    </Paragraph>
    <Paragraph Type="Parenthetical">
      <Text{FONT}>(quietly)</Text>
    </Paragraph>
    <Paragraph Alignment="Left" LeftIndent="2.50" RightIndent="6.00" Type="Dialogue" StartsNewPage="No">
      <Text{FONT} Style="Italic" RevisionID="2">You </Text>
      <Text{FONT}>lied</Text>
      <Text{FONT}>!</Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text{FONT}></Text>
    </Paragraph>
    <Paragraph>
      <DualDialogue>
        <Paragraph Type="Character">
          <Text{FONT}>MARIA</Text>
        </Paragraph>
        <Paragraph Type="Dialogue">
          <Text{FONT}>Go!</Text>
        </Paragraph>
      </DualDialogue>
    </Paragraph>
    <Paragraph>
      <DualDialogue>
        <Paragraph Type="Character">
          <Text{FONT}>TOMAS</Text>
        </Paragraph>
        <Paragraph Type="Dialogue">
          <Text{FONT}>Stay!</Text>
        </Paragraph>
      </DualDialogue>
    </Paragraph>
    <Paragraph Type="Transition">
      <Text{FONT}>CUT TO:</Text>
    </Paragraph>
    <Paragraph Type="End of Act">
      <Text{FONT}>END OF ACT ONE</Text>
    </Paragraph>
    <Paragraph Number="2" Type="Scene Heading">
      <Text{FONT}>EXT. “THE PIER” – Night</Text>
    </Paragraph>
  </Content>
  <TitlePage>
    <Content>
      <Paragraph Type="Action">
        <Text>THE TITLE</Text>
      </Paragraph>
    </Content>
  </TitlePage>
  <SpellCheckIgnoreLists>
    <IgnoredRanges>
      <Range End="12" Start="5"/>
    </IgnoredRanges>
    <IgnoredWords>
      <Word>TOMAS</Word>
    </IgnoredWords>
  </SpellCheckIgnoreLists>
  <DialogueBreaks AutomaticCharacterContinueds="Yes" DialogueBottom="(MORE)" DialogueTop="(CONT'D)"/>
  <ScriptNotes>
    <ScriptNote Id="1" Range="20,31" WriterName="M">
      <Paragraph Alignment="Left">
        <Text Font="Arial">Too long.</Text>
      </Paragraph>
    </ScriptNote>
  </ScriptNotes>
</FinalDraft>
"#
    )
}

fn crlf(s: &str) -> String {
    s.replace('\n', "\r\n")
}

fn members(a_hash: &str) -> (Member, Member) {
    let a = Member { file: "FILM.fdx".into(), language: Some("en".into()), hash: a_hash.into(), created: None };
    let b = Member { file: "FILM.es.fdx".into(), language: Some("es".into()), hash: String::new(), created: None };
    (a, b)
}

/// `sessionfile::hash` (FNV-1a 64), copied: `versions` doesn't depend on
/// `review`. It must stay the same function as
/// `scriptorium_review::sessionfile::hash`, or the hand-test links file names
/// hashes the app and CLI don't recognise.
fn hash(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Make the placeholder of `raw` as `version new` will: blank_copy on the
/// file's script, write_script on the original's file, its text offsets
/// cleared, the version's hash into the links file.
fn make_as(raw: &str, id: &str, a: Member, b: Member) -> (FdxFile, Script, String, Links) {
    let f = FdxFile::parse(raw.to_string()).unwrap();
    let (script, mut links) = blank_copy(&f.to_script(), id, a, b, |k| f.is_caps(k.name())).unwrap();
    let out = clear_text_offsets(&f.write_script(&script).unwrap());
    links.members[1].hash = hash(&out);
    (f, script, out, links)
}

fn make(raw: &str) -> (FdxFile, Script, String, Links) {
    let (a, b) = members(&hash(raw));
    make_as(raw, "lk-test", a, b)
}

fn kinds(s: &Script) -> Vec<(ParaId, ElementKind)> {
    s.paragraphs.iter().map(|p| (p.id, p.kind.clone())).collect()
}

#[test]
fn written_placeholder_has_the_originals_paragraphs_ids_and_kinds() {
    for raw in [film(), crlf(&film())] {
        let (f, script, out, _) = make(&raw);
        let back = FdxFile::parse(out).unwrap();
        let back_script = back.to_script();
        assert_eq!(back.paragraphs().len(), f.paragraphs().len());
        assert_eq!(kinds(&back_script), kinds(&f.to_script()));
        // What blank_copy returns is what the file reads back as.
        assert_eq!(back_script, script);
        assert_eq!(back_script.fixed, f.to_script().fixed);
        assert_eq!(back_script.id_floor, f.to_script().id_floor);
    }
}

#[test]
fn headings_names_and_kept_lines_are_byte_identical() {
    for raw in [film(), crlf(&film())] {
        let (f, _, out, _) = make(&raw);
        let back = FdxFile::parse(out.clone()).unwrap();
        for i in [0, 2, 5, 6, 8, 11, 12] {
            assert_eq!(back.paragraphs()[i].runs, f.paragraphs()[i].runs, "paragraph {i}");
            assert_eq!(back.paragraphs()[i].text(), f.paragraphs()[i].text(), "paragraph {i}");
        }
        // Everything outside <Content>'s blanked runs is the original's:
        // the scene summary, scene numbers, title page, Mores and Continueds.
        for kept in [
            "<Paragraph Number=\"1\" Type=\"Scene Heading\">",
            "<Text Font=\"Courier\">They argue.</Text>",
            "<Paragraph Number=\"2\" Type=\"Scene Heading\">",
            "<Text>THE TITLE</Text>",
            "<DialogueBreaks AutomaticCharacterContinueds=\"Yes\" DialogueBottom=\"(MORE)\" DialogueTop=\"(CONT'D)\"/>",
            "MARIA (V.O.)",
            "END OF ACT ONE",
        ] {
            assert!(out.contains(kept), "lost {kept}");
        }
        for gone in ["Maria cooks", "(quietly)", "lied", "Go!", "Stay!", "CUT TO:"] {
            assert!(!out.contains(gone), "kept {gone}");
        }
        // Up to the first blanked paragraph and from the title page on, the
        // bytes are the original's.
        let start = raw.find("<Paragraph Alignment=\"Left\" FirstIndent").unwrap();
        assert_eq!(out[..start], raw[..start]);
        let tail = raw.find("  <TitlePage>").unwrap();
        assert!(out.ends_with(&clear_text_offsets(&raw)[tail..]));
        // The notes and ignored ranges pointed into the English text: gone,
        // their wrappers kept; ignored words are not offsets and stay.
        for gone in ["<ScriptNote ", "Too long.", "<Range "] {
            assert!(!out.contains(gone), "kept {gone}");
        }
        let eol = if raw.contains("\r\n") { "\r\n" } else { "\n" };
        for kept in [
            format!("<ScriptNotes>{eol}  </ScriptNotes>"),
            format!("<IgnoredRanges>{eol}    </IgnoredRanges>"),
            "<Word>TOMAS</Word>".to_string(),
        ] {
            assert!(out.contains(&kept), "lost {kept}");
        }
    }
}

#[test]
fn blanked_lines_keep_the_first_runs_attributes_with_empty_text() {
    for raw in [film(), crlf(&film())] {
        let (_, _, out, _) = make(&raw);
        let eol = if raw.contains("\r\n") { "\r\n" } else { "\n" };
        let dialogue = format!(
            "<Paragraph Alignment=\"Left\" LeftIndent=\"2.50\" RightIndent=\"6.00\" Type=\"Dialogue\" StartsNewPage=\"No\">{eol}      <Text{FONT} Style=\"Italic\" RevisionID=\"2\"></Text>{eol}    </Paragraph>"
        );
        assert!(out.contains(&dialogue), "{out}");
        let action = format!(
            "<Paragraph Alignment=\"Left\" FirstIndent=\"0.00\" LeftIndent=\"1.50\" Type=\"Action\">{eol}      <Text{FONT}></Text>{eol}"
        );
        assert!(out.contains(&action));
        if eol == "\r\n" {
            assert!(!out.replace("\r\n", "").contains('\n'), "a bare LF in a CRLF file");
        }
    }
}

#[test]
fn dual_dialogue_is_blanked_and_stays_fixed() {
    let (f, script, out, links) = make(&film());
    let back = FdxFile::parse(out.clone()).unwrap();
    assert_eq!(back.wrappers().len(), 2);
    let texts: Vec<String> = (6..10).map(|i| back.paragraphs()[i].text()).collect();
    assert_eq!(texts, ["MARIA", "", "TOMAS", ""]);
    assert_eq!(script.fixed, f.to_script().fixed);
    assert!(script.is_fixed(ParaId(7)) && script.fixed_group(ParaId(7)) == Some(0));
    assert_eq!(links.rows[7].cells, vec![vec![ParaId(7)], vec![ParaId(7)]]);
    assert_eq!(out.matches("<DualDialogue>").count(), 2);
}

#[test]
fn the_placeholders_no_op_round_trip_is_byte_identical() {
    for raw in [film(), crlf(&film())] {
        let (_, _, out, _) = make(&raw);
        let g = FdxFile::parse(out.clone()).unwrap();
        assert_eq!(g.write_script(&g.to_script()).unwrap(), out);
    }
}

#[test]
fn links_rows_and_stamps() {
    let raw = film();
    let (f, _, out, links) = make(&raw);
    let n = f.paragraphs().len();
    assert_eq!(links.id, "lk-test");
    assert_eq!(links.members[0].hash, hash(&raw));
    assert_eq!(links.members[1].hash, hash(&out));
    assert_eq!(links.members[0].created, None);
    assert_eq!(links.members[1].created.as_deref(), Some(CREATED_BY_VERSION_NEW));
    assert_eq!(links.made(), Some(1));
    assert_eq!(links.rows.len(), n);
    for (i, row) in links.rows.iter().enumerate() {
        let id = ParaId(i as u64);
        assert_eq!(row.cells, vec![vec![id], vec![id]]);
        assert_eq!(links.paras[0][&id].kind, links.paras[1][&id].kind);
    }
    let synced = |t: &str| Some(Stamp::Synced(t.to_string()));
    let placeholder = |t: &str| Some(Stamp::Placeholder(t.to_string()));
    let a = |i: u64| links.paras[0][&ParaId(i)].stamp.clone();
    let b = |i: u64| links.paras[1][&ParaId(i)].stamp.clone();
    // The original's side: its normalized text, all `=`.
    assert_eq!(a(1), synced("Maria cooks & hums."));
    assert_eq!(a(4), synced("You lied!"));
    // Heading `~` its text; smart quotes and dashes normalized, caps upper-cased.
    assert_eq!(b(0), placeholder("INT. KITCHEN - DAY"));
    assert_eq!(b(12), placeholder("EXT. \"THE PIER\" - NIGHT"));
    assert_eq!(a(12), synced("EXT. \"THE PIER\" - NIGHT"));
    // Character, non-editable and empty: kept, `=`.
    assert_eq!(b(2), synced("MARIA (V.O.)"));
    assert_eq!(b(6), synced("MARIA"));
    assert_eq!(b(11), synced("END OF ACT ONE"));
    assert_eq!(b(5), synced(""));
    assert_eq!(a(5), synced(""));
    // Everything else: emptied, `~ ""`.
    for i in [1, 3, 4, 7, 9, 10] {
        assert_eq!(b(i), placeholder(""), "b{i}");
    }
    assert_eq!(links.paras[1][&ParaId(11)].kind, ElementKind::Other("End of Act".into()));

    // It's a valid links file that round-trips.
    let text = links::write(&links).unwrap();
    assert_eq!(links::parse(&text).unwrap(), links);
}

#[test]
fn stamp_text_normalizes_and_upper_cases_caps_kinds() {
    assert_eq!(stamp_text("  It’s…  fine — ok ", false), "It's... fine - ok");
    assert_eq!(stamp_text("int. bar – night", true), "INT. BAR - NIGHT");
    assert_eq!(stamp_text("a\nb", false), "a b");
}

#[test]
fn refuses_an_empty_script_and_ids_that_arent_the_files() {
    let (a, b) = members("0123456789abcdef");
    let none = |_: &ElementKind| false;
    assert_eq!(blank_copy(&Script::default(), "lk", a.clone(), b.clone(), none), Err(BlankCopyError::Empty));
    let mut s = FdxFile::parse(film()).unwrap().to_script();
    s.paragraphs.remove(3);
    let err = blank_copy(&s, "lk", a, b, none).unwrap_err();
    assert_eq!(err, BlankCopyError::NotFileIds { index: 3, id: ParaId(4) });
    assert!(err.to_string().contains("line 4"));
}

/// The real script, if present next to the project (it's kept out of the
/// repo). Override with SCRIPTORIUM_REAL_FDX=/path/to/script.fdx.
fn real_script() -> Option<String> {
    let path = std::env::var_os("SCRIPTORIUM_REAL_FDX")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../2026_11_05_PURPLE LIST.fdx"));
    std::fs::read_to_string(path).ok()
}

#[test]
fn real_script_placeholder() {
    let Some(raw) = real_script() else {
        eprintln!("skipped: real script not found");
        return;
    };
    let (f, script, out, links) = make(&raw);
    let back = FdxFile::parse(out.clone()).unwrap();
    assert_eq!(back.to_script(), script);
    assert_eq!(kinds(&back.to_script()), kinds(&f.to_script()));
    assert_eq!(back.wrappers().len(), f.wrappers().len());
    for (i, (p, q)) in f.paragraphs().iter().zip(back.paragraphs()).enumerate() {
        let kept = matches!(ElementKind::from_name(&p.kind), ElementKind::SceneHeading | ElementKind::Character)
            || !ElementKind::from_name(&p.kind).is_editable()
            || p.text().trim().is_empty();
        if kept {
            assert_eq!(p.runs, q.runs, "paragraph {i}");
        } else {
            assert_eq!(q.text(), "", "paragraph {i}");
            assert_eq!(q.runs.len(), 1, "paragraph {i}");
            assert_eq!(q.runs[0].attrs, p.runs[0].attrs, "paragraph {i}");
        }
    }
    assert_eq!(back.write_script(&back.to_script()).unwrap(), out);
    assert_eq!(back.eol(), f.eol());
    // Outside <Content> nothing changed but the emptied notes and ranges.
    let tail = |s: &str| s[s.find("</Content>").unwrap()..].to_string();
    assert_eq!(tail(&out), tail(&clear_text_offsets(&raw)));
    assert!(raw.contains("<ScriptNote ") && raw.contains("<Range "), "the real script has notes and ranges");
    assert!(!out.contains("<ScriptNote ") && !out.contains("<Range "));
    assert!(out.contains("<ScriptNotes>") && out.contains("<IgnoredRanges>"));
    let head = |s: &str| s[..s.find("<Content>").unwrap()].to_string();
    assert_eq!(head(&out), head(&raw));
    assert_eq!(links::parse(&links::write(&links).unwrap()).unwrap(), links);
}

/// Hand test 1 (plan A3): the placeholder of the real script, for Final
/// Draft, with its links file and a copy of the original, all in
/// `target/handtest/`. Never written next to the real script. Run it with
/// `cargo test -p scriptorium-versions --test placeholder -- --ignored real_script_handtest_files`.
#[test]
#[ignore = "writes target/handtest/ for hand test 1"]
fn real_script_handtest_files() {
    let Some(raw) = real_script() else {
        eprintln!("skipped: real script not found");
        return;
    };
    let a = Member { file: "PURPLE LIST.fdx".into(), language: Some("en".into()), hash: hash(&raw), created: None };
    let b =
        Member { file: "PURPLE LIST.es.fdx".into(), language: Some("es".into()), hash: String::new(), created: None };
    let id = format!("lk-{}", &hash(&format!("handtest\n{raw}"))[..12]);
    let (_, _, out, links) = make_as(&raw, &id, a, b);
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/handtest");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("PURPLE LIST.fdx"), &raw).unwrap();
    std::fs::write(dir.join("PURPLE LIST.es.fdx"), &out).unwrap();
    std::fs::write(dir.join("PURPLE LIST.es.links.txt"), links::write(&links).unwrap()).unwrap();
    eprintln!("hand test files in {}", dir.display());
}
