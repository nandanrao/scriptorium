use std::collections::BTreeMap;
use std::path::PathBuf;

use scriptorium_core::outline::annotate;
use scriptorium_core::{ElementKind, ParaId, Style};
use scriptorium_fdx::*;

enum R<'a> {
    Plain(&'a str),
    Styled(&'a str, &'a str),
}
use R::*;

fn para(kind: &str, runs: &[R]) -> String {
    let body: Vec<String> = runs
        .iter()
        .map(|r| match r {
            Plain(t) => format!("      <Text>{t}</Text>"),
            Styled(a, t) => format!("      <Text{a}>{t}</Text>"),
        })
        .collect();
    format!("    <Paragraph Type=\"{kind}\">\n{}\n    </Paragraph>\n", body.join("\n"))
}

fn doc(paras: &[String]) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\" ?>\n\
         <FinalDraft DocumentType=\"Script\" Template=\"No\" Version=\"5\">\n\n  <Content>\n{}  </Content>\n  \
         <TitlePage>\n    <Content>\n    <Paragraph Type=\"Action\">\n      <Text>TITLE PAGE TEXT</Text>\n    \
         </Paragraph>\n    </Content>\n  </TitlePage>\n</FinalDraft>\n",
        paras.concat()
    )
}

fn sample() -> String {
    doc(&[
        para("Scene Heading", &[Plain("EXT. FARM HOUSE - NIGHT")]),
        para(
            "Action",
            &[Plain("They are playing "), Styled(" Style=\"Italic\"", "truco"), Plain(" (Argentinian cards game).")],
        ),
        para("Character", &[Plain("CESAR")]),
        para("Dialogue", &[Plain("I am go to the town tomorrow.")]),
        para("Parenthetical", &[Plain("(smiling)")]),
        para("Dialogue", &[Plain("Thank you.")]),
        para("Action", &[Plain("Laura smiles. ")]),
        para("Character", &[Plain("LAURA")]),
        para("Dialogue", &[Plain("Thank you.")]),
        para("Action", &[Plain("She say “hello” to everybody &amp; leaves.")]),
    ])
}

fn edits(pairs: &[(usize, &str)]) -> BTreeMap<usize, String> {
    pairs.iter().map(|(i, t)| (*i, t.to_string())).collect()
}

fn texts(runs: &[Run]) -> Vec<(&str, &str)> {
    runs.iter().map(|r| (r.attrs.as_str(), r.text.as_str())).collect()
}

#[test]
fn decode_and_escape() {
    assert_eq!(
        decode_entities("a &amp; b &lt;c&gt; &quot;d&quot; &apos;e&apos; &#10;&#xA;&#233;"),
        "a & b <c> \"d\" 'e' \n\né"
    );
    assert_eq!(escape_text("a & <b>"), "a &amp; &lt;b&gt;");
}

#[test]
fn parses_only_main_content_with_types_scenes_speakers() {
    let f = FdxFile::parse(sample()).unwrap();
    let script = f.to_script();
    assert_eq!(script.paragraphs.len(), 10);
    assert!(!script.paragraphs.iter().any(|p| p.text().contains("TITLE PAGE")));
    assert_eq!(script.paragraphs[1].text(), "They are playing truco (Argentinian cards game).");
    assert_eq!(script.paragraphs[9].text(), "She say “hello” to everybody & leaves.");
    assert_eq!(script.paragraphs[0].kind, ElementKind::SceneHeading);
    assert!(script.paragraphs.iter().all(|p| p.kind.is_editable()));
    assert!(script.paragraphs[1].spans[1].style.italic);
    assert_eq!(script.paragraphs[1].spans[0].style, Style::default());

    let ctx = annotate(&script);
    assert_eq!(ctx[1].scene, "EXT. FARM HOUSE - NIGHT");
    assert_eq!(ctx[3].speaker, "CESAR");
    assert_eq!(ctx[4].speaker, "CESAR");
    assert_eq!(ctx[8].speaker, "LAURA");
    assert_eq!(ctx[6].speaker, "");
}

#[test]
fn rejects_non_final_draft_input() {
    assert_eq!(FdxFile::parse("<html></html>").unwrap_err(), FdxError::NotFinalDraft);
}

