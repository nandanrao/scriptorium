//! [`FdxFile::write_script`]: splicing an edited [`Script`] into the original bytes.
//!
//! The rules (docs/plans/block-suggestions.md, A2):
//!
//! - **Ids.** The file's paragraphs have ids `0..n`; ids `≥ n` are new. The
//!   surviving originals must appear in increasing order, each once (else
//!   "moved" / "appears twice").
//! - **Two steps.** [`plan`] works out and checks the change (every refusal
//!   happens there, so a caller can ask whether an edit is writable without
//!   rendering it); [`splice`] renders it and can't fail.
//! - **Removing** cuts the paragraph's element together with the spaces/tabs
//!   and one line break before it, when those are pure whitespace; otherwise
//!   exactly the element.
//! - **Inserting** after the nearest surviving original before the new
//!   paragraphs (its *anchor*): `{eol}{indent}{xml}` at the anchor's end (the
//!   end of its `<DualDialogue>` wrapper when the anchor is the wrapper's last
//!   line). With no anchor: `{xml}{eol}{indent}` at the start of the first
//!   surviving original (or of its wrapper); with no survivor at all, right
//!   after `<Content>`.
//! - **Splices** are applied in order of their start; at an equal start a
//!   zero-width insert comes before a cut or rewrite. They never overlap.
//! - **New XML** uses the file's most common layout attributes for that
//!   paragraph type, the most common font of that type's first runs, and the
//!   file's line ending.
//! - **Retype** rewrites `Type` in place and swaps the layout attributes for
//!   the new type's; leaving Scene Heading drops `Number` and the
//!   `<SceneProperties>` child.
//! - **DualDialogue.** Text edits inside a wrapper are fine; removing,
//!   retyping or inserting among its lines is refused (the editor refuses
//!   them first; this is the backstop).

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;
use scriptorium_core::{ElementKind, ParaId, Paragraph, Script};

use crate::{FdxError, FdxFile, StructureReason, content_span, escape_text, rewrite_paragraph};

/// Opening-tag attributes a new paragraph copies from its template, and that
/// a retype swaps for the new type's: they decide the paragraph's layout.
const LAYOUT_ATTRS: &[&str] =
    &["Type", "Alignment", "FirstIndent", "LeftIndent", "RightIndent", "Spacing", "SpaceBefore", "Leading"];

/// One attribute with the whitespace before it: ` Name="value"`.
static ATTR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\s+([A-Za-z_:][-\w.:]*)\s*=\s*(?:"[^"]*"|'[^']*')"#).unwrap());

fn refused(reason: StructureReason, para: ParaId) -> FdxError {
    FdxError::StructureChanged { reason, para: Some(para) }
}

