//! Block suggestions: a run of paragraphs replaced by typed lines (B5).
//! Parsing the tool arguments that name a run and its options, and the
//! checks against the live document. Shared with the CLI's `suggest --paras
//! / --insert-after --block` through [`block_entry`].

use std::collections::HashSet;

use scriptorium_core::text::{looks_same, normalize};
use scriptorium_core::{DocError, Document, ElementKind, FIXED_MESSAGE, Group, NewPara, ParaId, Run, Selection};
use scriptorium_review::{Entry, Line};
use serde_json::Value;

use crate::catalogue::MAX_OPTIONS;

/// Most lines on either side of a block suggestion: the run it replaces and
/// each option (for `insert_after`, the new lines).
pub const MAX_LINES: usize = 40;

/// Characters that would break a line into two paragraphs.
pub(crate) const LINE_BREAKS: [char; 4] = ['\n', '\r', '\u{2028}', '\u{2029}'];

/// Which paragraphs a suggestion is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spot {
    /// One paragraph (`paragraph_id`).
    Para(u64),
    /// From the first to the last, inclusive, in script order (the CLI's
    /// `--paras a-b`; never built from `paragraph_ids`). Empty paragraphs at the edges are
    /// left out; one inside is refused (the caller didn't see it: `read_scene`
    /// hides empty lines), asking for the full list.
    Range(u64, u64),
    /// Exactly these consecutive paragraphs (every `paragraph_ids` list, and
    /// the CLI's `--paras a,b,c`). Empty paragraphs are allowed: they were named.
    List(Vec<u64>),
    /// Add lines after this paragraph (`insert_after`): the suggestion
    /// replaces it with itself, unchanged, followed by the new lines.
    InsertAfter(u64),
}

impl Spot {
    /// The first paragraph it names, for error reports.
    pub fn first(&self) -> u64 {
        match self {
            Spot::Para(id) | Spot::Range(id, _) | Spot::InsertAfter(id) => *id,
            Spot::List(ids) => ids.first().copied().unwrap_or_default(),
        }
    }
}

/// A suggestion's options as given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Opts {
    /// Plain strings: new texts for one line (only with `paragraph_id`).
    Strings(Vec<String>),
    /// Typed lines per option; `[]` is a cut.
    Lines(Vec<Vec<Line>>),
}

/// `paragraph_id` | `paragraph_ids` | `insert_after` from a suggestion's
/// arguments (`get` reads one argument, `None` for missing or null).
pub(crate) fn spot<'a>(get: impl Fn(&str) -> Option<&'a Value>) -> Result<Spot, String> {
    let id = |v: &Value, what: &str| v.as_u64().ok_or(format!("`{what}` must be a paragraph number"));
    let given: Vec<&str> =
        ["paragraph_id", "paragraph_ids", "insert_after"].into_iter().filter(|k| get(k).is_some()).collect();
    match given.as_slice() {
        ["paragraph_id"] => Ok(Spot::Para(id(get("paragraph_id").expect("given"), "paragraph_id")?)),
        ["insert_after"] => Ok(Spot::InsertAfter(id(get("insert_after").expect("given"), "insert_after")?)),
        ["paragraph_ids"] => {
            let list = get("paragraph_ids")
                .and_then(Value::as_array)
                .ok_or("`paragraph_ids` must be a list of paragraph numbers")?;
            let ids: Vec<u64> = list
                .iter()
                .map(|v| v.as_u64().ok_or("`paragraph_ids` must be paragraph numbers".to_string()))
                .collect::<Result<_, _>>()?;
            match ids.as_slice() {
                [] => Err("`paragraph_ids` is empty".into()),
                _ => Ok(Spot::List(ids)),
            }
        }
        _ => Err("give exactly one of paragraph_id, paragraph_ids or insert_after".into()),
    }
}

/// `options`: one to three, all strings or all lists of `{kind, text}`.
pub(crate) fn opts(v: Option<&Value>) -> Result<Opts, String> {
    let list = v.and_then(Value::as_array).ok_or(format!(
        "`options` must be a list of one to {MAX_OPTIONS} options (strings, or lists of {{kind, text}} lines)"
    ))?;
    match list.len() {
        0 => return Err("give at least one option (an option `[]` cuts the lines)".into()),
        n if n > MAX_OPTIONS => return Err(format!("give at most {MAX_OPTIONS} options, not {n}")),
        _ => {}
    }
    if list.iter().all(Value::is_string) {
        return Ok(Opts::Strings(list.iter().filter_map(|o| o.as_str().map(str::to_string)).collect()));
    }
    let lines = list
        .iter()
        .map(|o| {
            let o = o.as_array().ok_or("give every option as a string, or every option as a list of lines")?;
            o.iter().map(line).collect::<Result<Vec<_>, String>>()
        })
        .collect::<Result<_, _>>()?;
    Ok(Opts::Lines(lines))
}

