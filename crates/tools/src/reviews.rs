//! Listing attached reviews and their suggestions.

use std::collections::HashMap;

use scriptorium_core::outline::annotate;
use scriptorium_core::{Document, ParaId};
use scriptorium_review::{Item, Session, Status};
use serde::Serialize;
use serde_json::{Value, json};

use crate::args::Args;
use crate::{Ctx, ToolError};

pub(crate) fn list_reviews(ctx: &Ctx, summary: bool) -> String {
    let rows: Vec<Value> = ctx
        .session
        .reviews
        .iter()
        .enumerate()
        .map(|(r, review)| {
            let p = ctx.session.progress(ctx.doc, r);
            let mut row = json!({
                "name": review.name, "path": review.path, "id": review.id,
                "default": ctx.default_review == Some(r),
                "total": p.total, "pending": p.pending, "accepted": p.accepted,
                "kept": p.rejected, "rewritten": p.rewritten,
                "removed": p.removed,
            });
            if summary {
                let unmatched: Vec<Value> =
                    review.unmatched.iter().map(|u| json!({ "block": u.entry + 1, "before": u.before })).collect();
                row["invisible"] = json!(review.invisible.len());
                row["unmatched"] = json!(unmatched);
                row["warnings"] = json!(review.warnings);
            }
            row
        })
        .collect();
    Value::Array(rows).to_string()
}

/// The attached review stored at `path`, for resolving
/// [`Target::File`](crate::Target::File): `path` is compared with each
/// review's [`Review::path`](scriptorium_review::Review::path), never with its
/// name or file name, so `b/x.review.txt` never resolves to an attached
/// `a/x.review.txt`.
///
/// This crate can't touch the file system, so the comparison is lexical
/// (`\` as `/`, `.` and empty components dropped, `..` folded into the
/// component before it). Hosts must pass the path in the form the session
/// stores ([`stored_path`](scriptorium_review::sessionfile::stored_path) of
/// the canonical path), so two spellings of one file compare equal. The
/// comparison is case-sensitive, even on file systems that aren't: the
/// canonical stored path is what makes spellings agree. A `..` never climbs
/// above the root, including a drive prefix (`C:/../x` is `C:/x`).
pub fn find_review(session: &Session, path: &str) -> Option<usize> {
    let want = normal_path(path);
    if want.is_empty() {
        return None;
    }
    session.reviews.iter().position(|r| normal_path(&r.path) == want)
}

/// Each item's number (1-based) as listings show it and `decide` takes it:
/// by the paragraph's position in `doc` as it is now, so `#n` is 1 + the
/// number of items before it in the script. Items keep their order in
/// `items` (hosts rely on stable item indices; [`Session::append_entries`]
/// adds new ones at the end), so the number is computed, not the vector
/// position.
///
/// An item whose paragraph is gone (removed or joined away) keeps a number,
/// so removing a line shifts no `#n`: it goes right after the surviving item
/// that came before it when matched (by matched position, [`Item::index`]),
/// which is where it was numbered before the line went, and where a reopened
/// session places it.
///
/// One lookup map and two sorts: fast enough to call per refresh on a long
/// script (hosts that render often should still keep the result).
pub fn item_numbers(doc: &Document, items: &[Item]) -> Vec<usize> {
    let position: HashMap<ParaId, usize> = doc.paragraphs().iter().enumerate().map(|(i, p)| (p.id, i)).collect();
    let live: Vec<Option<usize>> = items.iter().map(|it| position.get(&it.para).copied()).collect();
    // Sweep in matched order; a gone item takes the live position of the
    // last live item before it (its anchor).
    let mut matched: Vec<usize> = (0..items.len()).collect();
    matched.sort_unstable_by_key(|&i| (items[i].index, i));
    let mut anchor = vec![None; items.len()];
    let mut last_live = None;
    for &i in &matched {
        match live[i] {
            Some(pos) => last_live = Some(pos),
            None => anchor[i] = last_live,
        }
    }
    // (live position of the anchor, 0 for a live item / 1 + matched position for a gone one, i)
    let key = |i: usize| match live[i] {
        Some(pos) => (Some(pos), 0, i),
        None => (anchor[i], 1 + items[i].index, i),
    };
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_unstable_by_key(|&i| key(i));
    let mut numbers = vec![0; items.len()];
    for (k, &i) in order.iter().enumerate() {
        numbers[i] = k + 1;
    }
    numbers
}

/// The index into `items` of the item numbered `n` by [`item_numbers`].
pub fn item_numbered(doc: &Document, items: &[Item], n: usize) -> Option<usize> {
    item_numbers(doc, items).iter().position(|&m| m == n)
}