/// What writing a [`Script`] back changes in the file, checked to be
/// writable: [`plan`] refuses everything [`splice`] can't write. Paragraphs
/// are file indexes (`0..n`) or the script's new paragraphs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Plan<'s> {
    /// The file's paragraphs that are still there, with their new version, in order.
    pub survivors: Vec<(usize, &'s Paragraph)>,
    /// The file's paragraphs that are gone, in order.
    pub removed: Vec<usize>,
    /// New paragraphs, grouped by anchor: the surviving original before
    /// them (`None`: before every survivor).
    pub inserts: Vec<(Option<usize>, Vec<&'s Paragraph>)>,
    /// The surviving paragraphs whose type changed, in order.
    pub retypes: Vec<usize>,
}

pub(crate) fn plan<'s>(f: &FdxFile, script: &'s Script) -> Result<Plan<'s>, FdxError> {
    let n = f.paragraphs.len();
    let mut last: Option<usize> = None;
    let mut seen = vec![false; n];
    let mut survivors: Vec<(usize, &Paragraph)> = Vec::new();
    let mut inserts: Vec<(Option<usize>, Vec<&Paragraph>)> = Vec::new();
    for p in &script.paragraphs {
        match usize::try_from(p.id.0).ok().filter(|&i| i < n) {
            Some(i) => {
                if seen[i] {
                    return Err(refused(StructureReason::Duplicated, p.id));
                }
                if last.is_some_and(|l| i < l) {
                    return Err(refused(StructureReason::Moved, p.id));
                }
                seen[i] = true;
                last = Some(i);
                survivors.push((i, p));
            }
            None => match inserts.last_mut() {
                Some((anchor, group)) if *anchor == last => group.push(p),
                _ => inserts.push((last, vec![p])),
            },
        }
    }
    // New or edited text must be writable as XML 1.0 on one line.
    let fresh = inserts.iter().flat_map(|(_, g)| g.iter().copied());
    let edited = survivors.iter().filter(|(i, p)| p.text() != f.paragraphs[*i].text()).map(|&(_, p)| p);
    for p in fresh.chain(edited) {
        if let Some(ch) = p.text().chars().find(|&c| crate::is_unwritable(c)) {
            return Err(FdxError::BadText { para: p.id, ch });
        }
    }
    let in_wrapper = |i: usize| f.paragraphs[i].wrapper.is_some();
    let removed: Vec<usize> = (0..n).filter(|&i| !seen[i]).collect();
    if let Some(&i) = removed.iter().find(|&&i| in_wrapper(i)) {
        return Err(refused(StructureReason::InDualDialogue, ParaId(i as u64)));
    }
    let retypes: Vec<usize> =
        survivors.iter().filter(|(i, p)| p.kind.name() != f.paragraphs[*i].kind).map(|&(i, _)| i).collect();
    if let Some(&i) = retypes.iter().find(|&&i| in_wrapper(i)) {
        return Err(refused(StructureReason::InDualDialogue, ParaId(i as u64)));
    }
    for (anchor, group) in &inserts {
        // Between two lines of one wrapper: there's nowhere outside it to go.
        if let Some(i) = *anchor
            && let Some(w) = f.paragraphs[i].wrapper
            && f.paragraphs.get(i + 1).is_some_and(|next| next.wrapper == Some(w))
        {
            return Err(refused(StructureReason::InDualDialogue, group[0].id));
        }
    }
    Ok(Plan { survivors, removed, inserts, retypes })
}

pub(crate) fn splice(f: &FdxFile, plan: &Plan) -> String {
    let mut splices: Vec<(Range<usize>, String)> = Vec::new();
    for &i in &plan.removed {
        let orig = &f.paragraphs[i];
        splices.push((cut_start(&f.raw, orig.span.start)..orig.span.end, String::new()));
    }
    let templates = Templates::new(f);
    for &(i, p) in &plan.survivors {
        let orig = &f.paragraphs[i];
        let new_text = p.text();
        let retyped = plan.retypes.binary_search(&i).is_ok();
        if new_text == orig.text() && !retyped {
            continue;
        }
        let mut xml = f.raw[orig.span.clone()].to_string();
        if new_text != orig.text() {
            xml = rewrite_paragraph(&xml, &new_text, f.eol);
        }
        if retyped {
            xml = retype(&xml, &orig.kind, &p.kind, &templates);
        }
        splices.push((orig.span.clone(), xml));
    }
    for (anchor, group) in &plan.inserts {
        splices.push(place(f, *anchor, group, &plan.survivors, &templates));
    }

    // Stable: equal starts keep their push order after the zero-width rule.
    splices.sort_by_key(|(r, _)| (r.start, !r.is_empty()));
    let mut out = String::with_capacity(f.raw.len());
    let mut pos = 0;
    for (range, bytes) in splices {
        assert!(range.start >= pos, "fdx writer: overlapping splices at {}", range.start);
        out.push_str(&f.raw[pos..range.start]);
        out.push_str(&bytes);
        pos = range.end;
    }
    out.push_str(&f.raw[pos..]);
    out
}

