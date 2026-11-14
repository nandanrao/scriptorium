//! Operations on a paragraph's styled spans, addressed by UTF-8 byte offsets.
//!
//! Spans are kept *normalized*: no empty spans, and no two neighbours with the
//! same style. That makes "same content" mean "equal `Vec<Span>`", which is
//! what undo correctness tests rely on.

use std::ops::Range;

use crate::model::{Span, Style};

pub fn len(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.text.len()).sum()
}

pub fn text(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

/// Drop empty spans and merge neighbours that share a style.
pub fn normalize(spans: &mut Vec<Span>) {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for s in spans.drain(..) {
        if s.text.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.style == s.style => last.text.push_str(&s.text),
            _ => out.push(s),
        }
    }
    *spans = out;
}

/// The spans covering `range` (clamped), normalized.
pub fn slice(spans: &[Span], range: Range<usize>) -> Vec<Span> {
    let mut out = Vec::new();
    let mut pos = 0;
    for s in spans {
        let (a, b) = (pos, pos + s.text.len());
        pos = b;
        let (lo, hi) = (range.start.max(a), range.end.min(b));
        if lo < hi {
            out.push(Span { text: s.text[lo - a..hi - a].to_string(), style: s.style });
        }
    }
    normalize(&mut out);
    out
}

/// Replace `range` with `insert`.
pub fn splice(spans: &mut Vec<Span>, range: Range<usize>, insert: &[Span]) {
    let total = len(spans);
    let mut out = slice(spans, 0..range.start);
    out.extend_from_slice(insert);
    out.extend(slice(spans, range.end..total));
    normalize(&mut out);
    *spans = out;
}

/// Style that typing at `offset` should get: the character before it, else
/// the first character, else plain.
pub fn style_at(spans: &[Span], offset: usize) -> Style {
    let mut pos = 0;
    for s in spans {
        let end = pos + s.text.len();
        if offset > pos && offset <= end {
            return s.style;
        }
        pos = end;
    }
    spans.first().map(|s| s.style).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(text: &str, italic: bool) -> Span {
        Span { text: text.into(), style: Style { italic, ..Style::default() } }
    }

    #[test]
    fn slice_and_splice() {
        let spans = vec![sp("ab", false), sp("cd", true), sp("ef", false)];
        assert_eq!(slice(&spans, 1..5), vec![sp("b", false), sp("cd", true), sp("e", false)]);
        let mut s = spans.clone();
        splice(&mut s, 2..4, &[sp("X", false)]);
        assert_eq!(s, vec![sp("abXef", false)], "merges neighbours after removing the italic run");
        let mut s = spans.clone();
        splice(&mut s, 0..6, &[]);
        assert!(s.is_empty());
    }

    #[test]
    fn style_at_prefers_previous_char() {
        let spans = vec![sp("ab", false), sp("cd", true)];
        assert!(!style_at(&spans, 2).italic);
        assert!(style_at(&spans, 3).italic);
        assert!(!style_at(&spans, 0).italic);
        assert!(!style_at(&[], 0).italic);
    }
}
