//! Proposing changes: validating suggestions (here) and adding them to a
//! review ([`apply`], called by the host once it has the review).

use std::collections::HashMap;

use scriptorium_core::text::{looks_same, normalize, similarity};
use scriptorium_core::{Document, ElementKind, ParaId};
use scriptorium_review::matching::FUZZY_THRESHOLD;
use scriptorium_review::{Entry, Session, Unplaced};
use serde::Serialize;
use serde_json::{Value, json};

use crate::args::Args;
use crate::catalogue::{MAX_BATCH, MAX_OPTIONS};
use crate::reviews::{find_review, item_numbers};
use crate::{Append, Applied, Ctx, Failed, Outcome, Proposal, Target, ToolError};

/// Where the suggestions go, and the attached review to check duplicates
/// against with whether it was named explicitly (`None`: the review doesn't
/// exist yet; [`apply`] checks once it does).
fn target(ctx: &Ctx, a: &Args) -> Result<(Target, Option<(usize, bool)>), ToolError> {
    if let Some(file) = a.opt_str("review_file")? {
        return Ok((Target::File(file.to_string()), find_review(ctx.session, file).map(|r| (r, true))));
    }
    Ok(match ctx.default_review.filter(|&r| r < ctx.session.reviews.len()) {
        Some(r) => (Target::Review { index: r, id: ctx.session.reviews[r].id.clone() }, Some((r, false))),
        None => (Target::Own, None),
    })
}

fn options(v: Option<&Value>) -> Result<Vec<String>, String> {
    let list = v
        .and_then(Value::as_array)
        .ok_or_else(|| format!("`options` must be a list of one to {MAX_OPTIONS} strings"))?;
    let options: Vec<String> = list
        .iter()
        .map(|o| o.as_str().map(str::to_string).ok_or_else(|| "`options` must be strings".to_string()))
        .collect::<Result<_, _>>()?;
    match options.len() {
        0 => Err("give at least one option".into()),
        n if n > MAX_OPTIONS => Err(format!("give at most {MAX_OPTIONS} options, not {n}")),
        _ => Ok(options),
    }
}

/// Characters that would break a line into two paragraphs.
const LINE_BREAKS: [char; 4] = ['\n', '\r', '\u{2028}', '\u{2029}'];

/// The checks that depend only on the line as it is now: it exists, can be
/// changed, has text, and the options are single non-empty lines of which at
/// least one would visibly change it. Returns the line's live text.
fn check_line(
    doc: &Document,
    is_caps: &dyn Fn(&ElementKind) -> bool,
    id: u64,
    options: &[String],
) -> Result<String, String> {
    let p = doc.paragraph(ParaId(id)).ok_or_else(|| format!("no paragraph {id}"))?;
    if !p.kind.is_editable() {
        return Err(format!("paragraph {id} ({}) isn't editable", p.kind.name()));
    }
    let text = p.text();
    if text.trim().is_empty() {
        return Err(format!("paragraph {id} is empty: suggest changes to lines with text"));
    }
    if options.iter().any(|o| o.contains(LINE_BREAKS)) {
        return Err("options can't contain newlines: one paragraph per suggestion".into());
    }
    if options.iter().any(|o| o.trim().is_empty()) {
        return Err("options can't be empty".into());
    }
    let caps = is_caps(&p.kind);
    if options.iter().all(|o| looks_same(o, &text, caps)) {
        return Err("that change wouldn't be visible in the script".into());
    }
    Ok(text)
}

/// A review holds one suggestion per line (the matcher can show only one), so
/// a second one on a paragraph that already has one is refused, whatever its
/// status. `explicit`: the review was named (MCP's `review_file`) rather than
/// being the host's own.
fn check_duplicate(session: &Session, doc: &Document, r: usize, id: u64, explicit: bool) -> Result<(), String> {
    let review = &session.reviews[r];
    let Some(i) = review.items.iter().position(|it| it.para == ParaId(id)) else { return Ok(()) };
    let status = session.status_label(doc, r, i);
    Err(format!(
        "{} already has a suggestion on ¶{id} (#{}, {status}): one suggestion per line per review; the writer \
         can ask for a new one in a new conversation",
        whose(&review.name, explicit),
        item_numbers(doc, &review.items)[i]
    ))
}

/// How errors name review `name`.
fn whose(name: &str, explicit: bool) -> String {
    if explicit { format!("review {name}") } else { "this conversation's review".into() }
}

/// Why a proposal whose line no longer reads as it did isn't written.
fn changed(id: u64) -> String {
    format!("¶{id} changed since the suggestion was made: read it again and suggest on its current text")
}

