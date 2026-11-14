use std::collections::BTreeMap;
use std::path::PathBuf;

use scriptorium_core::outline::annotate;
use scriptorium_core::{ElementKind, Style};
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
        rewrite_paragraph(xml, "New & text."),
        "<Paragraph Type=\"Action\">\n      <SceneProperties Length=\"1\"/>\n      <Text>New &amp; text.</Text>\n    </Paragraph>"
    );
}

#[test]
fn rewrite_paragraph_fills_paragraphs_without_text() {
    assert!(rewrite_paragraph("<Paragraph Type=\"Action\">\n    </Paragraph>", "Hi").contains("<Text>Hi</Text>"));
    assert_eq!(
        rewrite_paragraph("<Paragraph Type=\"Action\"/>", "Hi"),
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
fn write_script_applies_text_edits_and_rejects_structure_changes() {
    let f = FdxFile::parse(sample()).unwrap();
    let mut s = f.to_script();
    s.paragraphs[3].spans[0].text = "I am going to town tomorrow.".into();
    let out = f.write_script(&s).unwrap();
    assert!(out.contains("<Text>I am going to town tomorrow.</Text>"));

    let mut removed = f.to_script();
    removed.paragraphs.pop();
    assert!(matches!(f.write_script(&removed), Err(FdxError::StructureChanged(_))));
    let mut retyped = f.to_script();
    retyped.paragraphs[6].kind = ElementKind::Dialogue;
    assert!(matches!(f.write_script(&retyped), Err(FdxError::StructureChanged(_))));
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
    assert_eq!(script.paragraphs.len(), 2921);
    assert_eq!(f.write_edits(&BTreeMap::new()), raw);
    assert_eq!(f.write_script(&script).unwrap(), raw);

    let styled = script.paragraphs.iter().position(|p| p.text().contains("Argentinian cards game")).unwrap();
    let new_text = script.paragraphs[styled].text().replace("cards game", "card game");
    let out = f.write_edits(&edits(&[(styled, &new_text)]));
    assert_eq!(FdxFile::parse(out.clone()).unwrap().to_script().paragraphs[styled].text(), new_text);
    assert_eq!(out.len(), raw.len() - 1);
}
