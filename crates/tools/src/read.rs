//! Reading the script: summary, paragraph lists, scenes laid out for a model.

use std::collections::BTreeMap;
use std::ops::Range;

use scriptorium_core::outline::{ParaContext, annotate};
use scriptorium_core::{Document, ElementKind, ParaId, Paragraph};
use serde_json::{Value, json};

use crate::args::Args;
use crate::catalogue::{MAX_PAGE, PAGE};
use crate::{Ctx, ToolError};

/// What `read_scene` appends to a paragraph that can't be changed.
pub const NOT_EDITABLE: &str = " [not editable]";
/// Ends a line inside a dual dialogue block: text changes only.
pub const DUAL_DIALOGUE: &str = " [dual dialogue]";

pub(crate) fn script_info(ctx: &Ctx) -> String {
    let script = ctx.doc.script();
    let cx = annotate(script);
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    let mut speakers: BTreeMap<&str, usize> = BTreeMap::new();
    for (p, c) in script.paragraphs.iter().zip(&cx) {
        *kinds.entry(p.kind.name()).or_default() += 1;
        if !c.speaker.is_empty() && p.kind == ElementKind::Dialogue {
            *speakers.entry(&c.speaker).or_default() += 1;
        }
    }
    let words: usize = script.paragraphs.iter().map(|p| p.text().split_whitespace().count()).sum();
    let mut out = format!(
        "paragraphs  {}\nscenes      {}\nwords       {words}\nreviews     {}\n\nelements\n",
        script.paragraphs.len(),
        cx.iter().filter(|c| c.heading).count(),
        ctx.session.reviews.len()
    );
    for (k, n) in &kinds {
        out.push_str(&format!("  {n:>5}  {k}\n"));
    }
    let mut top: Vec<_> = speakers.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    out.push_str("\ncharacters (dialogue paragraphs)\n");
    for (name, n) in top.iter().take(15) {
        out.push_str(&format!("  {n:>5}  {name}\n"));
    }
    out.trim_end().to_string()
}

pub(crate) fn list_paragraphs(ctx: &Ctx, a: &Args) -> Result<String, ToolError> {
    let script = ctx.doc.script();
    let cx = annotate(script);
    let lower = |k: &str| a.opt_str(k).map(|s| s.map(str::to_lowercase));
    let (contains, scene, speaker, kind) = (lower("contains")?, lower("scene")?, lower("speaker")?, lower("kind")?);
    let rows: Vec<(&Paragraph, &ParaContext, String)> =
        script.paragraphs.iter().zip(&cx).map(|(p, c)| (p, c, p.text())).collect();
    let matches: Vec<&(&Paragraph, &ParaContext, String)> = rows
        .iter()
        .filter(|(_, _, t)| contains.as_ref().is_none_or(|s| t.to_lowercase().contains(s.as_str())))
        .filter(|(_, c, _)| scene.as_ref().is_none_or(|s| c.scene.to_lowercase().contains(s.as_str())))
        .filter(|(_, c, _)| speaker.as_ref().is_none_or(|s| c.speaker.to_lowercase() == *s))
        .filter(|(p, _, _)| kind.as_ref().is_none_or(|k| p.kind.name().to_lowercase() == *k))
        .collect();
    let offset = a.opt_int("offset")?.unwrap_or(0).max(0) as usize;
    let limit = a.opt_int("limit")?.map_or(PAGE, |l| l.clamp(1, MAX_PAGE as i64) as u64) as usize;
    let page: Vec<Value> = matches
        .iter()
        .skip(offset)
        .take(limit)
        .map(|(p, c, t)| json!({ "id": p.id.0, "kind": p.kind.name(), "scene": c.scene, "speaker": c.speaker, "text": t }))
        .collect();
    let more = offset + page.len() < matches.len();
    Ok(json!({ "total": matches.len(), "offset": offset, "more": more, "paragraphs": page }).to_string())
}

/// The script split into scenes: paragraph index ranges, each starting at a
/// scene heading. Paragraphs before the first heading come first, as an
/// unnumbered range (`read_scene` heads it `BEFORE SCENE 1`).
struct Scenes {
    ranges: Vec<Range<usize>>,
    /// Whether `ranges[0]` is the part before the first heading.
    preamble: bool,
}

impl Scenes {
    fn of(cx: &[ParaContext]) -> Self {
        let starts: Vec<usize> = cx.iter().enumerate().filter(|(_, c)| c.heading).map(|(i, _)| i).collect();
        let mut ranges = vec![];
        let first = starts.first().copied().unwrap_or(cx.len());
        let preamble = first > 0;
        if preamble {
            ranges.push(0..first);
        }
        for (k, &s) in starts.iter().enumerate() {
            ranges.push(s..starts.get(k + 1).copied().unwrap_or(cx.len()));
        }
        Self { ranges, preamble }
    }

