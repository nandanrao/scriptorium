//! Rows of a pair of versions as agents and the CLI see them: one shape,
//! [`RowView`], for the CLI's `rows --json` and the `read_rows` tool. Pure:
//! the host lays the rows out (`scriptorium_fsio::versions::LoadedPair::lay_out`)
//! and hands them over as a [`LinksCtx`].
//!
//! "This" is the script the rows are shown on, "other" the file lined up
//! with it. [`RowStatus`] is always from the version's side; shown on the
//! original it is swapped for this/other, and no row needs work there
//! (only the version is written).
//!
//! The tools here: `list_versions` ([`LinksCtx::summary`], what the CLI's
//! `versions --json` prints), `read_rows` ([`RowView::to_json`] per row,
//! what `rows --json` prints) and `new_version`; and the row a suggestion
//! on the version goes on ([`row_for`]).

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use scriptorium_core::text::normalize;
use scriptorium_core::{Document, ParaId, Paragraph, Script};
use scriptorium_review::{RowRef, Session};
use scriptorium_versions::{LaidRow, Member, Note, PairView, RowStatus};
use serde_json::{Value, json};

use crate::args::Args;
use crate::block::Spot;
use crate::reviews::item_numbers;
use crate::{Ctx, NewVersion, Outcome, ToolError};

/// Default and largest number of rows `read_rows` returns.
pub const ROWS_PAGE: usize = 200;
pub const MAX_ROWS_PAGE: usize = 500;
/// Most bytes of rows in one `read_rows` result (about [`crate::READ_BUDGET`]
/// tokens, as `read_script` keeps to): it stops early, at a whole row, and
/// says where to go on.
pub const ROWS_BYTES: usize = crate::READ_BUDGET * 4;

/// A pair's rows as the host laid them out, for the script the tools are
/// called on. Hosts build it from the shell's [`PairView`] (`LinksCtx::from`).
///
/// The statuses were worked out with one `is_caps` for both members, the
/// original's (plan, "Status"), though each `.fdx` has its own set of
/// element kinds shown in capitals: a letter-case-only difference on a kind
/// one file shows in capitals and the other doesn't may read as a change.
#[derive(Debug, Clone)]
pub struct LinksCtx<'a> {
    /// The links file's `id`.
    pub id: String,
    /// The links file, absolute (as the host found it).
    pub links_path: String,
    /// The members as the links file names them (file, language, `created`).
    pub members: Vec<Member>,
    /// Each member's file, absolute: in the links file's folder (both files
    /// of a pair live there).
    pub paths: Vec<String>,
    /// The rows, in the links file's order (numbered from 1).
    pub rows: Vec<LaidRow>,
    /// Each member's script as the rows read it (indexed by member): the
    /// version's live script, the original's file.
    pub scripts: Vec<Cow<'a, Script>>,
    /// Each member's label: its language's name, or its file stem.
    pub labels: Vec<String>,
    /// The member the tools are called on.
    pub this: usize,
    /// The version's member (the made one, or `b`).
    pub version: usize,
    /// Both members name the same language: a blank version line reads
    /// "blank", not "not yet translated".
    pub same_language: bool,
    /// The (review, item) in the session of each suggestion placed on a row,
    /// as [`LaidRow::suggestions`] indexes them. Empty on the original.
    pub placed: Vec<(usize, usize)>,
}

/// One row as shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowView {
    /// Its number, from 1.
    pub row: usize,
    /// `in_sync`, `this_changed`, `other_changed`, `both_changed`, `blank`,
    /// `one_sided`, from the script the rows are shown on.
    pub status: &'static str,
    /// The status in words, as the app shows it ("Español changed").
    pub label: String,
    pub needs: bool,
    pub note: Option<String>,
    pub this: Vec<ParaView>,
    pub other: Vec<ParaView>,
    /// The suggestions placed on the row (several reviews' may share it).
    pub suggestions: Vec<SuggestionView>,
}

/// A paragraph of a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParaView {
    pub id: u64,
    pub kind: String,
    pub text: String,
}

