//! Copying and pasting paragraphs.
//!
//! A copy from Scriptorium carries typed paragraphs ([`ClipboardMeta`]) next
//! to its plain text, so a paste back into Scriptorium keeps kinds and styles.
//! Other programs (Final Draft included) only see the plain text, and plain
//! text pasted in goes through [`classify`], a guess at each line's kind from
//! how screenplays are laid out. [`Document::paste_fragment`] puts typed
//! paragraphs in at a selection; [`for_clipboard`] decides what a paste is.

use serde::{Deserialize, Serialize};

use crate::doc::{DocError, Document, Group, NewPara, Pos, Selection};
use crate::model::ElementKind;
use crate::spans;

/// The current [`ClipboardMeta::scriptorium`] version.
pub const CLIPBOARD_VERSION: u32 = 1;

/// Longest line [`classify`] takes for a character name.
const MAX_CHARACTER_LEN: usize = 38;

/// What Scriptorium puts on the clipboard beside the plain text, as JSON:
/// `{"scriptorium":1,"paras":[{"kind":…,"spans":[…]}]}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardMeta {
    pub scriptorium: u32,
    pub paras: Vec<NewPara>,
}

impl ClipboardMeta {
    pub fn new(paras: Vec<NewPara>) -> Self {
        Self { scriptorium: CLIPBOARD_VERSION, paras }
    }

    /// The paragraphs, if this is a version this build reads and it still
    /// describes `text` (the clipboard's plain text). Anything else is
    /// ignored and the text is pasted as plain text.
    pub fn paras_for(self, text: &str) -> Option<Vec<NewPara>> {
        let same = plain_text(&self.paras) == text.replace("\r\n", "\n");
        (self.scriptorium == CLIPBOARD_VERSION && !self.paras.is_empty() && same).then_some(self.paras)
    }
}

/// The plain text of `paras`, one line each, as copied to other programs.
pub fn plain_text(paras: &[NewPara]) -> String {
    paras.iter().map(|p| spans::text(&p.spans)).collect::<Vec<_>>().join("\n")
}

/// What a paste puts in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pasted {
    /// Text that goes into the current paragraph as typing would (taking the
    /// style at the cursor).
    Text(String),
    /// Typed paragraphs for [`Document::paste_fragment`].
    Paras(Vec<NewPara>),
}

/// Decide what pasting `text` does, given the Scriptorium metadata that came
/// with it, if any. Metadata that matches the text wins. Otherwise text with
/// no line break goes in as it is; several lines are [`classify`]d, and if
/// that leaves one line it goes in unclassified.
/// Characters a script file can't hold ([`writable`]) are dropped first.
pub fn for_clipboard(text: &str, meta: Option<ClipboardMeta>) -> Pasted {
    let text = &writable(text);
    let meta = meta.map(|mut m| {
        for p in &mut m.paras {
            for s in &mut p.spans {
                s.text = writable(&s.text).replace(['\n', '\r'], "");
            }
            spans::normalize(&mut p.spans);
        }
        m
    });
    if let Some(paras) = meta.and_then(|m| m.paras_for(text)) {
        return Pasted::Paras(paras);
    }
    if !text.contains(['\n', '\r']) {
        return Pasted::Text(text.to_string());
    }
    let mut paras = classify(text);
    match paras.len() {
        0 => Pasted::Text(String::new()),
        1 => Pasted::Text(spans::text(&paras.remove(0).spans)),
        _ => Pasted::Paras(paras),
    }
}

/// `text` without the characters XML 1.0 (so a Final Draft file) can't hold:
/// C0 control characters other than tab and line breaks, U+FFFE and U+FFFF.
/// Line breaks stay (typing and pasting turn them into new paragraphs). Typed
/// and pasted text goes through this, so saving can't fail on them.
pub fn writable(text: &str) -> String {
    text.chars()
        .filter(|&c| !matches!(c, '\u{0}'..='\u{8}' | '\u{B}' | '\u{C}' | '\u{E}'..='\u{1F}' | '\u{FFFE}' | '\u{FFFF}'))
        .collect()
}