    fn count(&self) -> usize {
        self.ranges.len() - usize::from(self.preamble)
    }

    /// Range by scene number (1-based), skipping the preamble.
    fn numbered(&self, n: usize) -> Option<&Range<usize>> {
        n.checked_sub(1).and_then(|k| self.ranges.get(k + usize::from(self.preamble)))
    }

    /// Position in `ranges` of the range holding paragraph index `ix`.
    fn holding(&self, ix: usize) -> Option<usize> {
        self.ranges.iter().position(|r| r.contains(&ix))
    }

    fn number_of(&self, k: usize) -> Option<usize> {
        if self.preamble { k.checked_sub(1).map(|n| n + 1) } else { Some(k + 1) }
    }
}

/// Shown in capitals, as the script displays them.
type IsCaps<'a> = &'a dyn Fn(&ElementKind) -> bool;

/// One paragraph as `read_scene` lays it out, or `None` for an empty one.
fn line(p: &Paragraph, fixed: bool, is_caps: IsCaps) -> Option<String> {
    let text = p.text().replace(['\r', '\n', '\u{2028}', '\u{2029}'], " ");
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let text = if is_caps(&p.kind) { text.to_uppercase() } else { text.to_string() };
    let body = if p.kind == ElementKind::Dialogue { format!("  {text}") } else { text };
    let mark = if p.kind.is_editable() { "" } else { NOT_EDITABLE };
    let dual = if fixed { DUAL_DIALOGUE } else { "" };
    Some(format!("¶{} {body}{mark}{dual}", p.id.0))
}

fn render(doc: &Document, scenes: &Scenes, k: usize, is_caps: IsCaps) -> String {
    let r = &scenes.ranges[k];
    let paras = &doc.paragraphs()[r.clone()];
    let (first, last) = (paras[0].id.0, paras[paras.len() - 1].id.0);
    let head = match scenes.number_of(k) {
        Some(n) => format!("SCENE {n} of {} (¶{first}–¶{last})", scenes.count()),
        None => format!("BEFORE SCENE 1 (¶{first}–¶{last})"),
    };
    let mut out = head;
    for l in paras.iter().filter_map(|p| line(p, doc.script().is_fixed(p.id), is_caps)) {
        out.push('\n');
        out.push_str(&l);
    }
    out
}

/// `scene` as a number: a JSON integer, or a string of digits naming an
/// existing scene, written as a number is (no sign, no leading zero). Any
/// other string is heading text: a heading may be just a number ("1987",
/// "007"), and "+3" or "007" isn't how anyone writes scene 3 or 7.
fn scene_number(scene: &Value, scenes: &Scenes) -> Option<i64> {
    if let Some(n) = scene.as_i64() {
        return Some(n);
    }
    let s = scene.as_str()?.trim();
    if s.is_empty() || s.starts_with('0') || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = s.parse::<i64>().ok()?;
    usize::try_from(n).ok().and_then(|u| scenes.numbered(u)).map(|_| n)
}

pub(crate) fn read_scene(ctx: &Ctx, a: &Args) -> Result<String, ToolError> {
    let doc = ctx.doc;
    let cx = annotate(doc.script());
    let scenes = Scenes::of(&cx);
    let k = match (a.get("scene"), a.opt_int("paragraph_id")?) {
        (Some(scene), None) => match (scene_number(scene, &scenes), scene.as_str()) {
            (Some(n), _) => {
                let count = scenes.count();
                let r = usize::try_from(n).ok().and_then(|n| scenes.numbered(n));
                let r = r.ok_or_else(|| ToolError::Invalid(format!("no scene {n} (1 to {count})")))?;
                scenes.holding(r.start).expect("a scene's range holds its start")
            }
            (None, Some(s)) => {
                let s = s.trim().to_lowercase();
                if s.is_empty() {
                    return Err(ToolError::Args("`scene` is empty: give a scene number or heading text".into()));
                }
                let found = scenes
                    .ranges
                    .iter()
                    .enumerate()
                    .filter(|(k, _)| scenes.number_of(*k).is_some())
                    .find(|(_, r)| cx[r.start].scene.to_lowercase().contains(&s));
                found.map(|(k, _)| k).ok_or_else(|| {
                    let count = scenes.count();
                    ToolError::Invalid(if !s.starts_with('0') && s.chars().all(|c| c.is_ascii_digit()) {
                        format!("no scene {s} (1 to {count}) and no scene heading contains {s:?}")
                    } else {
                        format!("no scene heading contains {s:?}")
                    })
                })?
            }
            _ => return Err(ToolError::Args("`scene` must be a scene number or heading text".into())),
        },
        (None, Some(id)) => {
            let ix = u64::try_from(id)
                .ok()
                .and_then(|id| doc.index_of(ParaId(id)))
                .ok_or_else(|| ToolError::Invalid(format!("no paragraph {id}")))?;
            scenes.holding(ix).expect("every paragraph is in a range")
        }
        _ => return Err(ToolError::Args("give exactly one of scene or paragraph_id".into())),
    };
    Ok(render(doc, &scenes, k, ctx.is_caps))
}

