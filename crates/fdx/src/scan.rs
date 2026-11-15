//! Finding `<Paragraph>` elements by depth, without an XML parser.
//!
//! The reader needs each paragraph's exact byte span (to splice into it), so
//! it scans for paragraph tags itself. Comments and CDATA sections are opaque:
//! nothing inside them is a tag.

use std::ops::Range;

/// One `<Paragraph>` element found by [`scan_paragraphs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Element {
    /// The whole element, from `<Paragraph` to the end of its `</Paragraph>` (or `/>`).
    pub span: Range<usize>,
    /// Between the opening tag's `>` and the matching `</Paragraph>`; `None` if self-closing.
    pub inner: Option<Range<usize>>,
}

pub(crate) const CLOSE: &str = "</Paragraph>";

/// A paragraph tag found by [`next_tag`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// `<Paragraph…` (not `<ParagraphFoo`), at this position.
    Open(usize),
    /// `</Paragraph>`, at this position.
    Close(usize),
}

/// A comment or CDATA section opened at this byte offset and never closed
/// (within the scanned range).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Unclosed(pub usize);

/// If a comment or CDATA section starts at `lt`, where it ends (just past its
/// terminator), or `Err` if it is never closed before `end`.
fn opaque_end(raw: &str, lt: usize, end: usize) -> Option<Result<usize, Unclosed>> {
    let rest = &raw[lt..end];
    let (open, close) = if rest.starts_with("<!--") {
        ("<!--", "-->")
    } else if rest.starts_with("<![CDATA[") {
        ("<![CDATA[", "]]>")
    } else {
        return None;
    };
    let from = lt + open.len();
    Some(raw[from..end].find(close).map(|o| from + o + close.len()).ok_or(Unclosed(lt)))
}

/// The next `<Paragraph` opening tag or `</Paragraph>` in `raw[from..end]`,
/// skipping comments and CDATA sections.
fn next_tag(raw: &str, from: usize, end: usize) -> Result<Option<Mark>, Unclosed> {
    let mut at = from;
    while let Some(off) = raw[at..end].find('<') {
        let lt = at + off;
        if let Some(skip) = opaque_end(raw, lt, end) {
            at = skip?;
            continue;
        }
        let rest = &raw[lt..end];
        if rest.starts_with(CLOSE) {
            return Ok(Some(Mark::Close(lt)));
        } else if let Some(after) = rest.strip_prefix("<Paragraph") {
            match after.chars().next() {
                Some(c) if c.is_whitespace() || c == '>' || c == '/' => return Ok(Some(Mark::Open(lt))),
                None => return Ok(None),
                _ => at = lt + 1,
            }
        } else {
            at = lt + 1;
        }
    }
    Ok(None)
}

/// The outermost `<Paragraph>` elements in `raw[range]`, each matched with
/// its own `</Paragraph>` by depth, so a paragraph that contains paragraphs
/// (a `<DualDialogue>` wrapper, a scene summary) is one element. An opening
/// tag never closed within the range is skipped; so is a stray close. An
/// unclosed comment or CDATA section is an error: everything after it would
/// silently be lost.
pub(crate) fn scan_paragraphs(raw: &str, range: Range<usize>) -> Result<Vec<Element>, Unclosed> {
    let end = range.end;
    let mut out = Vec::new();
    let mut at = range.start;
    while let Some(tag) = next_tag(raw, at, end)? {
        let start = match tag {
            Mark::Open(o) => o,
            Mark::Close(c) => {
                at = c + CLOSE.len();
                continue;
            }
        };
        // The opening tag ends at the first `>`, as the old reader assumed.
        let Some(gt) = raw[start..end].find('>').map(|o| start + o) else { break };
        if raw[..gt].ends_with('/') {
            out.push(Element { span: start..gt + 1, inner: None });
            at = gt + 1;
            continue;
        }
        let body = gt + 1;
        let mut depth = 1;
        let mut pos = body;
        let mut close_at = None;
        while depth > 0 {
            match next_tag(raw, pos, end)? {
                Some(Mark::Open(o)) => {
                    let Some(ogt) = raw[o..end].find('>').map(|x| o + x) else { break };
                    if !raw[..ogt].ends_with('/') {
                        depth += 1;
                    }
                    pos = ogt + 1;
                }
                Some(Mark::Close(c)) => {
                    depth -= 1;
                    pos = c + CLOSE.len();
                    if depth == 0 {
                        close_at = Some(c);
                    }
                }
                None => break,
            }
        }
        match close_at {
            Some(c) => {
                out.push(Element { span: start..c + CLOSE.len(), inner: Some(body..c) });
                at = c + CLOSE.len();
            }
            None => at = body,
        }
    }
    Ok(out)
}