/// Check one suggestion against the live document and the target review,
/// and build its review-file entry (`para` and the live text as `before`).
fn validate(
    ctx: &Ctx,
    review: Option<(usize, bool)>,
    id: u64,
    options: Vec<String>,
    why: &str,
) -> Result<Entry, String> {
    let text = check_line(ctx.doc, ctx.is_caps, id, &options)?;
    if let Some((r, explicit)) = review {
        check_duplicate(ctx.session, ctx.doc, r, id, explicit)?;
    }
    Ok(Entry {
        before: text.trim().to_string(),
        para: Some(ParaId(id)),
        options,
        why: why.to_string(),
        ..Entry::default()
    })
}

/// MCP's `before`: the line with this text, and whether it was found only
/// approximately.
///
/// Every line is ranked first, exact matches before fuzzy ones, and only then
/// checked, so an ineligible best line is reported rather than skipped (which
/// would put the suggestion on some other, worse-matching line):
/// - An exact match compares normalized text, ignoring letter case in
///   elements the script shows in capitals. Several exact matches are an
///   error listing them.
/// - Without one exact match, every line with a similarity of at least
///   [`FUZZY_THRESHOLD`] is a candidate. More than one candidate is an error
///   listing them and asking for `paragraph_id`: guessing could put the
///   suggestion on the wrong one of two similar lines.
/// - The single match found must be eligible: an error names it if it isn't
///   editable, or if the target review (`review`, with whether it was named
///   explicitly) already has a suggestion on it.
fn find_by_text(ctx: &Ctx, review: Option<(usize, bool)>, before: &str) -> Result<(u64, bool), ToolError> {
    let want = normalize(before);
    if want.is_empty() {
        return Err(ToolError::Args("`before` is empty".into()));
    }
    let list = |ids: &[u64]| ids.iter().map(|id| format!("¶{id}")).collect::<Vec<_>>().join(", ");
    let paras = ctx.doc.paragraphs();
    let exact: Vec<u64> = paras
        .iter()
        .filter(|p| {
            let text = normalize(&p.text());
            text == want || (ctx.is_caps)(&p.kind) && text.to_uppercase() == want.to_uppercase()
        })
        .map(|p| p.id.0)
        .collect();
    let (id, fuzzy) = match exact.as_slice() {
        [id] => (*id, false),
        [] => {
            let close: Vec<u64> = paras
                .iter()
                .filter(|p| {
                    let text = p.text();
                    !text.trim().is_empty() && similarity(before, &text) >= FUZZY_THRESHOLD
                })
                .map(|p| p.id.0)
                .collect();
            match close.as_slice() {
                [] => return Err(ToolError::Invalid(format!("no line in the script matches {before:?}"))),
                [id] => (*id, true),
                ids => {
                    return Err(ToolError::Invalid(format!(
                        "several lines read much like {before:?} ({}): give paragraph_id instead",
                        list(ids)
                    )));
                }
            }
        }
        ids => {
            return Err(ToolError::Invalid(format!(
                "{} lines read {before:?} ({}): give paragraph_id instead",
                ids.len(),
                list(ids)
            )));
        }
    };
    if !ctx.doc.paragraph(ParaId(id)).is_some_and(|p| p.kind.is_editable()) {
        return Err(ToolError::Invalid(format!("¶{id} isn't editable")));
    }
    if let Some((r, explicit)) = review {
        check_duplicate(ctx.session, ctx.doc, r, id, explicit).map_err(ToolError::Invalid)?;
    }
    Ok((id, fuzzy))
}

pub(crate) fn suggest_change(ctx: &Ctx, a: &Args) -> Result<Outcome, ToolError> {
    let (target, review) = target(ctx, a)?;
    let (id, fuzzy) = match (a.opt_int("paragraph_id")?, a.opt_str("before")?) {
        (Some(id), None) => (u64::try_from(id).map_err(|_| ToolError::Invalid(format!("no paragraph {id}")))?, false),
        (None, Some(b)) => find_by_text(ctx, review, b)?,
        _ => return Err(ToolError::Args("give exactly one of paragraph_id or before".into())),
    };
    let options = options(a.get("options")).map_err(ToolError::Args)?;
    let why = a.opt_str("why")?.unwrap_or("");
    let entry = validate(ctx, review, id, options, why).map_err(ToolError::Invalid)?;
    Ok(Outcome::Append(Append {
        target,
        proposals: vec![Proposal { index: 0, entry, fuzzy }],
        failed: vec![],
        batch: false,
    }))
}

