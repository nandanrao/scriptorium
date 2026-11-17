//! Reading and surgically rewriting Final Draft (`.fdx`) files.
//!
//! Only paragraphs inside the first `<Content>` (the script body) are read or
//! written. Writing replaces just the `<Text>` runs of changed paragraphs; every
//! other byte of the file is preserved, including each run's raw attributes.
//! The XML is never reserialized, so Final Draft sees exactly the file it wrote
//! plus the edited lines.
//!
//! Paragraphs are found by a scanner that matches each `<Paragraph>` with its
//! own close by depth. A `<Paragraph>` whose body is a `<DualDialogue>` is a
//! *wrapper*: it gets no id of its own; its inner paragraphs are read as
//! ordinary paragraphs that take text edits only ([`Script::fixed`]).
//!
//! Any other `<Paragraph>` nested inside a paragraph (a Scene Heading's
//! `<SceneProperties><Summary><Paragraph>…`) belongs to that paragraph's
//! metadata: its `<Text>` runs are not part of the paragraph's text, and text
//! edits never touch its bytes. Comments and CDATA sections are skipped when
//! matching paragraphs.
//!
//! **Legacy reader.** Reader version 1 ([`FdxFile::legacy_paragraphs`]) is
//! kept only so sessions saved by it can be checked, for about two releases
//! (until v0.5.0). Once it is removed, a session saved by reader 1 on a file
//! whose `script.fixed` is non-empty (one with DualDialogue) is discarded
//! rather than trusted.
//!
//! **Writing** ([`FdxFile::write_script`]) compares paragraph ids with the
//! file's: ids `0..n` are the file's paragraphs, higher ids are new. It
//! splices removals, insertions, retypes and text edits into the original
//! bytes; see `write.rs` for the rules.
//!
//! Pure: takes and returns strings. Reading and writing files is the caller's job.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;
use scriptorium_core::diff::{Tag, opcodes};
use scriptorium_core::{ElementKind, ParaId, Paragraph, Script, Span, Style};

mod ranges;
mod scan;
mod write;

pub use ranges::clear_text_offsets;

use write::Plan;

use scan::{CLOSE, opaque_ranges, own_segments, scan_paragraphs};

/// The reader before DualDialogue support (reader version 1), kept only for
/// [`FdxFile::legacy_paragraphs`]: it is non-greedy, so a wrapper is read as a
/// phantom paragraph ending at the first inner `</Paragraph>`.
static LEGACY_PARA_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<Paragraph\b[^>]*?(?:/>|>.*?</Paragraph>)").unwrap());
static TEXT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<Text\b([^>]*?)(?:/>|>(.*?)</Text>)").unwrap());
static ENTITY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&(#x[0-9a-fA-F]+|#[0-9]+|amp|lt|gt|quot|apos);").unwrap());
static ELEMENT_SETTINGS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<ElementSettings\b[^>]*\bType="([^"]*)"[^>]*>(.*?)</ElementSettings>"#).unwrap()
});
static FONT_STYLE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<FontSpec\b[^>]*\bStyle="([^"]*)""#).unwrap());

/// Version of this crate's paragraph reader. Saved sessions record the
/// version that assigned their paragraph ids. 1: the regex reader that
/// misread `<DualDialogue>`; 2: the depth scanner.
pub const READER_VERSION: u32 = 2;

/// Final Draft's defaults, used when a file doesn't carry its own element settings.
pub const DEFAULT_CAPS_TYPES: &[&str] =
    &["Scene Heading", "Character", "Transition", "Shot", "Cast List", "New Act", "End of Act"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FdxError {
    NotFinalDraft,
    UnclosedContent,
    /// A comment or CDATA section in the script body is never closed; reading
    /// on would silently lose every paragraph after it.
    UnclosedComment,
    /// `write_script` can't write this change back. `para` is the line it's
    /// about, when there is one: the one moved or repeated, the line inside
    /// a `<DualDialogue>` block removed or retyped, or the first new line
    /// that would go inside one.
    StructureChanged {
        reason: StructureReason,
        para: Option<ParaId>,
    },
    /// New or edited text has a character XML 1.0 can't hold (a control
    /// character other than tab, or U+FFFE / U+FFFF), or a line break:
    /// a paragraph is one line. `para` is the paragraph.
    BadText {
        para: ParaId,
        ch: char,
    },
}

/// Whether `c` can't be written into a paragraph: not an XML 1.0 character,
/// or a line break (a stray `\r` counts like `\n`).
pub fn is_unwritable(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{A}'..='\u{1F}' | '\u{FFFE}' | '\u{FFFF}')
}

/// Why [`FdxError::StructureChanged`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructureReason {
    /// The file's paragraphs are out of their order.
    Moved,
    /// One of the file's paragraphs appears twice.
    Duplicated,
    /// A line added, removed or retyped inside a `<DualDialogue>` block.
    InDualDialogue,
}

