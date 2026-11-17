//! Structural edits from the command line: `insert`, `remove`, `split`,
//! `join` and `retype`. Each is one undoable transaction through core's
//! [`Edit`] primitives, run under the script's session lock and saved to the
//! session (the app sees it on its next sync). The functions are typed so the
//! MCP server can expose them later for explicit writer requests.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use scriptorium_core::doc::{DocError, Edit, Group, NewPara, Pos, Selection};
use scriptorium_core::{ElementKind, ParaId, paste};
use scriptorium_review::file::{Case, parse_typed_lines};
use serde::Serialize;

use crate::commands::lock;
use scriptorium_fsio::opened::Opened;

/// Where `insert` puts its paragraphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Start,
    End,
    After(u64),
}

/// Where `split` cuts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitAt<'a> {
    /// Before the first occurrence of this text.
    Text(&'a str),
    /// After this many characters.
    Offset(usize),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParaOut {
    pub id: u64,
    pub kind: String,
    pub text: String,
}

/// What a structural edit did: ids that did not exist before, ids that are
/// gone, and the affected paragraphs that remain, in script order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Changes {
    pub new: Vec<u64>,
    pub removed: Vec<u64>,
    pub paras: Vec<ParaOut>,
}

impl Changes {
    /// The text the command prints: JSON, or one line per paragraph.
    pub fn render(&self, json: bool) -> String {
        if json {
            return serde_json::to_string(self).expect("serializable") + "\n";
        }
        let mut out = String::new();
        if !self.removed.is_empty() {
            let ids: Vec<String> = self.removed.iter().map(|i| format!("¶{i}")).collect();
            let _ = writeln!(out, "removed {}", ids.join(" "));
        }
        for p in &self.paras {
            let new = if self.new.contains(&p.id) { " (new)" } else { "" };
            let _ = writeln!(out, "¶{} {}{new}: {}", p.id, p.kind, p.text);
        }
        out
    }
}

/// An editable element name. Case, spaces, dashes and other punctuation are
/// ignored, so "scene heading", "Scene-Heading", "scene_heading" and
/// "SCENEHEADING" all name Scene Heading.
pub fn parse_kind(s: &str) -> Result<ElementKind, String> {
    ElementKind::parse_editable(s).ok_or_else(|| {
        format!(
            "unknown element type {s:?}; use one of: Scene Heading, Action, Character, Parenthetical, \
             Dialogue, Transition, Shot, General"
        )
    })
}

/// Parse a block: one `TYPE: text` line per paragraph, TYPE in any case
/// (see [`parse_kind`] for the spellings).
pub fn parse_block(block: &str) -> Result<Vec<(ElementKind, String)>, String> {
    parse_typed_lines(block, Case::Any)
}

fn doc_err(e: DocError) -> String {
    match e {
        DocError::Fixed(id) => format!(
            "paragraph {} is part of a dual dialogue block, which takes text edits only: \
             it can't be added to, removed, split, joined or retyped (use `edit` for its text)",
            id.0
        ),
        DocError::LastParagraph => "the last paragraph can't be removed: insert its replacement first".into(),
        DocError::MissingParagraph(id) => format!("no paragraph {} (use `dump` to find ids)", id.0),
        other => other.to_string(),
    }
}

fn index_of(tx: &Edit<'_>, id: u64) -> Result<usize, DocError> {
    tx.doc().paragraphs().iter().position(|p| p.id.0 == id).ok_or(DocError::MissingParagraph(ParaId(id)))
}