/// One suggestion the batch result reports as added.
#[derive(Serialize)]
struct Added {
    index: usize,
    paragraph_id: u64,
    /// Its number in `list_suggestions`.
    n: usize,
    /// Its stable reference, `e<entry>`.
    entry: String,
}

fn batch_result(added: &[Added], failed: &[Failed]) -> String {
    json!({ "added": added.len(), "suggestions": added, "failed": failed }).to_string()
}

pub(crate) fn suggest_changes(ctx: &Ctx, a: &Args) -> Result<Outcome, ToolError> {
    let list = a
        .get("suggestions")
        .and_then(Value::as_array)
        .ok_or_else(|| ToolError::Args("`suggestions` must be a list".into()))?;
    if list.is_empty() {
        return Err(ToolError::Args("give at least one suggestion".into()));
    }
    if list.len() > MAX_BATCH {
        return Err(ToolError::Args(format!("at most {MAX_BATCH} suggestions per call, not {}", list.len())));
    }
    let (target, review) = target(ctx, a)?;
    let (mut proposals, mut failed) = (vec![], vec![]);
    // Paragraph → the batch item already suggesting on it.
    let mut seen: HashMap<u64, usize> = HashMap::new();
    for (index, item) in list.iter().enumerate() {
        let id = item.get("paragraph_id").and_then(Value::as_u64);
        let checked = (|| {
            let id = id.ok_or("`paragraph_id` must be a paragraph number")?;
            let options = options(item.get("options"))?;
            let why = match item.get("why") {
                None | Some(Value::Null) => "",
                Some(w) => w.as_str().ok_or("`why` must be a string")?,
            };
            let entry = validate(ctx, review, id, options, why)?;
            if let Some(k) = seen.get(&id) {
                return Err(in_batch(id, *k));
            }
            Ok::<_, String>(entry)
        })();
        match checked {
            Ok(entry) => {
                seen.insert(entry.para.expect("validate sets para").0, index);
                proposals.push(Proposal { index, entry, fuzzy: false });
            }
            Err(error) => failed.push(Failed { index, paragraph_id: id, error }),
        }
    }
    if proposals.is_empty() {
        return Ok(Outcome::Text(batch_result(&[], &failed)));
    }
    Ok(Outcome::Append(Append { target, proposals, failed, batch: true }))
}

fn in_batch(id: u64, k: usize) -> String {
    format!("paragraph {id} is already suggested on by item {k} of this batch")
}