impl fmt::Display for StructureReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Moved => "paragraphs were moved",
            Self::Duplicated => "a paragraph appears twice",
            Self::InDualDialogue => "lines can't be added, removed or retyped inside dual dialogue",
        })
    }
}

impl fmt::Display for FdxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFinalDraft => write!(f, "not a Final Draft script (no <Content> section)"),
            Self::UnclosedContent => write!(f, "malformed Final Draft script (unclosed <Content>)"),
            Self::UnclosedComment => {
                write!(f, "malformed Final Draft script (a comment or CDATA section is never closed)")
            }
            Self::StructureChanged { reason, .. } => write!(f, "cannot write this change to Final Draft: {reason}"),
            Self::BadText { para, ch } => write!(
                f,
                "cannot write paragraph {} to Final Draft: its text has {} (control characters and line \
                 breaks can't be saved in the file)",
                para.0,
                if matches!(ch, '\n' | '\r') { "a line break".to_string() } else { format!("U+{:04X}", *ch as u32) }
            ),
        }
    }
}

impl std::error::Error for FdxError {}

/// One `<Text>` run: its raw attribute string (kept verbatim) and decoded text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub attrs: String,
    pub text: String,
    /// Byte range of the whole `<Text …>…</Text>` element within the paragraph XML.
    range: Range<usize>,
}

impl Run {
    pub fn new(attrs: impl Into<String>, text: impl Into<String>) -> Self {
        Self { attrs: attrs.into(), text: text.into(), range: 0..0 }
    }