#[test]
fn parse_runs_keeps_raw_attribute_strings() {
    let runs = parse_runs("<Paragraph><Text AdornmentStyle=\"-1\" Style=\"Italic\">x</Text><Text/></Paragraph>");
    assert_eq!(texts(&runs), [(" AdornmentStyle=\"-1\" Style=\"Italic\"", "x"), ("", "")]);
    assert!(runs[0].style().italic);
    assert!(!Run::new(" AdornmentStyle=\"Bold\"", "").style().bold, "attribute names must match exactly");
}

#[test]
fn restyle_keeps_styles_and_extends_neighbours() {
    let runs =
        [Run::new("", "They are playing "), Run::new(" Style=\"Italic\"", "truco"), Run::new("", " (cards game).")];
    let out = restyle(&runs, "They are playing truco (a card game).");
    assert_eq!(texts(&out), [("", "They are playing "), (" Style=\"Italic\"", "truco"), ("", " (a card game).")]);

    let out = restyle(&[Run::new("", "a "), Run::new(" Style=\"Bold\"", "big"), Run::new("", " dog")], "a huge dog");
    assert_eq!(texts(&out), [("", "a "), (" Style=\"Bold\"", "huge"), ("", " dog")]);
}

#[test]
fn restyle_handles_empty_input_and_output() {
    assert_eq!(texts(&restyle(&[], "new")), [("", "new")]);
    assert_eq!(texts(&restyle(&[Run::new(" X=\"1\"", "gone")], "")), [(" X=\"1\"", "")]);
}

#[test]
fn rewrite_paragraph_keeps_indentation_and_other_children() {
    let xml = "<Paragraph Type=\"Action\">\n      <SceneProperties Length=\"1\"/>\n      <Text>Old text.</Text>\n    </Paragraph>";
    assert_eq!(
        rewrite_paragraph(xml, "New & text.", "\n"),
        "<Paragraph Type=\"Action\">\n      <SceneProperties Length=\"1\"/>\n      <Text>New &amp; text.</Text>\n    </Paragraph>"
    );
}

#[test]
fn rewrite_paragraph_fills_paragraphs_without_text() {
    assert!(rewrite_paragraph("<Paragraph Type=\"Action\">\n    </Paragraph>", "Hi", "\n").contains("<Text>Hi</Text>"));
    assert_eq!(
        rewrite_paragraph("<Paragraph Type=\"Action\"/>", "Hi", "\n"),
        "<Paragraph Type=\"Action\">\n      <Text>Hi</Text>\n    </Paragraph>"
    );
}

#[test]
fn no_op_writes_are_byte_identical() {
    let raw = sample();
    let f = FdxFile::parse(raw.clone()).unwrap();
    assert_eq!(f.write_edits(&BTreeMap::new()), raw);
    assert_eq!(f.write_edits(&edits(&[(3, "I am go to the town tomorrow.")])), raw);
    assert_eq!(f.write_script(&f.to_script()).unwrap(), raw);
}

#[test]
fn writes_only_edited_paragraphs_and_round_trips() {
    let raw = sample();
    let f = FdxFile::parse(raw.clone()).unwrap();
    let out = f.write_edits(&edits(&[
        (3, "I am going to town tomorrow."),
        (1, "They are playing truco (an Argentinian card game)."),
    ]));
    let back = FdxFile::parse(out.clone()).unwrap().to_script();
    assert_eq!(back.paragraphs[3].text(), "I am going to town tomorrow.");
    assert_eq!(back.paragraphs[1].text(), "They are playing truco (an Argentinian card game).");
    assert!(out.contains("<Text Style=\"Italic\">truco</Text>"));
    let undone = out
        .replace("I am going to town tomorrow.", "I am go to the town tomorrow.")
        .replace(" (an Argentinian card game).", " (Argentinian cards game).");
    assert_eq!(undone, raw);
}

#[test]
fn write_script_applies_text_edits_and_refuses_reordering() {
    let f = FdxFile::parse(sample()).unwrap();
    let mut s = f.to_script();
    s.paragraphs[3].spans[0].text = "I am going to town tomorrow.".into();
    let out = f.write_script(&s).unwrap();
    assert!(out.contains("<Text>I am going to town tomorrow.</Text>"));

    let mut moved = f.to_script();
    moved.paragraphs.swap(2, 3);
    assert!(matches!(f.write_script(&moved), Err(FdxError::StructureChanged { .. })));
    let mut duplicated = f.to_script();
    duplicated.paragraphs.push(duplicated.paragraphs[0].clone());
    assert!(matches!(f.write_script(&duplicated), Err(FdxError::StructureChanged { .. })));
}