/// Where and what to insert for the new paragraphs `group` after `anchor`
/// ([`plan`] has checked the place is outside any wrapper).
fn place(
    f: &FdxFile,
    anchor: Option<usize>,
    group: &[&Paragraph],
    survivors: &[(usize, &Paragraph)],
    templates: &Templates,
) -> (Range<usize>, String) {
    // The element a paragraph's position is taken from: its wrapper, if any.
    let element = |i: usize| match f.paragraphs[i].wrapper {
        Some(w) => f.wrappers[w].clone(),
        None => f.paragraphs[i].span.clone(),
    };
    let eol = f.eol;
    let mut bytes = String::new();
    let at = match anchor {
        Some(i) => {
            let el = element(i);
            let indent = indent_before(&f.raw, el.start);
            for p in group {
                bytes.push_str(&format!("{eol}{indent}{}", templates.xml(p, indent, eol)));
            }
            el.end
        }
        None => match survivors.first() {
            Some(&(k, _)) => {
                let el = element(k);
                let indent = indent_before(&f.raw, el.start);
                for p in group {
                    bytes.push_str(&format!("{}{eol}{indent}", templates.xml(p, indent, eol)));
                }
                el.start
            }
            None => {
                let indent = f.paragraphs.first().map_or("    ", |p| indent_before(&f.raw, element_start(f, p)));
                let indent = if indent.is_empty() { "    " } else { indent };
                for p in group {
                    bytes.push_str(&format!("{eol}{indent}{}", templates.xml(p, indent, eol)));
                }
                let (cs, _) = content_span(&f.raw).expect("parsed file has a <Content>");
                cs + "<Content>".len()
            }
        },
    };
    (at..at, bytes)
}

fn element_start(f: &FdxFile, p: &crate::FdxParagraph) -> usize {
    p.wrapper.map_or(p.span.start, |w| f.wrappers[w].start)
}

/// The spaces and tabs directly before `pos`.
fn indent_before(raw: &str, pos: usize) -> &str {
    let start = raw[..pos].trim_end_matches([' ', '\t']).len();
    &raw[start..pos]
}

/// Where a cut of an element starting at `start` begins: before the spaces
/// and tabs and one line break in front of it, if those are there; else at
/// `start` itself.
pub(crate) fn cut_start(raw: &str, start: usize) -> usize {
    let ws = raw[..start].trim_end_matches([' ', '\t']).len();
    if raw[..ws].ends_with("\r\n") {
        ws - 2
    } else if raw[..ws].ends_with('\n') {
        ws - 1
    } else {
        start
    }
}

/// Attributes of an opening tag: `(name, range)`, the range covering the
/// whitespace before the attribute and the attribute.
fn tag_attrs(tag: &str) -> Vec<(&str, Range<usize>)> {
    ATTR_RE.captures_iter(tag).map(|c| (c.get(1).unwrap().as_str(), c.get(0).unwrap().range())).collect()
}

/// The opening tag of an element's XML: up to and excluding the first `>`.
fn open_tag(xml: &str) -> &str {
    &xml[..xml.find('>').unwrap_or(xml.len())]
}

/// Per paragraph type, what new paragraphs of that type look like in this file.
struct Templates {
    /// Type name → layout attributes (each ` Name="value"`, raw), file order.
    layout: BTreeMap<String, Vec<String>>,
    /// Type name → ` Font="…" Size="…"` for its runs.
    font: BTreeMap<String, String>,
}

/// The most common value, ties going to the first seen.
fn most_common<T: PartialEq + Clone>(items: impl IntoIterator<Item = T>) -> Option<T> {
    let mut counts: Vec<(T, usize)> = Vec::new();
    for item in items {
        match counts.iter_mut().find(|(v, _)| *v == item) {
            Some((_, c)) => *c += 1,
            None => counts.push((item, 1)),
        }
    }
    let mut best: Option<(T, usize)> = None;
    for (v, c) in counts {
        if best.as_ref().is_none_or(|(_, b)| c > *b) {
            best = Some((v, c));
        }
    }
    best.map(|(v, _)| v)
}