/// Every scene, as `read_scene` shows them, separated by blank lines.
fn script_body(doc: &Document, is_caps: IsCaps) -> (String, usize) {
    let cx = annotate(doc.script());
    let scenes = Scenes::of(&cx);
    let body = (0..scenes.ranges.len()).map(|k| render(doc, &scenes, k, is_caps)).collect::<Vec<_>>().join("\n\n");
    (body, scenes.count())
}

/// Rough size of what `read_script` returns, for asking before sending a long
/// script: (characters, ≈tokens at four characters a token).
pub fn script_size(doc: &Document, is_caps: &dyn Fn(&ElementKind) -> bool) -> (usize, usize) {
    let chars = script_body(doc, is_caps).0.chars().count();
    (chars, chars.div_ceil(4))
}

fn approx(tokens: usize) -> String {
    if tokens >= 1000 { format!("≈{}k tokens", (tokens + 500) / 1000) } else { format!("≈{tokens} tokens") }
}

/// Default `read_script` budget in tokens (≈ four characters each): under the
/// ~25k-token cap some MCP clients (Claude Code) put on a tool result.
pub const READ_BUDGET: usize = 20_000;
/// Smallest `max_tokens` taken (smaller asks are raised to this).
pub const MIN_READ_BUDGET: usize = 1_000;

/// The script, or the part of it from scene `from_scene` (1-based; default 1,
/// which includes any lines before the first heading) that fits `max_tokens`
/// (default [`READ_BUDGET`]); always at least one scene. When the whole script
/// fits, the header is the summary line alone; otherwise it says which scenes
/// this part covers and how to get the next.
pub(crate) fn read_script(ctx: &Ctx, a: &Args) -> Result<String, ToolError> {
    let doc = ctx.doc;
    let cx = annotate(doc.script());
    let scenes = Scenes::of(&cx);
    let count = scenes.count();
    let from = match a.opt_int("from_scene")? {
        None => 1,
        Some(n) if n >= 1 && (n as usize) <= count.max(1) => n as usize,
        Some(n) => return Err(ToolError::Invalid(format!("no scene {n} (1 to {count})"))),
    };
    let budget = match a.opt_int("max_tokens")? {
        None => READ_BUDGET,
        Some(n) => usize::try_from(n).unwrap_or(0).max(MIN_READ_BUDGET),
    };
    let tokens_of = |s: &str| s.chars().count().div_ceil(4);
    // Position in `ranges` of the first range to show: from scene 1 that is
    // the preamble too.
    let start = if from == 1 { 0 } else { from - 1 + usize::from(scenes.preamble) };
    let mut parts: Vec<String> = vec![];
    let mut used = 0;
    let mut end = start;
    while end < scenes.ranges.len() {
        let part = render(doc, &scenes, end, ctx.is_caps);
        let cost = tokens_of(&part) + usize::from(!parts.is_empty());
        // At least one numbered scene, so the next part starts past this one.
        let has_scene = end > start && !(scenes.preamble && end == 1);
        if has_scene && used + cost > budget {
            break;
        }
        used += cost;
        parts.push(part);
        end += 1;
    }
    let body = parts.join("\n\n");
    let summary = |tokens: usize| {
        format!("{}: {count} scenes, {} paragraphs, {}", ctx.script_name, doc.paragraphs().len(), approx(tokens))
    };
    if start == 0 && end == scenes.ranges.len() {
        return Ok(format!("{}\n\n{body}", summary(tokens_of(&body))));
    }
    let total = script_size(doc, ctx.is_caps).1;
    // Scene numbers shown: the last range's number (the preamble alone has none).
    let last = scenes.number_of(end - 1).expect("a part ends with a numbered scene");
    let covers = if from == last { format!("scene {from}") } else { format!("scenes {from}–{last}") };
    let next = if end < scenes.ranges.len() {
        format!("More follows: call read_script with from_scene {}.", last + 1)
    } else {
        "This part ends the script.".to_string()
    };
    Ok(format!(
        "{} in all. This part: {covers} of {count} ({}). {next}\n\n{body}",
        summary(total),
        approx(tokens_of(&body))
    ))
}