#[test]
fn write_script_refuses_unwritable_characters_in_new_or_edited_text() {
    let f = FdxFile::parse(sample()).unwrap();
    for bad in ["a\u{1}b", "a\rb", "a\nb", "a\u{FFFE}", "a\u{FFFF}", "a\u{8}"] {
        let mut edited = f.to_script();
        edited.paragraphs[3].spans[0].text = bad.into();
        let id = edited.paragraphs[3].id;
        assert_eq!(
            f.write_script(&edited),
            Err(FdxError::BadText { para: id, ch: bad.chars().nth(1).unwrap() }),
            "{bad:?}"
        );

        let mut inserted = f.to_script();
        let mut p = inserted.paragraphs[3].clone();
        p.id = ParaId(900);
        p.spans[0].text = bad.into();
        inserted.paragraphs.push(p);
        assert!(matches!(f.write_script(&inserted), Err(FdxError::BadText { para: ParaId(900), .. })), "{bad:?}");
    }
    // Tab and ordinary text are fine.
    let mut ok = f.to_script();
    ok.paragraphs[3].spans[0].text = "a\tb".into();
    assert!(f.write_script(&ok).is_ok());
}

#[test]
fn caps_types_from_file_with_defaults() {
    let settings = "<ElementSettings Type=\"Character\"><FontSpec Style=\"Bold\"/></ElementSettings>\
                    <ElementSettings Type=\"Action\"><FontSpec Style=\"AllCaps\"/></ElementSettings>";
    let caps = caps_types(&sample().replace("</FinalDraft>", &format!("{settings}</FinalDraft>")));
    assert!(!caps.contains("Character"), "file overrides the default");
    assert!(caps.contains("Action"), "custom all-caps type");
    assert!(caps.contains("Scene Heading"), "default kept when the file is silent");
    let f = FdxFile::parse(sample()).unwrap();
    let caps_kinds: Vec<&str> = f.paragraphs().iter().filter(|p| f.is_caps(&p.kind)).map(|p| p.kind.as_str()).collect();
    assert_eq!(caps_kinds, ["Scene Heading", "Character", "Character"]);
}

/// The real script, if present next to the project (it's kept out of the repo).
/// Override with SCRIPTORIUM_REAL_FDX=/path/to/script.fdx.
fn real_script() -> Option<String> {
    let path = std::env::var_os("SCRIPTORIUM_REAL_FDX")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../2026_11_05_PURPLE LIST.fdx"));
    std::fs::read_to_string(path).ok()
}

#[test]
fn real_script_parses_and_no_op_write_is_byte_identical() {
    let Some(raw) = real_script() else {
        eprintln!("skipped: real script not found");
        return;
    };
    let f = FdxFile::parse(raw.clone()).unwrap();
    let script = f.to_script();
    assert!(!script.paragraphs.is_empty());
    assert_eq!(f.write_edits(&BTreeMap::new()), raw);
    assert_eq!(f.write_script(&script).unwrap(), raw);

    // An edit to one paragraph round-trips (content-independent: the first
    // paragraph with a word to change).
    let Some(i) = script.paragraphs.iter().position(|p| p.text().contains('e')) else { return };
    let new_text = script.paragraphs[i].text().replacen('e', "ee", 1);
    let out = f.write_edits(&edits(&[(i, &new_text)]));
    let reparsed = FdxFile::parse(out).unwrap().to_script();
    assert_eq!(reparsed.paragraphs.len(), script.paragraphs.len());
    assert_eq!(reparsed.paragraphs[i].text(), new_text);
}

/// A paragraph as Final Draft nests it inside `<DualDialogue>` (deeper indent).
fn inner(kind: &str, text: &str) -> String {
    format!("        <Paragraph Type=\"{kind}\">\n          <Text>{text}</Text>\n        </Paragraph>\n")
}

fn dual(members: &[String]) -> String {
    format!("    <Paragraph>\n      <DualDialogue>\n{}      </DualDialogue>\n    </Paragraph>\n", members.concat())
}

/// Two DualDialogue blocks, back to back after the first; the second's first
/// inner child is self-closing (an empty Character).
fn dual_sample() -> String {
    doc(&[
        para("Scene Heading", &[Plain("INT. KITCHEN - DAY")]),
        dual(&[
            inner("Character", "MARIA"),
            inner("Dialogue", "Go!"),
            inner("Character", "TOMAS"),
            inner("Dialogue", "Stay!"),
        ]),
        dual(&[
            "        <Paragraph Type=\"Character\"/>\n".to_string(),
            inner("Dialogue", "Who said that?"),
            inner("Character", "TOMAS"),
            inner("Dialogue", "Me."),
        ]),
        para("Action", &[Plain("They stare.")]),
    ])
}