    /// The cross-format style this run's `Style="Bold+Italic…"` attribute expresses.
    pub fn style(&self) -> Style {
        let s = attr(&self.attrs, "Style").unwrap_or_default();
        let has = |w| s.split('+').any(|p| p == w);
        Style { bold: has("Bold"), italic: has("Italic"), underline: has("Underline") }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FdxParagraph {
    /// The raw `Type` attribute ("Action", "End of Act"…; "" if missing).
    pub kind: String,
    pub runs: Vec<Run>,
    /// Byte range of the `<Paragraph>` element within the file.
    span: Range<usize>,
    /// Index into [`FdxFile::wrappers`] when this paragraph is inside a
    /// `<DualDialogue>` wrapper.
    pub wrapper: Option<usize>,
}

impl FdxParagraph {
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

/// A parsed Final Draft file. Keeps the original bytes so it can write them back.
#[derive(Debug, Clone)]
pub struct FdxFile {
    raw: String,
    paragraphs: Vec<FdxParagraph>,
    /// Byte ranges of the `<Paragraph><DualDialogue>…</Paragraph>` wrappers.
    wrappers: Vec<Range<usize>>,
    caps: HashSet<String>,
    /// The file's line ending, used for every line the writer adds.
    eol: &'static str,
}

impl FdxFile {
    pub fn parse(raw: impl Into<String>) -> Result<Self, FdxError> {
        let raw = raw.into();
        let (cs, ce) = content_span(&raw)?;
        let mut paragraphs = Vec::new();
        let mut wrappers = Vec::new();
        let unclosed = |_| FdxError::UnclosedComment;
        for el in scan_paragraphs(&raw, cs..ce).map_err(unclosed)? {
            let inner = el.inner.clone();
            let is_wrapper = inner.as_ref().is_some_and(|r| raw[r.clone()].trim_start().starts_with("<DualDialogue"));
            match inner {
                Some(inner) if is_wrapper => {
                    let w = wrappers.len();
                    wrappers.push(el.span);
                    for member in scan_paragraphs(&raw, inner).map_err(unclosed)? {
                        paragraphs.push(read_paragraph(&raw, member.span, Some(w)));
                    }
                }
                _ => paragraphs.push(read_paragraph(&raw, el.span, None)),
            }
        }
        let caps = caps_types(&raw);
        let eol = if raw.contains("\r\n") { "\r\n" } else { "\n" };
        Ok(Self { raw, paragraphs, wrappers, caps, eol })
    }

    /// The paragraphs as reader version 1 (before DualDialogue support) read
    /// this file, so a session saved with that reader's ids can be checked
    /// against [`FdxFile::paragraphs`]. Not for anything else: its spans and
    /// kinds are wrong around `<DualDialogue>`.
    pub fn legacy_paragraphs(&self) -> Vec<FdxParagraph> {
        let (cs, ce) = content_span(&self.raw).expect("parsed file has a <Content>");
        LEGACY_PARA_RE
            .find_iter(&self.raw[cs..ce])
            .map(|m| {
                let span = cs + m.start()..cs + m.end();
                let mut p = read_paragraph(&self.raw, span.clone(), None);
                // Reader 1 read every `<Text>` in its (non-greedy) span.
                p.runs = runs_in(&self.raw[span.clone()], 0..span.len());
                p
            })
            .collect()
    }

    /// [`FdxFile::legacy_paragraphs`] as a [`Script`] (ids are indexes, as
    /// reader 1 assigned them; nothing is fixed).
    pub fn legacy_script(&self) -> Script {
        let paragraphs = model_paragraphs(&self.legacy_paragraphs());
        Script { id_floor: paragraphs.len() as u64, paragraphs, ..Script::default() }
    }

    /// Byte ranges of the `<DualDialogue>` wrapper paragraphs within the file,
    /// in order. [`FdxParagraph::wrapper`] indexes this.
    pub fn wrappers(&self) -> &[Range<usize>] {
        &self.wrappers
    }

    /// The original file contents.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Script-body paragraphs, in order. Indexes are what [`FdxFile::write_edits`] takes.
    pub fn paragraphs(&self) -> &[FdxParagraph] {
        &self.paragraphs
    }

    /// Element types this file displays in all capitals.
    pub fn caps_types(&self) -> &HashSet<String> {
        &self.caps
    }

    pub fn is_caps(&self, kind: &str) -> bool {
        self.caps.contains(kind)
    }

    /// The file's line ending: `"\r\n"` if it contains one, else `"\n"`.
    pub fn eol(&self) -> &'static str {
        self.eol
    }

    /// Convert to the format-independent model. Paragraph ids are the paragraph
    /// indexes, and [`Script::id_floor`] is their count, so an id the writer
    /// sees at or above it is a new paragraph. Paragraphs inside a
    /// `<DualDialogue>` wrapper are [`Script::fixed`], grouped by wrapper index.
    pub fn to_script(&self) -> Script {
        let fixed = self
            .paragraphs
            .iter()
            .enumerate()
            .filter_map(|(i, p)| p.wrapper.map(|w| (ParaId(i as u64), w as u32)))
            .collect();
        Script { paragraphs: model_paragraphs(&self.paragraphs), fixed, id_floor: self.paragraphs.len() as u64 }
    }

    /// Apply `{paragraph index: new text}` and return the new file. Paragraphs
    /// whose text is unchanged (or indexes out of range) are left byte-for-byte intact.
    pub fn write_edits(&self, edits: &BTreeMap<usize, String>) -> String {
        let mut out = String::with_capacity(self.raw.len());
        let mut pos = 0;
        for (&i, new_text) in edits {
            let Some(p) = self.paragraphs.get(i) else { continue };
            if *new_text == p.text() {
                continue;
            }
            out.push_str(&self.raw[pos..p.span.start]);
            out.push_str(&rewrite_paragraph(&self.raw[p.span.clone()], new_text, self.eol));
            pos = p.span.end;
        }
        out.push_str(&self.raw[pos..]);
        out
    }

    /// Write back a [`Script`] that came from [`FdxFile::to_script`] and has
    /// since been edited: paragraphs edited, added, removed or retyped. Every
    /// byte the changes don't touch is kept; with no changes the output is the
    /// original file. Refuses (`StructureChanged`) paragraphs that moved or
    /// repeat, and structural changes inside a `<DualDialogue>` block (text
    /// edits there are fine). Also refuses new or edited text with a control character or line break (`BadText`).
    pub fn write_script(&self, script: &Script) -> Result<String, FdxError> {
        Ok(self.splice(&self.plan(script)?))
    }

    /// Check that `script` can be written back and work out what changes,
    /// without rendering it: every refusal [`FdxFile::write_script`] makes
    /// comes from here.
    /// Whether `write_script` would accept `script`: the refusal it would make, if any.
    pub fn check_writable(&self, script: &Script) -> Result<(), FdxError> {
        self.plan(script).map(|_| ())
    }

    pub(crate) fn plan<'s>(&self, script: &'s Script) -> Result<Plan<'s>, FdxError> {
        write::plan(self, script)
    }

