//! Reading and surgically rewriting Final Draft (`.fdx`) files.
//!
//! Only paragraphs inside the first `<Content>` (the script body) are read or
//! written. Writing replaces just the `<Text>` runs of changed paragraphs; every
//! other byte of the file is preserved, including each run's raw attributes.
//! The XML is never reserialized, so Final Draft sees exactly the file it wrote
//! plus the edited lines.
//!
//! Pure: takes and returns strings. Reading and writing files is the caller's job.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;
use scriptorium_core::diff::{Tag, opcodes};
use scriptorium_core::{ElementKind, ParaId, Paragraph, Script, Span, Style};

static PARA_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<Paragraph\b[^>]*?(?:/>|>.*?</Paragraph>)").unwrap());
static TEXT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<Text\b([^>]*?)(?:/>|>(.*?)</Text>)").unwrap());
static ENTITY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&(#x[0-9a-fA-F]+|#[0-9]+|amp|lt|gt|quot|apos);").unwrap());
static ELEMENT_SETTINGS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<ElementSettings\b[^>]*\bType="([^"]*)"[^>]*>(.*?)</ElementSettings>"#).unwrap()
});
static FONT_STYLE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<FontSpec\b[^>]*\bStyle="([^"]*)""#).unwrap());

/// Final Draft's defaults, used when a file doesn't carry its own element settings.
pub const DEFAULT_CAPS_TYPES: &[&str] =
    &["Scene Heading", "Character", "Transition", "Shot", "Cast List", "New Act", "End of Act"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FdxError {
    NotFinalDraft,
    UnclosedContent,
    /// `write_script` only supports text edits so far: same paragraphs, same order.
    StructureChanged(String),
}

impl fmt::Display for FdxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFinalDraft => write!(f, "not a Final Draft script (no <Content> section)"),
            Self::UnclosedContent => write!(f, "malformed Final Draft script (unclosed <Content>)"),
            Self::StructureChanged(why) => write!(f, "cannot write structural changes yet: {why}"),
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
    caps: HashSet<String>,
}

impl FdxFile {
    pub fn parse(raw: impl Into<String>) -> Result<Self, FdxError> {
        let raw = raw.into();
        let (cs, ce) = content_span(&raw)?;
        let body = &raw[cs..ce];
        let paragraphs = PARA_RE
            .find_iter(body)
            .map(|m| {
                let xml = m.as_str();
                let open_end = xml.find('>').unwrap_or(xml.len());
                FdxParagraph {
                    kind: attr(&xml[..open_end], "Type").unwrap_or_default(),
                    runs: parse_runs(xml),
                    span: cs + m.start()..cs + m.end(),
                }
            })
            .collect();
        let caps = caps_types(&raw);
        Ok(Self { raw, paragraphs, caps })
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

    /// Convert to the format-independent model. Paragraph ids are the paragraph
    /// indexes for now; stable ids across sessions come with the sidecar file.
    pub fn to_script(&self) -> Script {
        Script {
            paragraphs: self
                .paragraphs
                .iter()
                .enumerate()
                .map(|(i, p)| Paragraph {
                    id: ParaId(i as u64),
                    kind: ElementKind::from_name(&p.kind),
                    spans: p.runs.iter().map(|r| Span { text: r.text.clone(), style: r.style() }).collect(),
                })
                .collect(),
        }
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
            out.push_str(&rewrite_paragraph(&self.raw[p.span.clone()], new_text));
            pos = p.span.end;
        }
        out.push_str(&self.raw[pos..]);
        out
    }

    /// Write back a [`Script`] that came from [`FdxFile::to_script`] and has had
    /// paragraph text edited. Adding, removing, reordering or retyping
    /// paragraphs is not supported yet and returns an error.
    pub fn write_script(&self, script: &Script) -> Result<String, FdxError> {
        if script.paragraphs.len() != self.paragraphs.len() {
            return Err(FdxError::StructureChanged(format!(
                "paragraph count changed ({} → {})",
                self.paragraphs.len(),
                script.paragraphs.len()
            )));
        }
        let mut edits = BTreeMap::new();
        for (i, (p, orig)) in script.paragraphs.iter().zip(&self.paragraphs).enumerate() {
            if p.id != ParaId(i as u64) || p.kind.name() != orig.kind {
                return Err(FdxError::StructureChanged(format!("paragraph {i} was moved or retyped")));
            }
            edits.insert(i, p.text());
        }
        Ok(self.write_edits(&edits))
    }
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

/// Text runs of one paragraph's XML.
pub fn parse_runs(para_xml: &str) -> Vec<Run> {
    TEXT_RE
        .captures_iter(para_xml)
        .map(|c| {
            let whole = c.get(0).unwrap();
            Run {
                attrs: c[1].to_string(),
                text: c.get(2).map_or_else(String::new, |t| decode_entities(t.as_str())),
                range: whole.range(),
            }
        })
        .collect()
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

pub fn render_runs(runs: &[Run], indent: &str) -> String {
    runs.iter()
        .map(|r| format!("<Text{}>{}</Text>", r.attrs, escape_text(&r.text)))
        .collect::<Vec<_>>()
        .join(&format!("\n{indent}"))
}

/// Rewrite one paragraph's XML so its text becomes `new_text`.
pub fn rewrite_paragraph(xml: &str, new_text: &str) -> String {
    let runs = parse_runs(xml);
    let new_runs = restyle(&runs, new_text);
    if let (Some(first), Some(last)) = (runs.first(), runs.last()) {
        let (a, z) = (first.range.start, last.range.end);
        let indent = match xml[..a].rfind('\n') {
            Some(nl) => &xml[nl + 1..a],
            None => "      ",
        };
        return format!("{}{}{}", &xml[..a], render_runs(&new_runs, indent), &xml[z..]);
    }
    match xml.rfind("</Paragraph>") {
        Some(close) => {
            format!("{}  {}\n    {}", &xml[..close], render_runs(&new_runs, "      "), &xml[close..])
        }
        None => {
            // Self-closing <Paragraph …/>: expand it.
            let open = xml.trim_end().trim_end_matches("/>").trim_end();
            format!("{open}>\n      {}\n    </Paragraph>", render_runs(&new_runs, "      "))
        }
    }
}