/// One `{kind, text}` line.
fn line(v: &Value) -> Result<Line, String> {
    let kind =
        v.get("kind").and_then(Value::as_str).ok_or("each line is {\"kind\": <element type>, \"text\": <text>}")?;
    let text = v.get("text").and_then(Value::as_str).ok_or("each line needs a `text` (a string)")?;
    let kind = ElementKind::parse_editable(kind).ok_or(format!(
        "unknown element type {kind:?}: Scene Heading, Action, Character, Parenthetical, Dialogue, Transition, \
         Shot or General"
    ))?;
    Ok(Line::typed(kind, text))
}

/// The run of paragraphs `spot` names, checked: live, consecutive, editable,
/// at most [`MAX_LINES`]. `blank_ok`: blank lines may be the whole run (a
/// version's row not yet translated, named with `row`).
fn run(doc: &Document, spot: &Spot, blank_ok: bool) -> Result<Vec<ParaId>, String> {
    let index = |id: u64| doc.index_of(ParaId(id)).ok_or(format!("no paragraph {id}"));
    let paras = doc.paragraphs();
    let empty = |i: usize| !blank_ok && paras[i].text().trim().is_empty();
    let ids: Vec<ParaId> = match spot {
        Spot::Para(id) | Spot::InsertAfter(id) => {
            let i = index(*id)?;
            if empty(i) {
                return Err(match spot {
                    Spot::InsertAfter(_) => format!("paragraph {id} is empty: insert after the line before it"),
                    _ => format!("paragraph {id} is empty: suggest changes to lines with text"),
                });
            }
            vec![ParaId(*id)]
        }
        Spot::Range(a, b) => {
            let (i, j) = (index(*a)?, index(*b)?);
            if j < i {
                return Err(format!("¶{b} comes before ¶{a}: give [first, last] in script order"));
            }
            let (mut i, mut j) = (i, j);
            while i <= j && empty(i) {
                i += 1;
            }
            while j > i && empty(j) {
                j -= 1;
            }
            if i > j {
                return Err(format!("¶{a} to ¶{b} are all empty"));
            }
            if (i..=j).any(empty) {
                let all: Vec<String> = paras[i..=j].iter().map(|p| p.id.0.to_string()).collect();
                let gaps: Vec<String> = (i..=j).filter(|&k| empty(k)).map(|k| format!("¶{}", paras[k].id.0)).collect();
                return Err(format!(
                    "{} between ¶{a} and ¶{b} {} empty (the script view leaves empty lines out): give every id with \
                     --paras {}, so the empty lines are part of the change",
                    gaps.join(", "),
                    if gaps.len() == 1 { "is" } else { "are" },
                    all.join(",")
                ));
            }
            if j - i + 1 > MAX_LINES {
                return Err(format!("at most {MAX_LINES} lines per suggestion, not {}", j - i + 1));
            }
            paras[i..=j].iter().map(|p| p.id).collect()
        }
        Spot::List(ids) => {
            if ids.len() > MAX_LINES {
                return Err(format!("at most {MAX_LINES} lines per suggestion, not {}", ids.len()));
            }
            let mut seen = HashSet::new();
            if let Some(d) = ids.iter().find(|id| !seen.insert(**id)) {
                return Err(format!("¶{d} is listed twice in paragraph_ids"));
            }
            let at: Vec<usize> = ids.iter().map(|id| index(*id)).collect::<Result<_, _>>()?;
            for (w, pair) in at.windows(2).enumerate() {
                if pair[1] != pair[0] + 1 {
                    let (a, b) = (ids[w], ids[w + 1]);
                    return Err(if pair[1] < pair[0] {
                        format!("¶{b} comes before ¶{a}: list the paragraphs in script order")
                    } else {
                        let between: Vec<String> =
                            paras[pair[0] + 1..pair[1]].iter().map(|p| format!("¶{}", p.id.0)).collect();
                        format!(
                            "¶{a} and ¶{b} aren't next to each other ({} between them): paragraph_ids must be \
                             consecutive",
                            between.join(", ")
                        )
                    });
                }
            }
            if at.iter().all(|&i| empty(i)) {
                return Err("every listed paragraph is empty".into());
            }
            ids.iter().map(|id| ParaId(*id)).collect()
        }
    };
    for id in &ids {
        let p = doc.paragraph(*id).expect("checked live");
        if !p.kind.is_editable() {
            return Err(format!("paragraph {} ({}) isn't editable", id.0, p.kind.name()));
        }
    }
    Ok(ids)
}

/// Whether `lines` would read the same as `before` in the script.
fn invisible(lines: &[Line], before: &[Line], is_caps: &dyn Fn(&ElementKind) -> bool) -> bool {
    lines.len() == before.len()
        && lines
            .iter()
            .zip(before)
            .all(|(a, b)| a.kind == b.kind && b.kind.as_ref().is_some_and(|k| looks_same(&a.text, &b.text, is_caps(k))))
}