/// The parts of one paragraph's XML that are its own: everything except the
/// `<Paragraph>` elements nested inside it (scene summaries). In order; a
/// paragraph with nothing nested is one segment.
pub(crate) fn own_segments(xml: &str) -> Vec<Range<usize>> {
    let gt = xml.find('>').unwrap_or(xml.len());
    // Self-closing (or no tag at all): nothing nested; the scan finds nothing.
    let inner_end =
        if xml[..gt].ends_with('/') { gt } else { xml.rfind(CLOSE).filter(|&c| c > gt).unwrap_or(xml.len()) };
    let inner_start = (gt + 1).min(inner_end);
    let mut out = Vec::new();
    let mut from = 0;
    // A paragraph handed here was found by a successful scan, so it has no
    // unclosed comment; if it somehow does, treat it as having nothing nested.
    for nested in scan_paragraphs(xml, inner_start..inner_end).unwrap_or_default() {
        out.push(from..nested.span.start);
        from = nested.span.end;
    }
    out.push(from..xml.len());
    out
}

/// The comments and CDATA sections in `raw[range]`, in order. An unclosed one
/// runs to the end of the range.
pub(crate) fn opaque_ranges(raw: &str, range: Range<usize>) -> Vec<Range<usize>> {
    let end = range.end;
    let mut out = Vec::new();
    let mut at = range.start;
    while let Some(off) = raw[at..end].find('<') {
        let lt = at + off;
        match opaque_end(raw, lt, end) {
            Some(Ok(e)) => {
                out.push(lt..e);
                at = e;
            }
            Some(Err(_)) => {
                out.push(lt..end);
                break;
            }
            None => at = lt + 1,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(raw: &str) -> Vec<&str> {
        scan_paragraphs(raw, 0..raw.len()).unwrap().into_iter().map(|e| &raw[e.span]).collect()
    }

    #[test]
    fn matches_nested_paragraphs_by_depth() {
        let raw = "<Paragraph A><Paragraph B/><Paragraph C>x</Paragraph></Paragraph><Paragraph D>y</Paragraph>";
        assert_eq!(
            spans(raw),
            ["<Paragraph A><Paragraph B/><Paragraph C>x</Paragraph></Paragraph>", "<Paragraph D>y</Paragraph>"]
        );
        let els = scan_paragraphs(raw, 0..raw.len()).unwrap();
        assert_eq!(&raw[els[0].inner.clone().unwrap()], "<Paragraph B/><Paragraph C>x</Paragraph>");
        assert_eq!(els[1].inner, Some(raw.find('y').unwrap()..raw.find('y').unwrap() + 1));
    }

    #[test]
    fn self_closing_lookalikes_and_strays() {
        assert_eq!(spans("<Paragraph/>"), ["<Paragraph/>"]);
        assert_eq!(spans("<ParagraphSpacing/><Paragraph>a</Paragraph>"), ["<Paragraph>a</Paragraph>"]);
        assert_eq!(spans("</Paragraph><Paragraph>a</Paragraph>"), ["<Paragraph>a</Paragraph>"]);
        // Never closed: skipped, and its body is scanned on.
        assert_eq!(spans("<Paragraph><Paragraph>a</Paragraph>"), ["<Paragraph>a</Paragraph>"]);
    }

    #[test]
    fn comments_and_cdata_are_opaque() {
        let raw = "<!-- <Paragraph>c</Paragraph> --><Paragraph>a<![CDATA[</Paragraph>]]></Paragraph>";
        assert_eq!(spans(raw), ["<Paragraph>a<![CDATA[</Paragraph>]]></Paragraph>"]);
        assert_eq!(opaque_ranges(raw, 0..raw.len()), [0..33, 45..69]);
    }

    #[test]
    fn unclosed_comment_or_cdata_is_an_error() {
        for raw in ["<Paragraph>a</Paragraph><!-- <Paragraph>b</Paragraph>", "<Paragraph>a<![CDATA[</Paragraph>"] {
            assert!(scan_paragraphs(raw, 0..raw.len()).is_err(), "{raw}");
        }
        assert_eq!(opaque_ranges("x<!-- y", 0..7), vec![1..7]);
    }

    #[test]
    fn own_segments_skip_nested_paragraphs() {
        let xml = "<Paragraph>a<Paragraph>n</Paragraph>b</Paragraph>";
        let segs: Vec<&str> = own_segments(xml).into_iter().map(|r| &xml[r]).collect();
        assert_eq!(segs, ["<Paragraph>a", "b</Paragraph>"]);
        assert_eq!(own_segments("<Paragraph/>"), vec![0..12]);
    }
}