/// `path` with its separators and `.`/`..` components folded lexically.
fn normal_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    // A drive prefix (`C:`) is part of the root: `..` never pops it.
    let (drive, rest) = match path.as_bytes() {
        [d, b':', ..] if d.is_ascii_alphabetic() => path.split_at(2),
        _ => ("", path.as_str()),
    };
    let rooted = rest.starts_with('/');
    let mut parts: Vec<&str> = vec![];
    for c in rest.split('/') {
        match c {
            "" | "." => {}
            ".." if parts.last().is_some_and(|l| *l != "..") => {
                parts.pop();
            }
            // Nothing above a root (`/`, `C:/`) or a drive (`C:`).
            ".." if rooted || !drive.is_empty() => {}
            c => parts.push(c),
        }
    }
    format!("{drive}{}{}", if rooted { "/" } else { "" }, parts.join("/"))
}

/// An attached review a model names (`list_suggestions`'s `review`): by
/// path, then by name, then by file name alone.
fn named_review(session: &Session, which: &str) -> Option<usize> {
    let base = |s: &str| s.rsplit(['/', '\\']).next().unwrap_or(s).to_string();
    let reviews = &session.reviews;
    find_review(session, which)
        .or_else(|| reviews.iter().position(|r| r.name == which))
        .or_else(|| reviews.iter().position(|r| base(&r.path) == base(which) || base(&r.name) == base(which)))
}

/// The review a call means: `review` if given, else the default.
fn pick_review(ctx: &Ctx, which: Option<&str>) -> Result<usize, ToolError> {
    let names = || ctx.session.reviews.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", ");
    match which {
        Some(w) => named_review(ctx.session, w)
            .ok_or_else(|| ToolError::Invalid(format!("no attached review {w:?} (attached: {})", names()))),
        None => ctx.default_review.filter(|&r| r < ctx.session.reviews.len()).ok_or_else(|| {
            ToolError::Invalid(if ctx.session.reviews.is_empty() {
                "no reviews attached yet".into()
            } else {
                format!("which review? give `review` (attached: {})", names())
            })
        }),
    }
}

#[derive(Serialize)]
struct SuggestionRow {
    /// Number in this listing (1-based, by position in the script); what `decide` takes.
    n: usize,
    /// Stable position of the entry in the review file (0-based).
    entry: usize,
    paragraph: u64,
    kind: String,
    scene: String,
    speaker: String,
    status: String,
    /// Pending, but the line changed since the suggestion was written.
    outdated: bool,
    /// How the line was changed structurally since ("line removed", "line
    /// cut", "line split", "line joined with the next", "line retyped");
    /// absent if not.
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<&'static str>,
    fuzzy: bool,
    current: String,
    before: String,
    options: Vec<String>,
    why: String,
}

pub(crate) fn list_suggestions(ctx: &Ctx, a: &Args) -> Result<String, ToolError> {
    let r = pick_review(ctx, a.opt_str("review")?)?;
    let pending_only = a.bool("pending_only")?;
    let (doc, session) = (ctx.doc, ctx.session);
    let cx = annotate(doc.script());
    let items = &session.reviews[r].items;
    let numbers = item_numbers(doc, items);
    let mut rows: Vec<SuggestionRow> = items
        .iter()
        .enumerate()
        .filter(|(i, _)| !pending_only || session.status(doc, r, *i) == Status::Pending)
        .map(|(i, it)| {
            let ix = doc.index_of(it.para);
            let p = ix.map(|ix| &doc.paragraphs()[ix]);
            let c = ix.map(|ix| &cx[ix]);
            SuggestionRow {
                n: numbers[i],
                entry: it.entry,
                paragraph: it.para.0,
                kind: p.map(|p| p.kind.name().to_string()).unwrap_or_default(),
                scene: c.map(|c| c.scene.clone()).unwrap_or_default(),
                speaker: c.map(|c| c.speaker.clone()).unwrap_or_default(),
                status: session.status_label(doc, r, i),
                outdated: session.is_outdated(doc, r, i),
                line: session.line_label(doc, r, i),
                fuzzy: it.fuzzy,
                current: p.map(|p| p.text()).unwrap_or_default(),
                before: it.before.clone(),
                options: it.options.clone(),
                why: it.why.clone(),
            }
        })
        .collect();
    rows.sort_by_key(|row| row.n);
    Ok(serde_json::to_string_pretty(&rows).expect("rows serialize"))
}