/// A suggestion on a row: its review's name, its number there (as
/// `list_suggestions` numbers it) and its status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestionView {
    pub review: String,
    pub n: usize,
    pub status: String,
}

impl RowView {
    /// The row as JSON: `{row, status, label, needs, note?, this: [{id, kind,
    /// text}], other: [...], suggestions: [{review, n, status}]}`.
    pub fn to_json(&self) -> Value {
        let paras = |v: &[ParaView]| -> Vec<Value> {
            v.iter().map(|p| json!({ "id": p.id, "kind": p.kind, "text": p.text })).collect()
        };
        let mut row = json!({
            "row": self.row,
            "status": self.status,
            "label": self.label,
            "needs": self.needs,
            "this": paras(&self.this),
            "other": paras(&self.other),
            "suggestions": self
                .suggestions
                .iter()
                .map(|s| json!({ "review": s.review, "n": s.n, "status": s.status }))
                .collect::<Vec<_>>(),
        });
        if let Some(note) = &self.note {
            row["note"] = json!(note);
        }
        row
    }
}

impl<'a> From<PairView<'a>> for LinksCtx<'a> {
    fn from(v: PairView<'a>) -> Self {
        let links = v.links;
        let folder = v.path.parent().unwrap_or(Path::new(""));
        LinksCtx {
            id: links.id.clone(),
            links_path: v.path.display().to_string(),
            members: links.members.clone(),
            paths: links.members.iter().map(|m| folder.join(&m.file).display().to_string()).collect(),
            rows: v.rows,
            scripts: v.scripts,
            labels: (0..links.members.len()).map(|m| links.label(m)).collect(),
            this: v.this,
            version: links.sides().1,
            same_language: links.same_language(),
            placed: v.placed,
        }
    }
}

impl<'a> LinksCtx<'a> {
    /// Whether the rows are shown on the version.
    pub fn on_version(&self) -> bool {
        self.this == self.version
    }

    /// The member lined up with this one.
    pub fn other(&self) -> usize {
        if self.this == 0 { 1 } else { 0 }
    }

    /// The original's member.
    pub fn original(&self) -> usize {
        if self.version == 0 { 1 } else { 0 }
    }

