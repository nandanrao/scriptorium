//! Pure layout helpers for block suggestions on the review card: which lines
//! a block shows struck (BEFORE) or added (AFTER), how each sits on the card
//! by element kind, the card's heading, and where E puts the cursor.
//!
//! The script itself doesn't show the change (decided 2026-11-15): only a dot
//! on each covered line and the usual current-line highlight. The card is
//! where BEFORE and AFTER are read.

use scriptorium_core::diff::{Tag, opcodes};
use scriptorium_core::text::normalize;
use scriptorium_core::{ElementKind, ParaId};
use scriptorium_review::{Line, Shape};

use crate::layout::{self, Align};

/// How a line of a block reads on the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// In both BEFORE and the option, unchanged (an addition's anchor line).
    Same,
    /// In BEFORE only: struck through in red.
    Del,
    /// In the option only: green.
    Ins,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardLine {
    pub kind: ElementKind,
    pub text: String,
    pub side: Side,
    /// An added line that is a BEFORE line of the same stretch with only its
    /// kind changed: that line's kind, for an "Action → Character" tag.
    pub retyped_from: Option<ElementKind>,
}

fn kind_of(l: &Line) -> ElementKind {
    l.kind.clone().unwrap_or(ElementKind::Action)
}

/// The rows a block option shows: BEFORE and the option diffed line by line
/// (kind and text), lines in both shown once, each changed stretch as its
/// BEFORE lines (struck) followed by its new lines.
pub fn card_lines(before: &[Line], after: &[Line]) -> Vec<CardLine> {
    let key = |l: &Line| (kind_of(l), l.text.clone());
    let a: Vec<_> = before.iter().map(key).collect();
    let b: Vec<_> = after.iter().map(key).collect();
    let row = |l: &Line, side| CardLine { kind: kind_of(l), text: l.text.clone(), side, retyped_from: None };
    let mut rows = vec![];
    for op in opcodes(&a, &b) {
        if op.tag == Tag::Equal {
            rows.extend(after[op.j1..op.j2].iter().map(|l| row(l, Side::Same)));
            continue;
        }
        let gone = &before[op.i1..op.i2];
        rows.extend(gone.iter().map(|l| row(l, Side::Del)));
        rows.extend(after[op.j1..op.j2].iter().map(|l| {
            let same_text = |g: &&Line| normalize(&g.text) == normalize(&l.text) && kind_of(g) != kind_of(l);
            CardLine { retyped_from: gone.iter().find(same_text).map(kind_of), ..row(l, Side::Ins) }
        }));
    }
    rows
}

/// The index into `after` of the first line that isn't in BEFORE unchanged
/// (where E puts the cursor). None for a cut, or an option that only
/// removes lines (then the caller falls back to the first line left).
pub fn first_changed(before: &[Line], after: &[Line]) -> Option<usize> {
    let key = |l: &Line| (kind_of(l), l.text.clone());
    let a: Vec<_> = before.iter().map(key).collect();
    let b: Vec<_> = after.iter().map(key).collect();
    opcodes(&a, &b).into_iter().find(|op| op.tag != Tag::Equal && op.j2 > op.j1).map(|op| op.j1)
}

fn lines(n: usize) -> String {
    if n == 1 { "1 line".into() } else { format!("{n} lines") }
}

/// How the app names a line to a writer: its first few words, in quotes
/// ("“Grandma laughs.”", "“She walks to the…”"). Paragraph numbers mean
/// nothing to a writer.
pub fn quote(text: &str) -> String {
    const WORDS: usize = 5;
    const CHARS: usize = 32;
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return "an empty line".into();
    }
    let mut out = words[..words.len().min(WORDS)].join(" ");
    let mut cut = words.len() > WORDS;
    if out.chars().count() > CHARS {
        out = out.chars().take(CHARS).collect::<String>().trim_end().to_string();
        cut = true;
    }
    format!("\u{201c}{out}{}\u{201d}", if cut { "\u{2026}" } else { "" })
}

/// The heading for one option of a block (`Item::shape`): "Cut 4 lines",
/// "Add 2 lines after “Grandma laughs.”", "Change 2 lines", "Replace 4
/// lines with 2". `words` names a line ([`quote`] of its text).
pub fn heading(shape: Shape, words: impl Fn(ParaId) -> String) -> String {
    match shape {
        Shape::Line => "Change 1 line".into(),
        Shape::Cut { n } => format!("Cut {}", lines(n)),
        Shape::Add { after, n } => format!("Add {} after {}", lines(n), words(after)),
        Shape::Replace { n, m } if n == m => format!("Change {}", lines(n)),
        Shape::Replace { n, m } => format!("Replace {} with {m}", lines(n)),
    }
}