    /// Render a `Plan` made by `FdxFile::plan` on this file into the new file.
    pub(crate) fn splice(&self, plan: &Plan) -> String {
        write::splice(self, plan)
    }
}

fn model_paragraphs(paragraphs: &[FdxParagraph]) -> Vec<Paragraph> {
    paragraphs
        .iter()
        .enumerate()
        .map(|(i, p)| Paragraph {
            id: ParaId(i as u64),
            kind: ElementKind::from_name(&p.kind),
            spans: p.runs.iter().map(|r| Span { text: r.text.clone(), style: r.style() }).collect(),
        })
        .collect()
}

fn read_paragraph(raw: &str, span: Range<usize>, wrapper: Option<usize>) -> FdxParagraph {
    let xml = &raw[span.clone()];
    let open_end = xml.find('>').unwrap_or(xml.len());
    FdxParagraph { kind: attr(&xml[..open_end], "Type").unwrap_or_default(), runs: parse_runs(xml), span, wrapper }
}

fn content_span(raw: &str) -> Result<(usize, usize), FdxError> {
    let start = raw.find("<Content>").ok_or(FdxError::NotFinalDraft)?;
    let end = raw[start..].find("</Content>").ok_or(FdxError::UnclosedContent)? + start;
    Ok((start, end))
}

/// Value of attribute `name` in a raw tag/attribute string, entity-decoded.
fn attr(raw: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let mut from = 0;
    while let Some(off) = raw[from..].find(&needle) {
        let at = from + off;
        let boundary = raw[..at].chars().next_back().is_none_or(|c| !c.is_alphanumeric() && c != '_');
        let vstart = at + needle.len();
        if boundary {
            let vend = raw[vstart..].find('"').map_or(raw.len(), |e| vstart + e);
            return Some(decode_entities(&raw[vstart..vend]));
        }
        from = vstart;
    }
    None
}

pub fn decode_entities(s: &str) -> String {
    ENTITY_RE
        .replace_all(s, |c: &regex::Captures| {
            let e = &c[1];
            let decoded = match e {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
                _ => e[1..].parse().ok().and_then(char::from_u32),
            };
            decoded.map_or_else(|| c[0].to_string(), String::from)
        })
        .into_owned()
}

pub fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Text runs of one paragraph's XML: its own `<Text>` elements, not those of
/// paragraphs nested inside it (a Scene Heading's scene summary).
pub fn parse_runs(para_xml: &str) -> Vec<Run> {
    own_runs(para_xml).into_iter().flatten().collect()
}

/// [`parse_runs`], grouped by [`own_segments`]: runs in one group have no
/// nested paragraph between them.
fn own_runs(para_xml: &str) -> Vec<Vec<Run>> {
    own_segments(para_xml).into_iter().map(|seg| runs_in(para_xml, seg)).filter(|g| !g.is_empty()).collect()
}

/// Every `<Text>` run in `xml[seg]`, with ranges relative to `xml`. A
/// `<Text>` inside a comment or CDATA section is not a run.
fn runs_in(xml: &str, seg: Range<usize>) -> Vec<Run> {
    let hay = &xml[..seg.end];
    let opaque = opaque_ranges(xml, seg.clone());
    let mut out = Vec::new();
    let mut at = seg.start;
    while let Some(c) = TEXT_RE.captures_at(hay, at) {
        let whole = c.get(0).unwrap();
        if let Some(o) = opaque.iter().find(|o| o.contains(&whole.start())) {
            at = o.end;
            continue;
        }
        out.push(Run {
            attrs: c[1].to_string(),
            text: c.get(2).map_or_else(String::new, |t| decode_entities(t.as_str())),
            range: whole.start()..whole.end(),
        });
        at = whole.end();
    }
    out
}

/// Element types the file displays in all capitals, read from its
/// `<ElementSettings>` (FontSpec Style contains "AllCaps"), falling back to
/// Final Draft's defaults for types the file doesn't mention. Letter case typed
/// in these elements is invisible, so it doesn't count as a change.
pub fn caps_types(raw: &str) -> HashSet<String> {
    let mut found: HashMap<String, bool> = HashMap::new();
    for c in ELEMENT_SETTINGS_RE.captures_iter(raw) {
        if let Some(font) = FONT_STYLE_RE.captures(&c[2]) {
            found.insert(decode_entities(&c[1]), font[1].contains("AllCaps"));
        }
    }
    let mut caps: HashSet<String> =
        DEFAULT_CAPS_TYPES.iter().filter(|t| !found.contains_key(**t)).map(|t| t.to_string()).collect();
    caps.extend(found.into_iter().filter(|(_, is_caps)| *is_caps).map(|(t, _)| t));
    caps
}