    /// The status as `this`/`other` words, from the script shown on.
    pub fn status_key(&self, status: RowStatus) -> &'static str {
        match (status, self.on_version()) {
            (RowStatus::InSync, _) => "in_sync",
            (RowStatus::ThisChanged, true) | (RowStatus::OtherChanged, false) => "this_changed",
            (RowStatus::OtherChanged, true) | (RowStatus::ThisChanged, false) => "other_changed",
            (RowStatus::BothChanged, _) => "both_changed",
            (RowStatus::Blank, _) => "blank",
            (RowStatus::OneSided, _) => "one_sided",
        }
    }

    /// The status as the app labels it: "in sync", "Español changed",
    /// "English changed", "both changed", "not yet translated" (between two
    /// files in one language: "blank"), "nothing on this side".
    pub fn status_label(&self, status: RowStatus) -> String {
        match status {
            RowStatus::InSync => "in sync".into(),
            RowStatus::ThisChanged => format!("{} changed", self.labels[self.version]),
            RowStatus::OtherChanged => format!("{} changed", self.labels[self.original()]),
            RowStatus::BothChanged => "both changed".into(),
            RowStatus::Blank if self.same_language => "blank".into(),
            RowStatus::Blank => "not yet translated".into(),
            RowStatus::OneSided if self.on_version() => "nothing on this side".into(),
            RowStatus::OneSided => format!("nothing on the {} side", self.labels[self.version]),
        }
    }

    /// Row `r`'s status in words ([`Self::status_label`]), naming the side
    /// with nothing on it.
    pub fn row_label(&self, r: &LaidRow) -> String {
        if r.status != RowStatus::OneSided {
            return self.status_label(r.status);
        }
        let empty = (0..r.cells.len()).find(|&m| r.cells[m].is_empty()).unwrap_or(self.version);
        if empty == self.this {
            "nothing on this side".into()
        } else {
            format!("nothing on the {} side", self.labels[empty])
        }
    }

    /// Row `r`'s note, as said on this side: on the original, the version's
    /// side is named rather than "this side".
    pub fn note_text(&self, note: Note) -> String {
        match note {
            Note::NothingOnVersionSide if !self.on_version() => format!(
                "nothing on the {} side: if the line is new here, add an empty line of this type to {} in Final \
                 Draft at this point, then reopen; if it was removed there on purpose, leave it",
                self.labels[self.version], self.members[self.version].file
            ),
            note => note.text().to_string(),
        }
    }

    /// The pair as `list_versions` and the CLI's `versions --json` show it:
    /// `{links, id, this: {file, path, language, label, made, is_version},
    /// other: {...}, version, rows, status: {<status>: n}, needs,
    /// new_in_original: [row]}`. `made`: the file Scriptorium made
    /// (`version new`); `is_version`: the side that is translated, written
    /// and gets suggestions (the made one, else the links file's `b`), whose
    /// absolute path is also the top-level `version`.
    pub fn summary(&self) -> Value {
        let member = |m: usize| {
            let x = &self.members[m];
            json!({
                "file": x.file,
                "path": self.paths[m],
                "language": x.language,
                "label": self.labels[m],
                "made": x.created.is_some(),
                "is_version": m == self.version,
            })
        };
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for r in &self.rows {
            *counts.entry(self.status_key(r.status)).or_default() += 1;
        }
        let needs = if self.on_version() { self.rows.iter().filter(|r| r.needs).count() } else { 0 };
        // Rows with nothing listed on the version's side: lines new in the
        // original (or version lines removed on purpose).
        let new_rows: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.note == Some(Note::NothingOnVersionSide))
            .map(|(n, _)| n + 1)
            .collect();
        json!({
            "links": self.links_path,
            "id": self.id,
            "this": member(self.this),
            "other": member(self.other()),
            "version": self.paths[self.version],
            "rows": self.rows.len(),
            "status": counts,
            "needs": needs,
            "new_in_original": new_rows,
        })
    }

    /// Which rows are in the scene `scene` names (a number, 1 = the first,
    /// or part of its heading, as `read_scene` takes it), as this side
    /// reads; heading text no heading here holds is looked for in the other
    /// side's headings too (a translated scene found by its original's
    /// words). A row is in the scene of its first paragraph on the side
    /// read; a row with none there stays in the scene of the row above.
    pub fn in_scene(&self, scene: &Value) -> Result<Vec<bool>, ToolError> {
        let here = crate::read::in_scene(&self.scripts[self.this], scene);
        let (inside, m) = match (here, scene) {
            (Ok(inside), _) => (inside, self.this),
            (Err(e), Value::String(_)) => match crate::read::in_scene(&self.scripts[self.other()], scene) {
                Ok(inside) => (inside, self.other()),
                Err(_) => return Err(e),
            },
            (Err(e), _) => return Err(e),
        };
        let mut cur = false;
        Ok(self
            .rows
            .iter()
            .map(|r| {
                if let Some(&b) = r.cells[m].first().and_then(|id| inside.get(id)) {
                    cur = b;
                }
                cur
            })
            .collect())
    }

    /// The rows as shown, those of `scene` only (see [`Self::in_scene`]) and,
    /// with `needs_only`, those that need work only. `session` and `doc` are
    /// the session the suggestions were placed in (the version's), for their
    /// names, numbers and statuses.
    pub fn views(
        &self,
        session: &Session,
        doc: &Document,
        scene: Option<&Value>,
        needs_only: bool,
    ) -> Result<Vec<RowView>, ToolError> {
        let in_scene = scene.map(|q| self.in_scene(q)).transpose()?;
        let (this_m, other_m) = (self.this, self.other());
        let numbers: Vec<Vec<usize>> = session.reviews.iter().map(|r| item_numbers(doc, &r.items)).collect();
        let by_id: Vec<HashMap<ParaId, &Paragraph>> =
            self.scripts.iter().map(|s| s.paragraphs.iter().map(|p| (p.id, p)).collect()).collect();
        let side = |cell: &[ParaId], m: usize| -> Vec<ParaView> {
            cell.iter()
                .filter_map(|id| by_id[m].get(id))
                .map(|p| ParaView { id: p.id.0, kind: p.kind.name().to_string(), text: p.text() })
                .collect()
        };
        let mut out = vec![];
        for (k, r) in self.rows.iter().enumerate() {
            let needs = r.needs && self.on_version();
            if needs_only && !needs || in_scene.as_ref().is_some_and(|set| !set[k]) {
                continue;
            }
            let suggestions = r
                .suggestions
                .iter()
                .filter_map(|&n| self.placed.get(n))
                .filter(|(rv, i)| *rv < session.reviews.len() && *i < session.reviews[*rv].items.len())
                .map(|&(rv, i)| SuggestionView {
                    review: session.reviews[rv].name.clone(),
                    n: numbers[rv][i],
                    status: session.status_label(doc, rv, i),
                })
                .collect();
            out.push(RowView {
                row: k + 1,
                status: self.status_key(r.status),
                label: self.row_label(r),
                needs,
                note: r.note.map(|n| self.note_text(n)),
                this: side(&r.cells[this_m], this_m),
                other: side(&r.cells[other_m], other_m),
                suggestions,
            });
        }
        Ok(out)
    }
}