fn kinds_and_texts(f: &FdxFile) -> Vec<(String, String)> {
    f.paragraphs().iter().map(|p| (p.kind.clone(), p.text())).collect()
}

fn expected_dual() -> Vec<(String, String)> {
    [
        ("Scene Heading", "INT. KITCHEN - DAY"),
        ("Character", "MARIA"),
        ("Dialogue", "Go!"),
        ("Character", "TOMAS"),
        ("Dialogue", "Stay!"),
        ("Character", ""),
        ("Dialogue", "Who said that?"),
        ("Character", "TOMAS"),
        ("Dialogue", "Me."),
        ("Action", "They stare."),
    ]
    .iter()
    .map(|(k, t)| (k.to_string(), t.to_string()))
    .collect()
}

#[test]
fn dual_dialogue_members_are_ordinary_paragraphs_without_a_phantom() {
    for raw in [dual_sample(), dual_sample().replace('\n', "\r\n")] {
        let f = FdxFile::parse(raw.clone()).unwrap();
        assert_eq!(kinds_and_texts(&f), expected_dual());
        let wrappers: Vec<Option<usize>> = f.paragraphs().iter().map(|p| p.wrapper).collect();
        assert_eq!(wrappers, [None, Some(0), Some(0), Some(0), Some(0), Some(1), Some(1), Some(1), Some(1), None]);

        // Each wrapper span is the whole outer element, and the wrappers hold their members.
        assert_eq!(f.wrappers().len(), 2);
        for w in f.wrappers() {
            assert!(raw[w.clone()].starts_with("<Paragraph>"));
            assert!(
                raw[w.clone()].ends_with("</DualDialogue>\r\n    </Paragraph>")
                    || raw[w.clone()].ends_with("</DualDialogue>\n    </Paragraph>")
            );
        }

        // Members are fixed, grouped by wrapper; the rest aren't.
        let s = f.to_script();
        let fixed: Vec<(u64, u32)> = s.fixed.iter().map(|(id, g)| (id.0, *g)).collect();
        assert_eq!(fixed, [(1, 0), (2, 0), (3, 0), (4, 0), (5, 1), (6, 1), (7, 1), (8, 1)]);
        assert!(!s.is_fixed(scriptorium_core::ParaId(0)) && !s.is_fixed(scriptorium_core::ParaId(9)));
        assert_eq!(s.fixed_group(scriptorium_core::ParaId(6)), Some(1));
        assert_eq!(s.paragraphs[5].kind, ElementKind::Character);

        // No-op writes are byte-identical.
        assert_eq!(f.write_edits(&BTreeMap::new()), raw);
        assert_eq!(f.write_script(&s).unwrap(), raw);
    }
}

#[test]
fn dual_dialogue_text_edits_touch_only_their_paragraph() {
    let raw = dual_sample();
    let f = FdxFile::parse(raw.clone()).unwrap();
    let out = f.write_edits(&edits(&[(3, "TOMÁS"), (6, "Who said that, then?")]));
    let back = FdxFile::parse(out.clone()).unwrap();
    let mut want = expected_dual();
    want[3].1 = "TOMÁS".into();
    want[6].1 = "Who said that, then?".into();
    assert_eq!(kinds_and_texts(&back), want);
    assert_eq!(back.wrappers().len(), 2);
    // Everything outside the two edited <Text> runs is unchanged.
    assert_eq!(out.replace("TOMÁS", "TOMAS").replacen("Who said that, then?", "Who said that?", 1), raw);
}

#[test]
fn legacy_reader_reads_dual_dialogue_as_before() {
    let f = FdxFile::parse(dual_sample()).unwrap();
    let old: Vec<(String, String)> = f.legacy_paragraphs().iter().map(|p| (p.kind.clone(), p.text())).collect();
    // The phantom takes the outer paragraph's (missing) Type and the first
    // inner runs; with a self-closing first child it swallows the second too.
    let want: Vec<(String, String)> = [
        ("Scene Heading", "INT. KITCHEN - DAY"),
        ("", "MARIA"),
        ("Dialogue", "Go!"),
        ("Character", "TOMAS"),
        ("Dialogue", "Stay!"),
        ("", "Who said that?"),
        ("Character", "TOMAS"),
        ("Dialogue", "Me."),
        ("Action", "They stare."),
    ]
    .iter()
    .map(|(k, t)| (k.to_string(), t.to_string()))
    .collect();
    assert_eq!(old, want);
    assert!(f.legacy_script().fixed.is_empty());
    // Without dual dialogue both readers agree.
    let plain = FdxFile::parse(sample()).unwrap();
    assert_eq!(plain.legacy_script(), plain.to_script());
}