/// Guess each line's element kind from screenplay layout. Pure.
///
/// Blank lines are dropped (they only matter for the rules that look at the
/// line above or below). Per line, trimmed, the first rule that matches:
///
/// 1. Starts with `INT.`, `EXT.`, `EST.`, `INT./EXT.`, `INT/EXT.`, `EXT./INT.`,
///    `EXT/INT.` or `I/E` (alone, or followed by `.` or a space) → Scene Heading.
/// 2. `(…)` → Parenthetical.
/// 3. ALL CAPS and ending in `:` (`FADE IN:`, `CUT TO:`, `DISSOLVE TO:`), or
///    `FADE OUT.` / `FADE TO BLACK.` → Transition.
/// 4. Directly below a Character or Parenthetical (no blank line between) →
///    Dialogue: a speaker is never followed by another speaker.
/// 5. A name with an extension, `NAME (V.O.)`, `NAME (O.S.)`, `NAME (cont'd)`
///    (the name ALL CAPS, at most 38 characters) → Character.
/// 6. ALL CAPS, at most 38 characters, followed directly by a non-blank line,
///    and not inside an action run (the line directly above isn't Action) →
///    Character. So `BOOM!` on its own line before a blank stays Action, and
///    so does `BOOM!` in the middle of a paragraph of action lines; only a
///    caps line that opens a block and has text right under it reads as a
///    speaker.
/// 7. Otherwise Dialogue if the line directly above was Dialogue, else Action.
pub fn classify(text: &str) -> Vec<NewPara> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = text.split('\n').map(str::trim).collect();
    let mut out: Vec<NewPara> = vec![];
    // The kind of the line directly above, if it wasn't blank.
    let mut above: Option<ElementKind> = None;
    for (i, line) in lines.iter().enumerate() {
        if line.is_empty() {
            above = None;
            continue;
        }
        let next_is_text = lines.get(i + 1).is_some_and(|l| !l.is_empty());
        let kind = classify_line(line, above.as_ref(), next_is_text);
        above = Some(kind.clone());
        out.push(NewPara::plain(kind, *line));
    }
    out
}

/// Has letters and no lowercase ones.
fn all_caps(s: &str) -> bool {
    s.chars().any(char::is_alphabetic) && !s.chars().any(char::is_lowercase)
}

/// `NAME (EXT)` with an ALL-CAPS name and an extension that is ALL CAPS or
/// `cont'd` in any case.
fn name_with_extension(line: &str) -> bool {
    let Some(body) = line.strip_suffix(')') else { return false };
    let Some((name, ext)) = body.rsplit_once('(') else { return false };
    let name = name.trim_end();
    let ext_ok = all_caps(ext) || ["cont'd", "cont’d", "contd"].contains(&ext.to_lowercase().as_str());
    !name.is_empty() && name.chars().count() <= MAX_CHARACTER_LEN && all_caps(name) && ext_ok
}

fn classify_line(line: &str, above: Option<&ElementKind>, next_is_text: bool) -> ElementKind {
    use ElementKind::*;
    let heading = ["INT./EXT.", "INT/EXT.", "EXT./INT.", "EXT/INT.", "INT.", "EXT.", "EST.", "I/E.", "I/E "]
        .iter()
        .any(|p| line.starts_with(p))
        || line == "I/E";
    if heading {
        return SceneHeading;
    }
    if line.starts_with('(') && line.ends_with(')') {
        return Parenthetical;
    }
    if (all_caps(line) && line.ends_with(':')) || ["FADE OUT.", "FADE TO BLACK."].contains(&line) {
        return Transition;
    }
    if matches!(above, Some(Character | Parenthetical)) {
        return Dialogue;
    }
    if name_with_extension(line) {
        return Character;
    }
    let short = line.chars().count() <= MAX_CHARACTER_LEN;
    if all_caps(line) && short && next_is_text && !matches!(above, Some(Action)) {
        return Character;
    }
    match above {
        Some(Dialogue) => Dialogue,
        _ => Action,
    }
}

impl Document {
    /// The selected text as typed paragraphs, with styles (for copying).
    /// Empty for a collapsed selection.
    pub fn fragment(&self, sel: Selection) -> Vec<NewPara> {
        let sel = self.clamp_selection(sel);
        if sel.is_collapsed() {
            return vec![];
        }
        let (a, b) = self.ordered(sel);
        let (Some(ia), Some(ib)) = (self.index_of(a.para), self.index_of(b.para)) else { return vec![] };
        self.paragraphs()[ia..=ib]
            .iter()
            .enumerate()
            .map(|(k, p)| {
                let lo = if k == 0 { a.offset } else { 0 };
                let hi = if ia + k == ib { b.offset } else { spans::len(&p.spans) };
                NewPara { kind: p.kind.clone(), spans: spans::slice(&p.spans, lo.min(hi)..hi) }
            })
            .collect()
    }