/// A block suggestion's review-file entry: `PARA` = every id of the run,
/// `BEFORE` = the run's live lines, typed; for [`Spot::InsertAfter`] each
/// option is `[anchor unchanged, new lines…]`. Checks everything that depends
/// on the document as it is now (not duplicates in a review: callers do
/// that): the run (see [`Spot`]); one to three options of at most
/// [`MAX_LINES`] lines, each a single non-empty line; at least one option
/// visibly different; `insert_after` adds at least one line per option; no
/// line added, removed or retyped inside a dual dialogue block; and that the
/// entry can be written to a review file and read back ([`Entry::check`]).
pub fn block_entry(
    doc: &Document,
    is_caps: &dyn Fn(&ElementKind) -> bool,
    spot: &Spot,
    options: Vec<Vec<Line>>,
    why: &str,
) -> Result<Entry, String> {
    block_entry_for(doc, is_caps, spot, options, why, false)
}

/// [`block_entry`], where `blank_ok` lets blank lines be the run (or the
/// `insert_after` anchor): a version's row not yet translated, named with
/// `row`; the caller adds the `ROW:` fields, without which the entry can't
/// be written.
pub(crate) fn block_entry_for(
    doc: &Document,
    is_caps: &dyn Fn(&ElementKind) -> bool,
    spot: &Spot,
    options: Vec<Vec<Line>>,
    why: &str,
    blank_ok: bool,
) -> Result<Entry, String> {
    let ids = run(doc, spot, blank_ok)?;
    let before: Vec<Line> = ids
        .iter()
        .map(|id| {
            let p = doc.paragraph(*id).expect("checked live");
            Line::typed(p.kind.clone(), p.text().trim())
        })
        .collect();
    match options.len() {
        0 => return Err("give at least one option (an option `[]` cuts the lines)".into()),
        n if n > MAX_OPTIONS => return Err(format!("give at most {MAX_OPTIONS} options, not {n}")),
        _ => {}
    }
    for o in &options {
        if o.len() > MAX_LINES {
            return Err(format!("at most {MAX_LINES} lines per option, not {}", o.len()));
        }
        for l in o {
            if l.text.contains(LINE_BREAKS) {
                return Err("a line can't contain a newline: give each paragraph as its own line".into());
            }
            if l.text.trim().is_empty() {
                return Err("lines can't be empty".into());
            }
            if l.kind.as_ref().is_none_or(|k| !k.is_editable()) {
                return Err("every line needs an element type".into());
            }
        }
    }
    let options: Vec<Vec<Line>> = match spot {
        Spot::InsertAfter(_) => {
            if options.iter().any(Vec::is_empty) {
                return Err("insert_after adds lines: give at least one line in every option (to cut, target the \
                            lines with paragraph_id or paragraph_ids and give an option `[]`)"
                    .into());
            }
            options
                .into_iter()
                .map(|o| {
                    let mut l = before.clone();
                    l.extend(o.into_iter().map(|l| Line { text: l.text.trim().to_string(), ..l }));
                    l
                })
                .collect()
        }
        _ => options
            .into_iter()
            .map(|o| o.into_iter().map(|l| Line { text: l.text.trim().to_string(), ..l }).collect())
            .collect(),
    };
    if options.iter().all(|o| invisible(o, &before, is_caps)) {
        return Err("that change wouldn't be visible in the script".into());
    }
    let fixed = |id: ParaId| doc.script().is_fixed(id);
    if ids.iter().copied().any(fixed) {
        // Try each option on a scratch copy: the editor's own rules decide
        // what is a structural change inside a dual dialogue block.
        for o in &options {
            let mut scratch = Document::new(doc.script().clone());
            let new: Vec<NewPara> =
                o.iter().map(|l| NewPara::plain(l.kind.clone().expect("typed"), l.text.clone())).collect();
            let sel = Selection::cursor(scratch.start());
            match scratch.edit(sel, Group::Other, |e| e.replace_run(Run::Ids(ids.clone()), &new).map(|_| (sel, ()))) {
                Ok(()) => {}
                Err(DocError::Fixed(_)) => return Err(FIXED_MESSAGE.into()),
                Err(e) => return Err(e.to_string()),
            }
        }
    }
    let entry = Entry { para: ids, before, options, why: why.to_string(), ..Entry::default() };
    // A blank run is checked once its `ROW:` fields are set.
    if !blank_ok {
        entry.check().map_err(|e| format!("the suggestion can't be written to a review file: {e}"))?;
    }
    Ok(entry)
}

/// Whether `entry`'s lines still read as its `BEFORE` in `doc` (kinds and
/// normalized text), as consecutive live paragraphs.
pub(crate) fn still_reads(doc: &Document, entry: &Entry) -> bool {
    let at: Vec<Option<usize>> = entry.para.iter().map(|id| doc.index_of(*id)).collect();
    let consecutive = at.windows(2).all(|w| matches!(w, [Some(a), Some(b)] if *b == a + 1));
    consecutive
        && at.iter().all(Option::is_some)
        && entry.para.len() == entry.before.len()
        && entry.para.iter().zip(&entry.before).all(|(id, l)| {
            doc.paragraph(*id).is_some_and(|p| {
                l.kind.as_ref().is_none_or(|k| *k == p.kind) && normalize(&p.text()) == normalize(&l.text)
            })
        })
}