/// Add validated suggestions to review `r` (the one `append.target` named,
/// resolved by the host) in one write, matching each by its `PARA:` against
/// `doc` as it is now.
///
/// Validation in [`call`](crate::call) may have seen another review (or none)
/// and another document: the host may have created the review since, or
/// another call may have added to it. So each proposal is checked again here,
/// against the resolved review (one suggestion per paragraph), earlier
/// proposals of this call, and the live line (exists, editable, still reads
/// as the proposal's `before`, change still visible); then all that pass are
/// given to `append_entries` with `para_only`, so a proposal whose line
/// changed is not placed on another line with the same text. Proposals that
/// fail any of this are never written. For [`Target::Review`], `r` must be
/// the review with the id the call saw. That check is only as good as the
/// ids: it relies on hosts pinning an `Id:` in every review they attach
/// ([`Review::pin_id`](scriptorium_review::Review::pin_id), per the host
/// contract), so no two attached reviews share one.
///
/// The tool result: for `suggest_change` a sentence with the suggestion's
/// number and entry (`#n`, `e<entry>`), quoting the line when MCP's `before`
/// found it approximately, or an error if nothing was written; for
/// `suggest_changes` always `{"added": n, "suggestions": [...], "failed": [...]}`,
/// even when nothing was added (`file_text` is then `None`). Whenever
/// something was written (`file_text` is `Some`), the result is `Ok`, so the
/// host saves it: a suggestion that turned out to change nothing visible is
/// written to the file but not shown, and the message says so.
pub fn apply(
    session: &mut Session,
    doc: &Document,
    r: usize,
    append: Append,
    is_caps: &dyn Fn(&ElementKind) -> bool,
) -> Result<Applied, ToolError> {
    if r >= session.reviews.len() {
        return Err(ToolError::Invalid(format!("no review {r}")));
    }
    let name = session.reviews[r].name.clone();
    if let Target::Review { id, .. } = &append.target
        && session.reviews[r].id != *id
    {
        return Err(ToolError::Invalid(format!(
            "{name} isn't the review this call was for (id {id}): reviews changed meanwhile; call the tool again"
        )));
    }
    let explicit = matches!(append.target, Target::File(_));
    let mut failed = append.failed;
    let n = append.proposals.len();
    // Proposal → its position among the entries handed to append_entries.
    let mut slot: Vec<Option<usize>> = vec![None; n];
    // Per entry handed over: (request index, paragraph, whether found approximately).
    let (mut entries, mut kept) = (vec![], vec![]);
    let mut seen: HashMap<u64, usize> = HashMap::new();
    for (k, Proposal { index, entry, fuzzy }) in append.proposals.into_iter().enumerate() {
        let id = entry.para.map(|p| p.0);
        let checked = (|| {
            let id = id.ok_or("the suggestion names no paragraph")?;
            let live = check_line(doc, is_caps, id, &entry.options)?;
            // The review's PARA pass would also take a line that now reads as
            // one of the options (a decided entry); a new suggestion is only
            // for the line as it was read.
            if normalize(&live) != normalize(&entry.before) {
                return Err(changed(id));
            }
            check_duplicate(session, doc, r, id, explicit)?;
            if let Some(&j) = seen.get(&id) {
                return Err(in_batch(id, j));
            }
            Ok::<_, String>(id)
        })();
        match checked {
            Ok(id) => {
                seen.insert(id, index);
                slot[k] = Some(entries.len());
                kept.push((index, id, fuzzy.then(|| entry.before.clone())));
                entries.push(entry);
            }
            Err(error) => failed.push(Failed { index, paragraph_id: id, error }),
        }
    }
    let mut numbers = vec![None; n];
    let mut written = vec![None; n];
    let mut added = vec![];
    let mut matched = None;
    let mut file_text = None;
    if !entries.is_empty() {
        let done = session.append_entries(doc, r, entries, true, is_caps).map_err(ToolError::Invalid)?;
        // By script position, as `list_suggestions` and `decide` number them.
        let by_position = item_numbers(doc, &session.reviews[r].items);
        for (k, s) in slot.iter().enumerate() {
            let Some(s) = *s else { continue };
            let (index, id, ref text) = kept[s];
            let res = done.results[s];
            written[k] = res.entry;
            match (res.unplaced, res.item, res.entry) {
                (None, Some(i), Some(e)) => {
                    numbers[k] = Some(by_position[i]);
                    added.push(Added { index, paragraph_id: id, n: by_position[i], entry: format!("e{e}") });
                    matched = matched.or(text.clone());
                }
                (why, _, entry) => {
                    let error = unplaced_error(why, id, entry, &name, explicit, &by_position);
                    failed.push(Failed { index, paragraph_id: Some(id), error });
                }
            }
        }
        if done.results.iter().any(|r| r.entry.is_some()) {
            file_text = Some(done.file_text);
        }
    }
    failed.sort_by_key(|f| f.index);
    let message = if append.batch {
        batch_result(&added, &failed)
    } else if let Some(a) = added.first() {
        let mut m = format!("added suggestion #{} ({}, ¶{}) to {name}", a.n, a.entry, a.paragraph_id);
        if let Some(text) = matched {
            m.push_str(&format!("; `before` matched ¶{} approximately, which reads {text:?}", a.paragraph_id));
        }
        m
    } else {
        let error = failed.pop().map_or("nothing to add".into(), |f| f.error);
        if file_text.is_none() {
            return Err(ToolError::Invalid(error));
        }
        error
    };
    Ok(Applied { file_text, numbers, entries: written, message })
}

/// Why `append_entries` gave proposal `id` no item (`why`), for the model.
/// `entry`: where it was written, if it was (only [`Unplaced::Invisible`]).
/// `numbers`: each item's number, from [`item_numbers`].
fn unplaced_error(
    why: Option<Unplaced>,
    id: u64,
    entry: Option<usize>,
    name: &str,
    explicit: bool,
    numbers: &[usize],
) -> String {
    match why {
        Some(Unplaced::Taken { item: Some(i), .. }) => {
            format!("{} already has a suggestion on ¶{id} (#{})", whose(name, explicit), numbers[i])
        }
        Some(Unplaced::Taken { item: None, .. }) => format!("¶{id} is already suggested on earlier in this call"),
        Some(Unplaced::NoPara) => "the suggestion names no paragraph".into(),
        Some(Unplaced::Missing) => format!("no paragraph {id}"),
        Some(Unplaced::NotEditable) => format!("paragraph {id} isn't editable"),
        Some(Unplaced::Mismatch) => changed(id),
        // Visibility was checked just before against the same line, so this
        // means review matching disagrees with that check.
        Some(Unplaced::Invisible) | None => {
            let at = entry.map_or(String::new(), |e| format!(" as e{e}"));
            format!("written to {name}{at}, but it changes nothing visible on ¶{id}, so it isn't shown")
        }
    }
}