    /// Replace the selection with `paras`, as one undo step. One paragraph:
    /// its spans go in at the cursor (its kind is ignored). Several: within
    /// one paragraph, it is split at the cursor ([`crate::Edit::split`], so
    /// the original id stays with the original text); over a selection across
    /// paragraphs, the selection is deleted without joining, so the head is
    /// the selection's first paragraph and the tail its last, each keeping
    /// its id. Then the first fragment's spans join the head, the last's are
    /// put before the tail, and the ones between are inserted with their
    /// kinds. The head and tail keep their kinds, except that one with no
    /// text of its own takes its fragment's. Returns a cursor after the
    /// pasted text. Refused ([`DocError::Fixed`]) where a split would be,
    /// e.g. several paragraphs into dual dialogue.
    pub fn paste_fragment(&mut self, sel: Selection, paras: &[NewPara]) -> Result<Selection, DocError> {
        let sel = self.clamp_selection(sel);
        self.edit(sel, Group::Other, |tx| {
            let cursor = match paras {
                [] => tx.delete_selection(sel)?,
                [only] => {
                    let pos = tx.delete_selection(sel)?;
                    tx.splice(pos.para, pos.offset..pos.offset, only.spans.clone())?;
                    Pos::new(pos.para, pos.offset + spans::len(&only.spans))
                }
                [first, middle @ .., last] => {
                    let text_len = |tx: &crate::Edit<'_>, id| tx.doc().paragraph(id).map_or(0, |p| p.text().len());
                    let (a, b) = tx.doc().ordered(sel);
                    let (head, tail) = if a.para != b.para {
                        tx.delete_between(a, b)?;
                        (a.para, b.para)
                    } else {
                        let pos = tx.delete_selection(sel)?;
                        let above = pos.offset == 0 && text_len(tx, pos.para) > 0;
                        let new_id = tx.split(pos)?;
                        if above { (new_id, pos.para) } else { (pos.para, new_id) }
                    };

                    let head_len = text_len(tx, head);
                    tx.splice(head, head_len..head_len, first.spans.clone())?;
                    if head_len == 0 {
                        tx.set_kind(head, first.kind.clone())?;
                    }
                    let below_head = tx.doc().index_of(head).ok_or(DocError::MissingParagraph(head))? + 1;
                    for (index, p) in (below_head..).zip(middle) {
                        tx.insert_para(index, p.clone())?;
                    }
                    if text_len(tx, tail) == 0 {
                        tx.set_kind(tail, last.kind.clone())?;
                    }
                    tx.splice(tail, 0..0, last.spans.clone())?;
                    Pos::new(tail, spans::len(&last.spans))
                }
            };
            let after = Selection::cursor(cursor);
            Ok((after, after))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ParaId, Paragraph, Script, Span, Style};
    use ElementKind::*;

    fn kinds_texts(paras: &[NewPara]) -> Vec<(ElementKind, String)> {
        paras.iter().map(|p| (p.kind.clone(), spans::text(&p.spans))).collect()
    }

    fn kt(k: ElementKind, t: &str) -> (ElementKind, String) {
        (k, t.to_string())
    }

    #[test]
    fn classify_table() {
        let text = "INT. KITCHEN - DAY\n\nMaria pours coffee.\n\nMARIA\n(quietly)\nYou lied.\nAbout everything.\n\nCUT TO:\n\nEXT. STREET - NIGHT\n\nTHE END\n";
        assert_eq!(
            kinds_texts(&classify(text)),
            vec![
                kt(SceneHeading, "INT. KITCHEN - DAY"),
                kt(Action, "Maria pours coffee."),
                kt(Character, "MARIA"),
                kt(Parenthetical, "(quietly)"),
                kt(Dialogue, "You lied."),
                kt(Dialogue, "About everything."),
                kt(Transition, "CUT TO:"),
                kt(SceneHeading, "EXT. STREET - NIGHT"),
                // ALL CAPS but followed by nothing: an action line.
                kt(Action, "THE END"),
            ]
        );
    }

    #[test]
    fn classify_headings_and_caps_lines() {
        for h in ["EST. CITY - DAY", "INT./EXT. CAR - NIGHT", "I/E CAR - NIGHT"] {
            assert_eq!(classify(h)[0].kind, SceneHeading, "{h}");
        }
        // An ALL-CAPS action line before a blank line stays Action.
        let got = classify("A LOUD BANG.\n\nEveryone ducks.");
        assert_eq!(kinds_texts(&got), vec![kt(Action, "A LOUD BANG."), kt(Action, "Everyone ducks.")]);
        // Too long for a name.
        let long = "THE WHOLE ROOM ERUPTS INTO CHAOS AND NOBODY CAN HEAR ANYTHING";
        assert_eq!(classify(&format!("{long}\nThen silence."))[0].kind, Action);
        // Indented, CRLF text from another program.
        let got = classify("          MARIA (V.O.)\r\n     Hello?\r\n");
        assert_eq!(kinds_texts(&got), vec![kt(Character, "MARIA (V.O.)"), kt(Dialogue, "Hello?")]);
        // Lowercase "int." is prose, not a heading; dialogue needs no blank line above.
        let got = classify("int. is short for interior.\n\nHello.");
        assert_eq!(kinds_texts(&got), vec![kt(Action, "int. is short for interior."), kt(Action, "Hello.")]);
    }

    fn kinds(text: &str) -> Vec<ElementKind> {
        classify(text).into_iter().map(|p| p.kind).collect()
    }

    #[test]
    fn classify_never_puts_a_character_under_a_character_or_parenthetical() {
        assert_eq!(kinds("MARIA\nHEY!\nStop."), vec![Character, Dialogue, Dialogue]);
        assert_eq!(kinds("MARIA\n(shouting)\nNO WAY\nReally."), vec![Character, Parenthetical, Dialogue, Dialogue]);
    }

    #[test]
    fn classify_caps_lines_ending_in_a_colon_are_transitions() {
        for t in ["FADE IN:", "CUT TO:", "DISSOLVE TO:", "SMASH CUT TO:", "MATCH CUT:"] {
            assert_eq!(kinds(&format!("{t}\nSomething happens.")), vec![Transition, Action], "{t}");
        }
        // Not caps: prose.
        assert_eq!(kinds("He said this:\n\nNothing."), vec![Action, Action]);
    }

    #[test]
    fn classify_fade_out_and_fade_to_black() {
        assert_eq!(kinds("She leaves.\n\nFADE OUT."), vec![Action, Transition]);
        assert_eq!(kinds("FADE TO BLACK.\nTHE END"), vec![Transition, Action]);
    }

    #[test]
    fn classify_slashed_headings() {
        for h in ["INT/EXT. CAR - DAY", "EXT/INT. CAR - DAY", "EXT./INT. CAR - DAY", "I/E. CAR - DAY", "I/E"] {
            assert_eq!(kinds(h), vec![SceneHeading], "{h}");
        }
        assert_eq!(kinds("I/EXAMPLE"), vec![Action]);
    }

    #[test]
    fn classify_names_with_extensions() {
        for c in ["MARIA (cont'd)", "MARIA (CONT'D)", "MARIA (V.O.)", "BOB (O.S.)", "DR. SMITH (O.S.)"] {
            assert_eq!(kinds(&format!("{c}\nHello?")), vec![Character, Dialogue], "{c}");
        }
        // Even inside an action run, an extension marks a speaker.
        assert_eq!(kinds("He listens.\nMARIA (V.O.)\nHello?"), vec![Action, Character, Dialogue]);
        // A lowercase name is not one.
        assert_eq!(kinds("maria (V.O.)\n\nHi."), vec![Action, Action]);
    }

    #[test]
    fn classify_boom_lines() {
        // Followed by a line and opening a block: a speaker.
        assert_eq!(kinds("BOOM!\nThe wall falls."), vec![Character, Dialogue]);
        // Before a blank line: action.
        assert_eq!(kinds("BOOM!\n\nThe wall falls."), vec![Action, Action]);
        // Inside an action run: action, and so is what follows.
        assert_eq!(kinds("He runs.\nBOOM!\nThe wall falls."), vec![Action, Action, Action]);
    }

    #[test]
    fn writable_drops_what_xml_cannot_hold() {
        assert_eq!(writable("a\u{0}b\u{7}c\td\ne\r\nf\u{1B}[0m\u{FFFF}"), "abc\td\ne\r\nf[0m");
        assert_eq!(writable("plain é ✓"), "plain é ✓");
        // A paste with control characters goes in without them, metadata or not.
        assert_eq!(for_clipboard("x\u{8}y", None), Pasted::Text("xy".into()));
        let meta = ClipboardMeta::new(vec![NewPara::plain(Character, "A\u{1}NA"), NewPara::plain(Dialogue, "Hi.")]);
        assert_eq!(
            for_clipboard("A\u{1}NA\nHi.", Some(meta)),
            Pasted::Paras(vec![NewPara::plain(Character, "ANA"), NewPara::plain(Dialogue, "Hi.")])
        );
    }

    #[test]
    fn for_clipboard_prefers_matching_metadata() {
        let paras = vec![NewPara::plain(Character, "ANA"), NewPara::plain(Dialogue, "Hi.")];
        let meta = ClipboardMeta::new(paras.clone());
        assert_eq!(for_clipboard("ANA\nHi.", Some(meta.clone())), Pasted::Paras(paras.clone()));
        assert_eq!(for_clipboard("ANA\r\nHi.", Some(meta.clone())), Pasted::Paras(paras));
        // Stale or foreign metadata is ignored.
        assert_eq!(for_clipboard("other", Some(meta.clone())), Pasted::Text("other".into()));
        let old = ClipboardMeta { scriptorium: 99, ..meta };
        assert_eq!(for_clipboard("ANA\nHi.", Some(old)), for_clipboard("ANA\nHi.", None));
        // One line, even after dropping blanks, goes in as text.
        assert_eq!(for_clipboard("  two  words ", None), Pasted::Text("  two  words ".into()));
        assert_eq!(for_clipboard("\nINT. HOUSE\n\n", None), Pasted::Text("INT. HOUSE".into()));
        assert_eq!(for_clipboard("\n\n", None), Pasted::Text(String::new()));
        assert!(matches!(for_clipboard("MARIA\nHi.", None), Pasted::Paras(p) if p.len() == 2));
    }

    #[test]
    fn metadata_json_shape() {
        let meta = ClipboardMeta::new(vec![NewPara::plain(Action, "x")]);
        let json = serde_json::to_string(&meta).unwrap();
        assert!(json.starts_with(r#"{"scriptorium":1,"paras":[{"kind":"Action","spans":"#), "{json}");
        assert_eq!(serde_json::from_str::<ClipboardMeta>(&json).unwrap(), meta);
    }

    // ---- paste_fragment ----------------------------------------------------

    fn doc() -> Document {
        Document::new(Script {
            paragraphs: vec![
                Paragraph::plain(ParaId(0), SceneHeading, "INT. HOUSE - DAY"),
                Paragraph::plain(ParaId(1), Action, "She waits by the door."),
                Paragraph::plain(ParaId(2), Action, "Nothing."),
            ],
            ..Script::default()
        })
    }

    fn state(d: &Document) -> Vec<(u64, ElementKind, String)> {
        d.paragraphs().iter().map(|p| (p.id.0, p.kind.clone(), p.text())).collect()
    }

    fn cur(para: u64, offset: usize) -> Selection {
        Selection::cursor(Pos::new(ParaId(para), offset))
    }

    fn three() -> Vec<NewPara> {
        vec![NewPara::plain(Action, "A"), NewPara::plain(Character, "BOB"), NewPara::plain(Dialogue, "Yes.")]
    }

    /// Paste, then check undo restores exactly and redo reproduces.
    fn paste(d: &mut Document, sel: Selection, paras: &[NewPara]) -> Selection {
        let before = state(d);
        let after_sel = d.paste_fragment(sel, paras).unwrap();
        let after = state(d);
        d.undo().unwrap();
        assert_eq!(state(d), before);
        d.redo().unwrap();
        assert_eq!(state(d), after);
        after_sel
    }

    #[test]
    fn one_paragraph_goes_in_at_the_cursor_with_its_styles() {
        let mut d = doc();
        let bold = Style { bold: true, ..Style::default() };
        let p = NewPara { kind: Dialogue, spans: vec![Span { text: "really ".into(), style: bold }] };
        let sel = paste(&mut d, cur(1, 4), &[p]);
        assert_eq!(d.paragraphs()[1].text(), "She really waits by the door.");
        assert_eq!(d.paragraphs()[1].kind, Action);
        assert!(d.paragraphs()[1].spans.iter().any(|s| s.style.bold && s.text == "really "));
        assert_eq!(sel, cur(1, 11));
    }

    #[test]
    fn several_in_the_middle_split_and_keep_the_original_id_on_the_head() {
        let mut d = doc();
        let sel = paste(&mut d, cur(1, 4), &three());
        assert_eq!(
            state(&d),
            vec![
                (0, SceneHeading, "INT. HOUSE - DAY".into()),
                (1, Action, "She A".into()),
                (4, Character, "BOB".into()),
                (3, Action, "Yes.waits by the door.".into()),
                (2, Action, "Nothing.".into()),
            ]
        );
        assert_eq!(sel, cur(3, 4));
    }

    #[test]
    fn several_at_the_start_keep_the_original_id_on_the_original_text() {
        let mut d = doc();
        let sel = paste(&mut d, cur(1, 0), &three());
        assert_eq!(
            state(&d),
            vec![
                (0, SceneHeading, "INT. HOUSE - DAY".into()),
                (3, Action, "A".into()),
                (4, Character, "BOB".into()),
                (1, Action, "Yes.She waits by the door.".into()),
                (2, Action, "Nothing.".into()),
            ]
        );
        assert_eq!(sel, cur(1, 4));
    }

    #[test]
    fn several_at_the_end_give_the_new_tail_the_last_kind() {
        let mut d = doc();
        let end = d.paragraphs()[2].text().len();
        let sel = paste(&mut d, cur(2, end), &three());
        assert_eq!(
            state(&d)[2..],
            [(2, Action, "Nothing.A".into()), (4, Character, "BOB".into()), (3, Dialogue, "Yes.".into())]
        );
        assert_eq!(sel, cur(3, 4));
    }

    #[test]
    fn into_an_empty_paragraph_both_ends_take_the_fragment_kinds() {
        let mut d = Document::new(Script {
            paragraphs: vec![
                Paragraph::plain(ParaId(0), Action, "Before."),
                Paragraph { id: ParaId(1), kind: Action, spans: vec![] },
            ],
            ..Script::default()
        });
        let paras = vec![NewPara::plain(Character, "ANA"), NewPara::plain(Dialogue, "Hi.")];
        paste(&mut d, cur(1, 0), &paras);
        assert_eq!(
            state(&d),
            vec![(0, Action, "Before.".into()), (1, Character, "ANA".into()), (2, Dialogue, "Hi.".into())]
        );
    }

    #[test]
    fn over_a_multi_paragraph_selection() {
        let mut d = doc();
        let sel = Selection { anchor: Pos::new(ParaId(2), 3), head: Pos::new(ParaId(0), 4) };
        let paras = vec![NewPara::plain(Action, "X"), NewPara::plain(Action, "Y")];
        paste(&mut d, sel, &paras);
        // The tail keeps its id and kind: the original id stays with the original text.
        assert_eq!(state(&d), vec![(0, SceneHeading, "INT.X".into()), (2, Action, "Yhing.".into())]);
        // The whole thing is one undo step.
        d.undo();
        assert_eq!(state(&d), state(&doc()));
    }

    #[test]
    fn dual_dialogue_takes_one_line_but_refuses_several() {
        let mut d = doc();
        let mut s = d.script().clone();
        s.fixed = [(ParaId(1), 0)].into();
        d = Document::new(s);
        let before = state(&d);
        assert_eq!(d.paste_fragment(cur(1, 4), &three()), Err(DocError::Fixed(ParaId(1))));
        assert_eq!(state(&d), before);
        assert!(d.log().is_empty());
        d.paste_fragment(cur(1, 4), &[NewPara::plain(Action, "only ")]).unwrap();
        assert_eq!(d.paragraphs()[1].text(), "She only waits by the door.");
    }

    #[test]
    fn fragment_copies_kinds_and_slices() {
        let d = doc();
        let sel = Selection { anchor: Pos::new(ParaId(0), 4), head: Pos::new(ParaId(2), 3) };
        let f = d.fragment(sel);
        assert_eq!(
            kinds_texts(&f),
            vec![kt(SceneHeading, " HOUSE - DAY"), kt(Action, "She waits by the door."), kt(Action, "Not")]
        );
        assert_eq!(plain_text(&f), d.text_in(sel));
        assert!(d.fragment(cur(1, 2)).is_empty());
        // Copy then paste back over the same selection: the text and the
        // surviving ends' ids and kinds are unchanged.
        let mut d2 = doc();
        d2.paste_fragment(sel, &f).unwrap();
        let t = |d: &Document| d.paragraphs().iter().map(|p| (p.kind.clone(), p.text())).collect::<Vec<_>>();
        assert_eq!(t(&d2), t(&doc()));
        assert_eq!(d2.paragraphs()[0].id, ParaId(0));
        assert_eq!(d2.paragraphs()[2].id, ParaId(2));
    }
}