#[test]
fn scanner_ignores_lookalike_tags_and_handles_self_closing_paragraphs() {
    let raw = doc(&[
        "    <ParagraphSpacing Value=\"1\"/>\n".to_string(),
        "    <Paragraph Type=\"Action\"/>\n".to_string(),
        para("Action", &[Plain("After.")]),
    ]);
    let f = FdxFile::parse(raw.clone()).unwrap();
    let kt = kinds_and_texts(&f);
    assert_eq!(kt, [("Action".to_string(), String::new()), ("Action".to_string(), "After.".to_string())]);
    assert_eq!(f.write_edits(&BTreeMap::new()), raw);
}

/// Final Draft's scene summary: a `<Paragraph>` nested in the heading's `<SceneProperties>`.
const SUMMARY: &str = "      <SceneProperties Length=\"1/8\" Page=\"1\" Title=\"\">\n        <Summary>\n          \
                       <Paragraph Alignment=\"Left\">\n            <Text Font=\"Courier\">She &amp; he argue.</Text>\n          \
                       </Paragraph>\n        </Summary>\n      </SceneProperties>\n";

/// A Scene Heading whose own runs and summary come in `parts` order
/// (`None` = the summary, `Some(text)` = an own `<Text>` run).
fn heading(parts: &[Option<&str>]) -> String {
    let body: String = parts
        .iter()
        .map(|p| match p {
            None => SUMMARY.to_string(),
            Some(t) => format!("      <Text>{t}</Text>\n"),
        })
        .collect();
    format!("    <Paragraph Type=\"Scene Heading\">\n{body}    </Paragraph>\n")
}

fn summary_doc(parts: &[Option<&str>]) -> String {
    doc(&[para("Action", &[Plain("Before.")]), heading(parts), para("Action", &[Plain("After.")])])
}

fn assert_summary_intact(out: &str) {
    assert_eq!(out.matches(SUMMARY).count(), 1, "summary bytes changed:\n{out}");
}

#[test]
fn scene_summaries_are_not_heading_text_and_survive_edits() {
    let cases: [&[Option<&str>]; 3] = [
        &[None, Some("INT. HOUSE - DAY")],
        &[Some("INT. HOUSE - DAY"), None],
        // Own runs split by the summary: rewritten into the first group.
        &[Some("INT. HOUSE"), None, Some(" - DAY")],
    ];
    for parts in cases {
        for raw in [summary_doc(parts), summary_doc(parts).replace('\n', "\r\n")] {
            let f = FdxFile::parse(raw.clone()).unwrap();
            let kt = kinds_and_texts(&f);
            assert_eq!(kt.len(), 3, "{kt:?}");
            assert_eq!(kt[1], ("Scene Heading".to_string(), "INT. HOUSE - DAY".to_string()));
            assert_eq!(f.write_edits(&BTreeMap::new()), raw);
            assert_eq!(f.write_script(&f.to_script()).unwrap(), raw);

            let out = f.write_edits(&edits(&[(1, "EXT. GARDEN - NIGHT")]));
            assert_summary_intact(&out.replace("\r\n", "\n"));
            let back = FdxFile::parse(out.clone()).unwrap();
            let mut want = kt.clone();
            want[1].1 = "EXT. GARDEN - NIGHT".into();
            assert_eq!(kinds_and_texts(&back), want);
            assert!(back.raw().contains("She &amp; he argue."));
            // Editing back restores the heading text, summary still intact.
            let again = back.write_edits(&edits(&[(1, "INT. HOUSE - DAY")]));
            assert_summary_intact(&again.replace("\r\n", "\n"));
            assert_eq!(FdxFile::parse(again).unwrap().paragraphs()[1].text(), "INT. HOUSE - DAY");
        }
    }
}

#[test]
fn heading_edit_outside_the_summary_leaves_the_rest_byte_identical() {
    for parts in [&[None, Some("INT. HOUSE - DAY")][..], &[Some("INT. HOUSE - DAY"), None][..]] {
        let raw = summary_doc(parts);
        let out = FdxFile::parse(raw.clone()).unwrap().write_edits(&edits(&[(1, "INT. HOUSE - NIGHT")]));
        assert_eq!(out.replace("INT. HOUSE - NIGHT", "INT. HOUSE - DAY"), raw);
    }
}