/// `list_versions`: `{"pair": ...}` ([`LinksCtx::summary`]), `{"pair": null}`
/// when no version is lined up with the script.
pub(crate) fn list_versions(ctx: &Ctx) -> Result<String, ToolError> {
    let pair = match &ctx.links {
        None => Value::Null,
        Some(Err(reason)) => return Err(ToolError::Invalid(reason.clone())),
        Some(Ok(links)) => links.summary(),
    };
    Ok(serde_json::to_string_pretty(&json!({ "pair": pair })).expect("json"))
}

/// `read_rows {scene?, from_row?, limit?, needs?}`: `{"rows": [...],
/// "next_from_row": n | null}`, one row per line, each as the CLI's `rows
/// --json` prints it ([`RowView::to_json`]); `next_from_row` is where the
/// next page starts (`null`: no more rows).
pub(crate) fn read_rows(ctx: &Ctx, a: &Args) -> Result<String, ToolError> {
    let links = match &ctx.links {
        None => {
            return Err(ToolError::Invalid(format!(
                "no version is lined up with {}: no *.links.txt beside it names it",
                ctx.script_name
            )));
        }
        Some(Err(reason)) => return Err(ToolError::Invalid(reason.clone())),
        Some(Ok(links)) => links,
    };
    let needs = a.bool("needs")?;
    if needs && !links.on_version() {
        // An empty list would read as "nothing left to translate".
        return Err(ToolError::Invalid(format!(
            "no row needs work on {}: it is the original. Rows are translated on the version, {}",
            ctx.script_name, links.paths[links.version]
        )));
    }
    let from_row = match a.opt_int("from_row")? {
        None => 1,
        Some(n) if n >= 1 => n as usize,
        Some(_) => return Err(ToolError::Args("`from_row` must be 1 or more (rows are numbered from 1)".into())),
    };
    let limit = match a.opt_int("limit")? {
        None => ROWS_PAGE,
        Some(n) if n >= 1 => (n as usize).min(MAX_ROWS_PAGE),
        Some(_) => return Err(ToolError::Args("`limit` must be 1 or more".into())),
    };
    let views = links.views(ctx.session, ctx.doc, a.get("scene"), needs)?;
    let mut rows: Vec<String> = vec![];
    let (mut bytes, mut next) = (0, None);
    for v in views.iter().filter(|v| v.row >= from_row) {
        let line = serde_json::to_string(&v.to_json()).expect("json");
        // Always at least one row, however long.
        if rows.len() == limit || (!rows.is_empty() && bytes + line.len() > ROWS_BYTES) {
            next = Some(v.row);
            break;
        }
        bytes += line.len() + 2;
        rows.push(line);
    }
    let list = if rows.is_empty() { "[]".to_string() } else { format!("[\n{}\n]", rows.join(",\n")) };
    Ok(format!("{{\"rows\": {list},\n\"next_from_row\": {}}}", next.map_or("null".to_string(), |n| n.to_string())))
}