/// Run `f` as one transaction on the script's saved session and report the
/// differences. `f` returns the paragraphs it touched.
fn transact(
    script: &str,
    notes: &mut Vec<String>,
    f: impl FnOnce(&mut Edit<'_>) -> Result<Vec<ParaId>, DocError>,
) -> Result<Changes, String> {
    let _lock = lock(script)?;
    let mut o = Opened::open(script, &[], notes)?;
    let ids = |o: &Opened| -> BTreeSet<u64> { o.doc.paragraphs().iter().map(|p| p.id.0).collect() };
    let before = ids(&o);
    let sel = o.sel();
    let touched = o
        .doc
        .edit(sel, Group::Other, |tx| {
            let touched = f(tx)?;
            Ok((Selection::cursor(tx.doc().start()), touched))
        })
        .map_err(doc_err)?;
    o.save()?;
    let after = ids(&o);
    let new: Vec<u64> = o.doc.paragraphs().iter().map(|p| p.id.0).filter(|i| !before.contains(i)).collect();
    let removed: Vec<u64> = before.difference(&after).copied().collect();
    let paras = o
        .doc
        .paragraphs()
        .iter()
        .filter(|p| new.contains(&p.id.0) || touched.contains(&p.id))
        .map(|p| ParaOut { id: p.id.0, kind: p.kind.name().to_string(), text: p.text() })
        .collect();
    Ok(Changes { new, removed, paras })
}

/// Insert typed paragraphs at `at`, as one undo step.
pub fn insert(
    script: &str,
    at: Anchor,
    paras: &[(ElementKind, String)],
    notes: &mut Vec<String>,
) -> Result<Changes, String> {
    if paras.is_empty() {
        return Err("nothing to insert".into());
    }
    for (_, t) in paras {
        check_text(t)?;
    }
    let mut refused = None;
    let result = transact(script, notes, |tx| {
        let index = match at {
            Anchor::Start => 0,
            Anchor::End => tx.doc().paragraphs().len(),
            Anchor::After(id) => index_of(tx, id)? + 1,
        };
        // Between two lines of one dual dialogue block is refused: say which.
        if let Some(i) = index.checked_sub(1)
            && let (Some(above), Some(below)) = (tx.doc().paragraphs().get(i), tx.doc().paragraphs().get(index))
            && let (Some(a), Some(b)) =
                (tx.doc().script().fixed_group(above.id), tx.doc().script().fixed_group(below.id))
            && a == b
        {
            refused = Some(format!(
                "can't insert between ¶{} and ¶{}: both are in one dual dialogue block, which takes \
                 text edits only (insert before or after the whole block)",
                above.id.0, below.id.0
            ));
            return Err(DocError::Fixed(below.id));
        }
        let mut ids = vec![];
        for (index, (kind, text)) in (index..).zip(paras) {
            ids.push(tx.insert_para(index, NewPara::plain(kind.clone(), text.clone()))?);
        }
        Ok(ids)
    });
    match (result, refused) {
        (Err(_), Some(why)) => Err(why),
        (result, _) => result,
    }
}

/// Untyped screenplay text as typed paragraphs, by the guess the app makes
/// when plain text is pasted ([`scriptorium_core::paste::classify`]).
pub fn guess_block(text: &str) -> Vec<(ElementKind, String)> {
    paste::classify(text).into_iter().map(|p| (p.kind, scriptorium_core::spans::text(&p.spans))).collect()
}

/// Refuse text that can't be one paragraph (moved to fsio, which `edit_paragraph` shares).
pub use scriptorium_fsio::ops::check_text;

/// Remove the paragraphs from `first` to `last` inclusive, in script order
/// (not by number). `last` before `first` is refused.
pub fn remove(script: &str, first: u64, last: u64, notes: &mut Vec<String>) -> Result<Changes, String> {
    transact(script, notes, |tx| {
        let (a, b) = (index_of(tx, first)?, index_of(tx, last)?);
        if b < a {
            return Err(DocError::Mismatch("remove: the last paragraph comes before the first in the script"));
        }
        let ids: Vec<ParaId> = tx.doc().paragraphs()[a..=b].iter().map(|p| p.id).collect();
        for id in ids {
            tx.remove_para(id)?;
        }
        Ok(vec![])
    })
    .map_err(|e| e.replace("operation does not match the document: ", ""))
}

/// Split paragraph `id` as Enter does (the original id keeps the original text).
pub fn split(script: &str, id: u64, at: SplitAt<'_>, notes: &mut Vec<String>) -> Result<Changes, String> {
    if let SplitAt::Text(t) = at {
        check_text(t)?;
    }
    transact(script, notes, |tx| {
        let _ = index_of(tx, id)?;
        if matches!(at, SplitAt::Text("")) {
            return Err(DocError::Mismatch("split: --at needs the text to split before (use --offset for a position)"));
        }
        let para = tx.doc().paragraph(ParaId(id)).expect("checked");
        let text = para.text();
        let offset = match at {
            SplitAt::Text(t) => {
                text.find(t).ok_or(DocError::Mismatch("split: the text to split before isn't in the paragraph"))?
            }
            SplitAt::Offset(n) => match text.char_indices().nth(n) {
                Some((b, _)) => b,
                None if n == text.chars().count() => text.len(),
                None => return Err(DocError::Mismatch("split: offset is past the end of the paragraph")),
            },
        };
        let new = tx.split(Pos::new(ParaId(id), offset))?;
        Ok(vec![ParaId(id), new])
    })
    .map_err(|e| e.replace("operation does not match the document: ", ""))
}

/// Join `second` onto `first`, which it must directly follow.
pub fn join(script: &str, first: u64, second: u64, notes: &mut Vec<String>) -> Result<Changes, String> {
    transact(script, notes, |tx| {
        tx.join(ParaId(first), ParaId(second))?;
        Ok(vec![ParaId(first), ParaId(second)])
    })
    .map_err(|e| e.replace("operation does not match the document: ", ""))
}

/// Give paragraph `id` another element kind.
pub fn retype(script: &str, id: u64, kind: &ElementKind, notes: &mut Vec<String>) -> Result<Changes, String> {
    transact(script, notes, |tx| {
        tx.set_kind(ParaId(id), kind.clone())?;
        Ok(vec![ParaId(id)])
    })
}

fn id_arg(s: &str) -> Result<u64, String> {
    s.parse().map_err(|_| format!("paragraph id must be a number, got {s:?}"))
}

/// The structural commands, dispatched by [`crate::run`].
pub const COMMANDS: &[&str] = &["insert", "remove", "split", "join", "retype"];

pub fn command(cmd: &str, args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let bad = || format!("wrong arguments for `{cmd}`\n\n{}", crate::USAGE);
    let (script, rest) = args.split_first().ok_or_else(bad)?;
    // Only a trailing `--json` is the flag; anywhere else it is text.
    let (rest, json) = match rest.split_last() {
        Some((&"--json", before)) => (before, true),
        _ => (rest, false),
    };
    let changes = match (cmd, rest) {
        ("insert", _) => {
            let (mut at, mut block, mut guess, mut pos) = (None, None, None, vec![]);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                let mut take = |what: &str| it.next().copied().ok_or(format!("{what} needs a value"));
                // After the type, the next argument is the text, whatever it looks like.
                let place = match *a {
                    text if pos.len() == 1 => {
                        pos.push(text);
                        None
                    }
                    "--after" => Some(Anchor::After(id_arg(take("--after")?)?)),
                    "--start" => Some(Anchor::Start),
                    "--end" => Some(Anchor::End),
                    "--block" => {
                        block = Some(take("--block")?);
                        None
                    }
                    "--guess" => {
                        guess = Some(take("--guess")?);
                        None
                    }
                    other if pos.is_empty() && other.starts_with("--") => {
                        return Err(format!(
                            "unknown option {other}; use --after <id>, --start, --end, --block or --guess"
                        ));
                    }
                    other => {
                        pos.push(other);
                        None
                    }
                };
                if let Some(p) = place
                    && at.replace(p).is_some()
                {
                    return Err("give one of --after, --start, --end".into());
                }
            }
            let at = at.ok_or("say where: --after <id>, --start or --end")?;
            let paras = match (block, guess, pos.as_slice()) {
                (Some(b), None, []) => parse_block(b)?,
                (None, Some(g), []) => guess_block(g),
                (None, None, [kind, text]) => vec![(parse_kind(kind)?, text.to_string())],
                _ => return Err(bad()),
            };
            insert(script, at, &paras, notes)?
        }
        ("remove", [range]) => {
            let (a, b) = match range.split_once("..") {
                Some((a, b)) => (id_arg(a)?, id_arg(b)?),
                None => (id_arg(range)?, id_arg(range)?),
            };
            remove(script, a, b, notes)?
        }
        ("split", [id, "--at", text]) => split(script, id_arg(id)?, SplitAt::Text(text), notes)?,
        ("split", [id, "--offset", n]) => {
            let n = n.parse().map_err(|_| format!("offset must be a number, got {n:?}"))?;
            split(script, id_arg(id)?, SplitAt::Offset(n), notes)?
        }
        ("join", [first, second]) => join(script, id_arg(first)?, id_arg(second)?, notes)?,
        ("retype", [id, kind]) => retype(script, id_arg(id)?, &parse_kind(kind)?, notes)?,
        _ => return Err(bad()),
    };
    Ok(changes.render(json))
}