#[test]
fn legacy_reader_read_the_summary_as_heading_text() {
    let before = FdxFile::parse(summary_doc(&[None, Some("INT. HOUSE - DAY")])).unwrap();
    let after = FdxFile::parse(summary_doc(&[Some("INT. HOUSE - DAY"), None])).unwrap();
    let legacy = |f: &FdxFile| f.legacy_paragraphs().iter().map(|p| p.text()).collect::<Vec<_>>();
    assert_eq!(legacy(&before), ["Before.", "She & he argue.", "After."]);
    assert_eq!(legacy(&after), ["Before.", "INT. HOUSE - DAYShe & he argue.", "After."]);
}

#[test]
fn scanner_skips_comments_and_cdata() {
    let raw = doc(&[
        "    <!-- <Paragraph Type=\"Action\"><Text>commented</Text> -->\n".to_string(),
        para("Action", &[Plain("One.")]),
        "    <![CDATA[ </Paragraph> <Paragraph Type=\"Action\"> ]]>\n".to_string(),
        "    <Paragraph Type=\"Action\">\n      <!-- </Paragraph> -->\n      <Text>Two.</Text>\n    </Paragraph>\n"
            .to_string(),
        para("Action", &[Plain("Three.")]),
    ]);
    let f = FdxFile::parse(raw.clone()).unwrap();
    let texts: Vec<String> = f.paragraphs().iter().map(|p| p.text()).collect();
    assert_eq!(texts, ["One.", "Two.", "Three."]);
    assert_eq!(f.write_edits(&BTreeMap::new()), raw);
    let out = f.write_edits(&edits(&[(1, "Deux.")]));
    assert_eq!(out.replace("Deux.", "Two."), raw);
}

#[test]
fn unclosed_comment_or_cdata_is_a_parse_error() {
    for junk in ["    <!-- never closed\n", "    <![CDATA[ never closed\n"] {
        let raw = doc(&[para("Action", &[Plain("One.")]), junk.to_string(), para("Action", &[Plain("Two.")])]);
        assert_eq!(FdxFile::parse(raw).unwrap_err(), FdxError::UnclosedComment, "{junk}");
    }
}

#[test]
fn text_inside_a_comment_is_not_a_run() {
    let raw = doc(&[
        "    <Paragraph Type=\"Action\">\n      <!-- <Text>old</Text> -->\n      <Text>New.</Text>\n    </Paragraph>\n"
            .to_string(),
    ]);
    let f = FdxFile::parse(raw.clone()).unwrap();
    assert_eq!(f.paragraphs()[0].text(), "New.");
    let out = f.write_edits(&edits(&[(0, "Newer.")]));
    assert_eq!(out.replace("Newer.", "New."), raw, "the comment is kept as it was");
}

#[test]
fn a_heading_without_text_gets_its_text_after_scene_properties() {
    // Final Draft writes <SceneProperties> before the heading's <Text>.
    let raw =
        doc(&["    <Paragraph Type=\"Scene Heading\">\n      <SceneProperties Length=\"1/8\"/>\n    </Paragraph>\n"
            .to_string()]);
    let f = FdxFile::parse(raw.clone()).unwrap();
    let out = f.write_edits(&edits(&[(0, "INT. HOUSE - DAY")]));
    assert!(out.contains(
        "<SceneProperties Length=\"1/8\"/>\n      <Text>INT. HOUSE - DAY</Text>\n    </Paragraph>\n  </Content>"
    ));
}

#[test]
fn rewriting_split_own_runs_leaves_no_blank_line() {
    for raw in [summary_doc(&[Some("INT. HOUSE"), None, Some(" - DAY")])] {
        for raw in [raw.clone(), raw.replace('\n', "\r\n")] {
            let f = FdxFile::parse(raw.clone()).unwrap();
            let out = f.write_edits(&edits(&[(1, "EXT. GARDEN")]));
            assert!(!out.lines().any(|l| !l.is_empty() && l.trim().is_empty()), "{out}");
            assert!(
                out.contains("</SceneProperties>\r\n    </Paragraph>")
                    || out.contains("</SceneProperties>\n    </Paragraph>")
            );
            assert_eq!(FdxFile::parse(out).unwrap().paragraphs()[1].text(), "EXT. GARDEN");
        }
    }
}
