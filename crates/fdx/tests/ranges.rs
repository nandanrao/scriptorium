//! `clear_text_offsets`: script notes and spell-check ranges emptied, every
//! other byte kept.

use scriptorium_fdx::{FdxFile, clear_text_offsets};

const FILE: &str = r#"<?xml version="1.0"?>
<FinalDraft Version="5">
  <Content>
    <Paragraph Type="Action">
      <Text>&lt;ScriptNotes&gt; is text here.</Text>
    </Paragraph>
  </Content>
  <SpellCheckIgnoreLists>
    <IgnoredRanges>
      <Range End="7" Start="1"/>
      <Range End="12" Start="9"/>
    </IgnoredRanges>
    <IgnoredWords>
      <Word>LUCIA</Word>
    </IgnoredWords>
  </SpellCheckIgnoreLists>
  <ScriptNotes>
    <ScriptNote Id="1" Range="1,4">
      <Paragraph>
        <Text>A note.</Text>
      </Paragraph>
    </ScriptNote>
  </ScriptNotes>
</FinalDraft>
"#;

const CLEARED: &str = r#"<?xml version="1.0"?>
<FinalDraft Version="5">
  <Content>
    <Paragraph Type="Action">
      <Text>&lt;ScriptNotes&gt; is text here.</Text>
    </Paragraph>
  </Content>
  <SpellCheckIgnoreLists>
    <IgnoredRanges>
    </IgnoredRanges>
    <IgnoredWords>
      <Word>LUCIA</Word>
    </IgnoredWords>
  </SpellCheckIgnoreLists>
  <ScriptNotes>
  </ScriptNotes>
</FinalDraft>
"#;

#[test]
fn notes_and_ranges_are_emptied_and_nothing_else_changes() {
    assert_eq!(clear_text_offsets(FILE), CLEARED);
    let crlf = |s: &str| s.replace('\n', "\r\n");
    assert_eq!(clear_text_offsets(&crlf(FILE)), crlf(CLEARED));
    // Clearing again changes nothing; the script body is untouched.
    assert_eq!(clear_text_offsets(CLEARED), CLEARED);
    let (a, b) = (FdxFile::parse(FILE).unwrap(), FdxFile::parse(CLEARED).unwrap());
    assert_eq!(a.to_script(), b.to_script());
}

#[test]
fn files_without_notes_or_with_empty_ones_come_back_unchanged() {
    for raw in [
        "<FinalDraft><Content></Content></FinalDraft>",
        "<FinalDraft><Content></Content><ScriptNotes/><SpellCheckIgnoreLists><IgnoredRanges /></SpellCheckIgnoreLists></FinalDraft>",
        "<FinalDraft><Content></Content><ScriptNotes></ScriptNotes></FinalDraft>",
    ] {
        assert_eq!(clear_text_offsets(raw), raw);
    }
    // On one line: the children go, nothing else.
    assert_eq!(
        clear_text_offsets(
            "<FinalDraft><Content></Content><ScriptNotes><ScriptNote Range=\"1,2\"/></ScriptNotes></FinalDraft>"
        ),
        "<FinalDraft><Content></Content><ScriptNotes></ScriptNotes></FinalDraft>"
    );
}