/// Whether `code` reads as a language code (`es`, `pt-BR`): what
/// `version new` takes, checked here so the error names the tool's argument.
fn language_code(code: &str) -> bool {
    (1..=35).contains(&code.len())
        && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && code.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
}

/// `new_version {language, original_language}`: [`Outcome::NewVersion`] for
/// the host, or the refusal. One version per script for now: a links file
/// naming the script that can't be used refuses (it may line up a version
/// already); a pair in another language refuses, naming its version; called
/// on the version of that language, the version's path is the answer.
/// Called on the original of a pair in that language, the host finds it
/// again ("already made") and gives its path.
pub(crate) fn new_version(ctx: &Ctx, a: &Args) -> Result<Outcome, ToolError> {
    let code = |k: &str| -> Result<String, ToolError> {
        let v = a.opt_str(k)?.ok_or_else(|| ToolError::Args(format!("missing `{k}` (a language code, like es)")))?;
        if !language_code(v) {
            return Err(ToolError::Args(format!("`{k}`: {v:?} isn't a language code (like es, fr or pt-BR)")));
        }
        Ok(v.to_string())
    };
    let (language, original_language) = (code("language")?, code("original_language")?);
    let links = match &ctx.links {
        None => return Ok(Outcome::NewVersion(NewVersion { language, original_language })),
        Some(Err(reason)) => {
            return Err(ToolError::Invalid(format!(
                "{reason}. A links file naming {} can't be used, so no version is made (it may line one up \
                 already): the writer fixes it or moves it away first",
                ctx.script_name
            )));
        }
        Some(Ok(links)) => links,
    };
    let v = links.version;
    let (file, path, label) = (&links.members[v].file, &links.paths[v], &links.labels[v]);
    let same = links.members[v].language.as_deref().is_some_and(|l| l.eq_ignore_ascii_case(&language));
    match (links.on_version(), same) {
        (false, true) => Ok(Outcome::NewVersion(NewVersion { language, original_language })),
        (false, false) => Err(ToolError::Invalid(format!(
            "{} already has a version, {file} ({label}), at {path}: one version per script for now",
            ctx.script_name
        ))),
        (true, true) => Ok(Outcome::Text(format!(
            "{} is already the version ({label}) of {}: suggest on it.\nThe version: {path}",
            ctx.script_name,
            links.members[links.original()].file
        ))),
        (true, false) => Err(ToolError::Invalid(format!(
            "{} is the version ({label}) of {}, at {}; a version is made of the original, and one per script for now",
            ctx.script_name,
            links.members[links.original()].file,
            links.paths[links.original()]
        ))),
    }
}

/// The row of the version a suggestion goes on, from [`row_for`].
pub(crate) struct RowTarget {
    /// Its number, from 1, as `read_rows` shows it.
    pub(crate) n: usize,
    /// The `ROW:` and `ROW TEXT:` fields for its entry.
    pub(crate) row: RowRef,
    /// The row is not yet translated ([`RowStatus::Blank`]): its blank
    /// lines may take a suggestion (`BEFORE: [empty]`).
    pub(crate) blank_ok: bool,
}

/// A row number: a whole number from 1, or one written as a string ("2").
fn row_number(v: &Value) -> Option<usize> {
    let n = match v {
        Value::String(s) => s.trim().parse::<u64>().ok(),
        v => v.as_u64(),
    };
    n.filter(|n| *n >= 1).and_then(|n| usize::try_from(n).ok())
}

/// `¶3, ¶4`.
fn ids_text(ids: &[u64]) -> String {
    ids.iter().map(|id| format!("¶{id}")).collect::<Vec<_>>().join(", ")
}