/// Where a line of kind `kind` sits on the card, as fractions of the card's
/// text width (the page's 1.5"–7.5" text column scaled down): its left edge,
/// its width, and its alignment. Zoom cancels out.
pub fn card_indent(kind: &ElementKind) -> (f32, f32, Align) {
    let g = layout::geometry(kind);
    let column = f32::from(layout::inch(6.0));
    let left = (f32::from(g.left) - f32::from(layout::inch(1.5))) / column;
    (left.clamp(0.0, 1.0), (f32::from(g.width) / column).clamp(0.0, 1.0), g.align)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ElementKind::*;

    fn t(kind: ElementKind, text: &str) -> Line {
        Line::typed(kind, text)
    }

    #[test]
    fn two_to_one_strikes_both_and_adds_one() {
        let before = [t(Character, "MARIA"), t(Dialogue, "I don't know."), t(Dialogue, "Really.")];
        let after = [t(Character, "MARIA"), t(Dialogue, "What?")];
        let rows = card_lines(&before, &after);
        let sides: Vec<Side> = rows.iter().map(|r| r.side).collect();
        assert_eq!(sides, [Side::Same, Side::Del, Side::Del, Side::Ins]);
        assert_eq!(rows[3].kind, Dialogue);
        assert_eq!(first_changed(&before, &after), Some(1));
    }

    #[test]
    fn a_cut_strikes_everything() {
        let before = [t(Action, "He waits."), t(Action, "Nothing.")];
        let rows = card_lines(&before, &[]);
        assert!(rows.iter().all(|r| r.side == Side::Del));
        assert_eq!(first_changed(&before, &[]), None);
    }

    #[test]
    fn an_addition_keeps_its_anchor_plain() {
        let before = [t(Action, "She sits.")];
        let after = [t(Action, "She sits."), t(Character, "ANA"), t(Dialogue, "Hello.")];
        let rows = card_lines(&before, &after);
        let sides: Vec<Side> = rows.iter().map(|r| r.side).collect();
        assert_eq!(sides, [Side::Same, Side::Ins, Side::Ins]);
        assert_eq!(first_changed(&before, &after), Some(1));
    }

    #[test]
    fn a_retype_is_a_change() {
        let before = [t(Action, "ANA")];
        let after = [t(Character, "ANA")];
        let rows = card_lines(&before, &after);
        assert_eq!(rows.iter().map(|r| r.side).collect::<Vec<_>>(), [Side::Del, Side::Ins]);
        assert_eq!(rows[1].retyped_from, Some(Action));
        assert_eq!(rows[0].retyped_from, None);
        // A new text isn't a retype.
        let rows = card_lines(&before, &[t(Character, "MARIA")]);
        assert_eq!(rows[1].retyped_from, None);
    }

    #[test]
    fn lines_are_named_by_their_first_words() {
        assert_eq!(quote("Grandma laughs."), "\u{201c}Grandma laughs.\u{201d}");
        assert_eq!(
            quote("  She walks to the window and looks out "),
            "\u{201c}She walks to the window\u{2026}\u{201d}"
        );
        assert_eq!(quote(""), "an empty line");
        let long = quote("Supercalifragilisticexpialidocious-ness everywhere");
        assert!(long.ends_with("\u{2026}\u{201d}") && long.chars().count() <= 35, "{long}");
    }

    #[test]
    fn indents_follow_the_page() {
        let (action, w, _) = card_indent(&Action);
        assert_eq!(action, 0.0);
        assert!((w - 1.0).abs() < 1e-4);
        let (dialogue, dw, _) = card_indent(&Dialogue);
        assert!((dialogue - 1.0 / 6.0).abs() < 1e-4, "{dialogue}");
        assert!((dw - 3.5 / 6.0).abs() < 1e-4);
        let (character, ..) = card_indent(&Character);
        assert!(character > dialogue);
        assert_eq!(card_indent(&Transition).2, Align::Right);
    }

    #[test]
    fn headings_name_the_shape() {
        let words = |id: ParaId| if id == ParaId(22) { quote("Grandma laughs.") } else { "?".into() };
        assert_eq!(heading(Shape::Cut { n: 4 }, words), "Cut 4 lines");
        assert_eq!(heading(Shape::Cut { n: 1 }, words), "Cut 1 line");
        assert_eq!(
            heading(Shape::Add { after: ParaId(22), n: 2 }, words),
            "Add 2 lines after \u{201c}Grandma laughs.\u{201d}"
        );
        assert_eq!(heading(Shape::Replace { n: 4, m: 2 }, words), "Replace 4 lines with 2");
        assert_eq!(heading(Shape::Replace { n: 2, m: 2 }, words), "Change 2 lines");
    }
}