impl Templates {
    fn new(f: &FdxFile) -> Self {
        let mut by_type: BTreeMap<&str, Vec<&crate::FdxParagraph>> = BTreeMap::new();
        for p in f.paragraphs.iter().filter(|p| p.wrapper.is_none()) {
            by_type.entry(p.kind.as_str()).or_default().push(p);
        }
        let mut layout = BTreeMap::new();
        let mut font = BTreeMap::new();
        for (kind, paras) in by_type {
            // Compared as a set (sorted), written in the first seen order.
            let sets = paras.iter().map(|p| {
                let tag = open_tag(&f.raw[p.span.clone()]);
                let attrs: Vec<String> = tag_attrs(tag)
                    .into_iter()
                    .filter(|(name, _)| LAYOUT_ATTRS.contains(name))
                    .map(|(_, r)| format!(" {}", tag[r].trim_start()))
                    .collect();
                let mut key = attrs.clone();
                key.sort();
                (key, attrs)
            });
            let keys: Vec<(Vec<String>, Vec<String>)> = sets.collect();
            if let Some(key) = most_common(keys.iter().map(|(k, _)| k.clone())) {
                let attrs = keys.iter().find(|(k, _)| *k == key).map(|(_, a)| a.clone()).unwrap_or_default();
                layout.insert(kind.to_string(), attrs);
            }
            let fonts = paras.iter().filter_map(|p| p.runs.first()).map(|r| {
                let raw_attr = |name: &str| {
                    tag_attrs(&r.attrs)
                        .into_iter()
                        .find(|(n, _)| *n == name)
                        .map(|(_, range)| format!(" {}", r.attrs[range].trim_start()))
                        .unwrap_or_default()
                };
                format!("{}{}", raw_attr("Font"), raw_attr("Size"))
            });
            if let Some(fs) = most_common(fonts) {
                font.insert(kind.to_string(), fs);
            }
        }
        Self { layout, font }
    }

    /// Layout attributes for type `kind` other than `Type` itself.
    fn layout_without_type(&self, kind: &str) -> Vec<&str> {
        self.layout
            .get(kind)
            .map(|attrs| attrs.iter().filter(|a| !is_attr(a, "Type")).map(String::as_str).collect())
            .unwrap_or_default()
    }

    /// The XML of new paragraph `p`, its lines (after the first) indented by
    /// `indent` and ended by `eol`.
    fn xml(&self, p: &Paragraph, indent: &str, eol: &str) -> String {
        let kind = p.kind.name();
        let type_attr = format!(" Type=\"{}\"", escape_attr(kind));
        let attrs: String = match self.layout.get(kind) {
            Some(attrs) if attrs.iter().any(|a| is_attr(a, "Type")) => attrs.concat(),
            Some(attrs) => format!("{type_attr}{}", attrs.concat()),
            None => type_attr,
        };
        let font = self.font.get(kind).map_or("", String::as_str);
        let mut runs: Vec<String> = p
            .spans
            .iter()
            .filter(|s| !s.text.is_empty())
            .map(|s| {
                let names: Vec<&str> =
                    [(s.style.bold, "Bold"), (s.style.italic, "Italic"), (s.style.underline, "Underline")]
                        .into_iter()
                        .filter_map(|(on, name)| on.then_some(name))
                        .collect();
                let style = if names.is_empty() { String::new() } else { format!(" Style=\"{}\"", names.join("+")) };
                format!("<Text{font}{style}>{}</Text>", escape_text(&s.text))
            })
            .collect();
        if runs.is_empty() {
            runs.push(format!("<Text{font}></Text>"));
        }
        let child = format!("{eol}{indent}  ");
        format!("<Paragraph{attrs}>{child}{}{eol}{indent}</Paragraph>", runs.join(&child))
    }
}

/// Whether ` Name="value"` is attribute `name`.
fn is_attr(raw: &str, name: &str) -> bool {
    raw.trim_start().strip_prefix(name).is_some_and(|rest| rest.trim_start().starts_with('='))
}

fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;")
}

/// Retype one paragraph's XML from `from` to `to`: `Type` rewritten in place,
/// the old layout attributes swapped for `to`'s template's (none if the file
/// has no paragraph of that type), and, leaving Scene Heading, `Number` and
/// `<SceneProperties>` dropped. Everything else is kept.
fn retype(xml: &str, from: &str, to: &ElementKind, templates: &Templates) -> String {
    let leaving_heading = from == "Scene Heading" && *to != ElementKind::SceneHeading;
    let tag = open_tag(xml);
    let attrs = tag_attrs(tag);
    let new_type = format!(" Type=\"{}\"{}", escape_attr(to.name()), templates.layout_without_type(to.name()).concat());
    let first = attrs.first().map_or(tag.trim_end_matches('/').trim_end().len(), |(_, r)| r.start);
    let mut out = tag[..first].to_string();
    if !attrs.iter().any(|(name, _)| *name == "Type") {
        out.push_str(&new_type);
    }
    let mut pos = first;
    for (name, range) in &attrs {
        out.push_str(&tag[pos..range.start]);
        pos = range.end;
        if *name == "Type" {
            out.push_str(&new_type);
        } else if LAYOUT_ATTRS.contains(name) || (leaving_heading && *name == "Number") {
            // dropped
        } else {
            out.push_str(&tag[range.clone()]);
        }
    }
    out.push_str(&tag[pos..]);
    let mut body = xml[tag.len()..].to_string();
    if leaving_heading {
        body = drop_scene_properties(&body);
    }
    out + &body
}

/// `body` without its first `<SceneProperties>` element (and the indentation
/// and line break before it).
fn drop_scene_properties(body: &str) -> String {
    let Some(start) = body.find("<SceneProperties") else { return body.to_string() };
    let after = &body[start + "<SceneProperties".len()..];
    if !after.starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/') {
        return body.to_string();
    }
    let Some(gt) = body[start..].find('>').map(|o| start + o) else { return body.to_string() };
    let end = if body[..gt].ends_with('/') {
        gt + 1
    } else {
        const CLOSE: &str = "</SceneProperties>";
        match body[gt..].find(CLOSE) {
            Some(c) => gt + c + CLOSE.len(),
            None => return body.to_string(),
        }
    };
    format!("{}{}", &body[..cut_start(body, start)], &body[end..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(id: u64, kind: &str, text: &str) -> Paragraph {
        Paragraph::plain(ParaId(id), ElementKind::parse_editable(kind).unwrap(), text)
    }

    #[test]
    fn plan_then_splice_is_write_script() {
        let raw = "<FinalDraft><Content>\n<Paragraph Type=\"Action\"><Text>A</Text></Paragraph>\n\
                   <Paragraph Type=\"Action\"><Text>B</Text></Paragraph>\n\
                   <Paragraph Type=\"Action\"><Text>C</Text></Paragraph>\n</Content></FinalDraft>";
        let f = FdxFile::parse(raw).unwrap();
        let mut s = f.to_script();
        s.paragraphs.remove(1);
        s.paragraphs[0].kind = ElementKind::Transition;
        s.paragraphs.insert(0, para(6, "Action", "Dawn."));
        s.paragraphs.push(para(7, "Transition", "CUT TO:"));
        let plan = plan(&f, &s).unwrap();
        assert_eq!(plan.removed, vec![1]);
        assert_eq!(plan.retypes, vec![0]);
        assert_eq!(plan.survivors.iter().map(|&(i, _)| i).collect::<Vec<_>>(), vec![0, 2]);
        let anchors: Vec<_> =
            plan.inserts.iter().map(|(a, g)| (*a, g.iter().map(|p| p.id.0).collect::<Vec<_>>())).collect();
        assert_eq!(anchors, vec![(None, vec![6]), (Some(2), vec![7])]);
        assert_eq!(splice(&f, &plan), f.write_script(&s).unwrap());
    }
}