/// The row of the version a suggestion on `spot` goes on, `row` being the
/// argument if given:
/// - no pair: `None` (`row` is refused); a pair that can't be used: refused
///   (the host passes one only when this script may be the version), since
///   a suggestion on a version is placed by its rows;
/// - on the original: `None` (`row` is refused, naming the version);
/// - on the version: the row whose lines on this side are exactly the
///   target's, found from the ids when `row` isn't given; targets that
///   aren't one row's whole side are refused, naming the rows and their
///   ids. `insert_after` is refused: a row's lines change in number through
///   a list option.
pub(crate) fn row_for(ctx: &Ctx, row: Option<&Value>, spot: &Spot) -> Result<Option<RowTarget>, String> {
    let links = match &ctx.links {
        None if row.is_some() => {
            return Err(format!(
                "`row` is for a version: no version is lined up with {} (no *.links.txt beside it names it)",
                ctx.script_name
            ));
        }
        None => return Ok(None),
        Some(Err(reason)) => {
            return Err(format!(
                "{reason}. Suggestions on a version are placed by its links file, so none are added until it is \
                 fixed"
            ));
        }
        Some(Ok(links)) => links,
    };
    if !links.on_version() {
        if row.is_some() {
            return Err(format!(
                "rows are suggested on the version, {}; {} is the original. Pass that path as the script",
                links.paths[links.version], ctx.script_name
            ));
        }
        return Ok(None);
    }
    let given: Vec<u64> = match spot {
        Spot::Para(id) => vec![*id],
        Spot::List(ids) => ids.clone(),
        Spot::Range(a, b) => vec![*a, *b],
        Spot::InsertAfter(_) => {
            return Err("on a version, name the row's lines with paragraph_id or paragraph_ids, never insert_after; \
                        to change the number of lines, give each option as a list of {kind, text} lines"
                .into());
        }
    };
    let cell = |k: usize| -> Vec<u64> { links.rows[k].cells[links.this].iter().map(|p| p.0).collect() };
    let k = match row {
        Some(v) => {
            let n = row_number(v).ok_or("`row` must be a row number from read_rows (from 1)")?;
            if n > links.rows.len() {
                return Err(format!("no row {n}: the pair has {} rows", links.rows.len()));
            }
            let ids = cell(n - 1);
            if ids.is_empty() {
                return Err(format!("row {n} has nothing on this side: {}", links.row_label(&links.rows[n - 1])));
            }
            if given != ids {
                let hint = match ids.as_slice() {
                    [one] => format!("give paragraph_id {one}"),
                    _ => format!(
                        "give paragraph_ids [{}] with each option as a list of lines",
                        ids.iter().map(u64::to_string).collect::<Vec<_>>().join(", ")
                    ),
                };
                return Err(format!(
                    "row {n} is {} on this side (the row's `this` ids in read_rows): {hint}",
                    ids_text(&ids)
                ));
            }
            n - 1
        }
        None => {
            let touched: Vec<usize> =
                (0..links.rows.len()).filter(|&k| cell(k).iter().any(|id| given.contains(id))).collect();
            match touched.as_slice() {
                // Not in any row (the links file lists every line, so rare): an
                // ordinary suggestion.
                [] => return Ok(None),
                [k] if cell(*k) == given => *k,
                _ => {
                    let rows: Vec<String> =
                        touched.iter().map(|&k| format!("row {} ({})", k + 1, ids_text(&cell(k)))).collect();
                    let verb = if given.len() == 1 { "is" } else { "are" };
                    return Err(format!(
                        "{} {verb} in {}: a suggestion on a version covers exactly one row's lines on this side; \
                         name them all, and only those",
                        ids_text(&given),
                        rows.join(" and ")
                    ));
                }
            }
        }
    };
    let r = &links.rows[k];
    if let Some(note) = r.note {
        return Err(format!("row {} can't take a suggestion: {}", k + 1, links.note_text(note)));
    }
    let original = links.original();
    let by_id: HashMap<ParaId, &Paragraph> = links.scripts[original].paragraphs.iter().map(|p| (p.id, p)).collect();
    let texts = r.cells[original].iter().filter_map(|id| by_id.get(id)).map(|p| normalize(&p.text())).collect();
    Ok(Some(RowTarget {
        n: k + 1,
        row: RowRef { id: links.id.clone(), texts },
        blank_ok: r.status == RowStatus::Blank,
    }))
}