/// Re-split `new_text` into runs that carry the original styling: unchanged
/// characters keep their run's attributes; inserted text takes the attributes
/// of what it replaces, or of the character before it.
pub fn restyle(runs: &[Run], new_text: &str) -> Vec<Run> {
    let default = runs.first().map_or("", |r| r.attrs.as_str());
    let old: Vec<(char, &str)> =
        runs.iter().flat_map(|r| r.text.chars().map(move |ch| (ch, r.attrs.as_str()))).collect();
    let attrs_at = |i: usize| old.get(i.min(old.len().saturating_sub(1))).map_or(default, |c| c.1);
    let old_chars: Vec<char> = old.iter().map(|c| c.0).collect();
    let new_chars: Vec<char> = new_text.chars().collect();

    let mut out: Vec<(char, &str)> = Vec::with_capacity(new_chars.len());
    for op in opcodes(&old_chars, &new_chars) {
        match op.tag {
            Tag::Equal => out.extend_from_slice(&old[op.i1..op.i2]),
            Tag::Delete => {}
            Tag::Replace | Tag::Insert => {
                let a = if op.tag == Tag::Replace { attrs_at(op.i1) } else { attrs_at(op.i1.saturating_sub(1)) };
                out.extend(new_chars[op.j1..op.j2].iter().map(|&ch| (ch, a)));
            }
        }
    }
    let mut merged: Vec<Run> = Vec::new();
    for (ch, a) in out {
        match merged.last_mut() {
            Some(last) if last.attrs == a => last.text.push(ch),
            _ => merged.push(Run::new(a, ch)),
        }
    }
    if merged.is_empty() {
        merged.push(Run::new(default, ""));
    }
    merged
}

/// Runs as `<Text>` elements, one per line: `eol` then `indent` between them.
pub fn render_runs(runs: &[Run], indent: &str, eol: &str) -> String {
    runs.iter()
        .map(|r| format!("<Text{}>{}</Text>", r.attrs, escape_text(&r.text)))
        .collect::<Vec<_>>()
        .join(&format!("{eol}{indent}"))
}

/// Rewrite one paragraph's XML so its text becomes `new_text`. Lines it adds
/// end with `eol` (the file's line ending, [`FdxFile::eol`]).
///
/// Only the paragraph's own `<Text>` runs are replaced; a nested paragraph
/// (scene summary) is never touched. Usually the own runs are contiguous and
/// the new runs replace them. If a nested paragraph sits between own runs, the
/// new runs replace the first contiguous group and the later groups' `<Text>`
/// elements are removed (with the indentation and line break before them,
/// when those are pure whitespace; everything nested stays), so the result
/// reads as `new_text`. A paragraph with no runs gets them as its last
/// child, after any `<SceneProperties>`, as Final Draft orders them.
pub fn rewrite_paragraph(xml: &str, new_text: &str, eol: &str) -> String {
    let groups = own_runs(xml);
    let runs: Vec<Run> = groups.iter().flatten().cloned().collect();
    let new_runs = restyle(&runs, new_text);
    if let Some((first_group, rest)) = groups.split_first() {
        let a = first_group[0].range.start;
        let indent = match xml[..a].rfind('\n') {
            Some(nl) => &xml[nl + 1..a],
            None => "      ",
        };
        let mut out = String::with_capacity(xml.len() + new_text.len());
        out.push_str(&xml[..a]);
        out.push_str(&render_runs(&new_runs, indent, eol));
        let mut pos = first_group[first_group.len() - 1].range.end;
        for g in rest {
            let (ga, gz) = (g[0].range.start, g[g.len() - 1].range.end);
            out.push_str(&xml[pos..write::cut_start(xml, ga).max(pos)]);
            pos = gz;
        }
        out.push_str(&xml[pos..]);
        return out;
    }
    match xml.rfind(CLOSE) {
        Some(close) => {
            format!("{}  {}{eol}    {}", &xml[..close], render_runs(&new_runs, "      ", eol), &xml[close..])
        }
        None => {
            // Self-closing <Paragraph …/>: expand it.
            let open = xml.trim_end().trim_end_matches("/>").trim_end();
            format!("{open}>{eol}      {}{eol}    </Paragraph>", render_runs(&new_runs, "      ", eol))
        }
    }
}
