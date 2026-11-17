//! Parts of a Final Draft file that point into the script's text by
//! character offset, outside `<Content>`: script notes
//! (`<ScriptNotes><ScriptNote Range="10110,10324">`) and spell-check
//! ignored ranges (`<SpellCheckIgnoreLists><IgnoredRanges><Range Start=…
//! End=…/>`). An offset is only right for the text it was taken from.

use std::sync::LazyLock;

use regex::Regex;

use crate::content_span;

/// Elements whose children are character offsets into the script's text.
static OFFSET_LISTS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<(ScriptNotes|IgnoredRanges)(?:\s[^>]*)?>").unwrap());

/// `raw` with every `<ScriptNotes>` and `<IgnoredRanges>` outside the script
/// body emptied: the elements stay, their children go, the closing tag keeps
/// its line and indentation. Every other byte is kept. For a file whose text
/// is no longer the one the offsets were taken from (a placeholder version
/// made from another file's bytes): its notes would sit on the wrong words.
/// A file with neither element, or with both already empty, comes back
/// unchanged.
pub fn clear_text_offsets(raw: &str) -> String {
    let body = content_span(raw).ok();
    let eol = if raw.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = String::with_capacity(raw.len());
    let mut pos = 0;
    for c in OFFSET_LISTS_RE.captures_iter(raw) {
        let open = c.get(0).unwrap();
        if open.start() < pos || open.as_str().ends_with("/>") {
            continue;
        }
        if body.is_some_and(|(cs, ce)| (cs..ce).contains(&open.start())) {
            continue;
        }
        let close_tag = format!("</{}>", &c[1]);
        let Some(close) = raw[open.end()..].find(&close_tag).map(|i| open.end() + i) else { continue };
        let inner = &raw[open.end()..close];
        if inner.trim().is_empty() {
            continue;
        }
        let line_start = raw[..close].rfind('\n').map_or(0, |i| i + 1);
        let indent = &raw[line_start..close];
        let keep = if line_start > open.end() && indent.chars().all(|ch| ch == ' ' || ch == '\t') {
            format!("{eol}{indent}")
        } else {
            String::new()
        };
        out.push_str(&raw[pos..open.end()]);
        out.push_str(&keep);
        pos = close;
    }
    out.push_str(&raw[pos..]);
    out
}
