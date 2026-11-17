//! `versions` and `rows` (plan step A5c): the version lined up with a script,
//! and its rows with their status. Both only read: the one write is re-find
//! rewriting the links file when a member's bytes changed (C6), under the
//! links lock, said in a note (done when the script is opened,
//! [`scriptorium_fsio::versions::resume`]).
//!
//! Rows are numbered from 1 in the links file's order (after re-find).
//! "This" is the script the command is called on, "other" the file lined up
//! with it. Statuses are worked out from the version's side
//! (`scriptorium_versions::RowStatus`); called on the original they are
//! swapped for this/other, and no row needs work there. `rows --json` prints
//! the rows as the `read_rows` tool does ([`scriptorium_tools::RowView`]).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use scriptorium_fsio::opened::Opened;
use scriptorium_fsio::versions::{LoadedPair, PairState};
use scriptorium_tools::{LinksCtx, ParaView};
use scriptorium_versions::{Note, RowStatus};
use serde_json::{Value, json};

/// The commands this file handles, dispatched from `commands::run_other`.
pub const COMMANDS: &[&str] = &["versions", "rows"];

pub fn command(cmd: &str, args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    match (cmd, args) {
        ("versions", [script]) => versions(script, false, notes),
        ("versions", [script, "--json"]) => versions(script, true, notes),
        ("rows", [script, rest @ ..]) => {
            let (mut scene, mut needs, mut json) = (None, false, false);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match *a {
                    "--scene" => scene = Some(*it.next().ok_or("--scene needs a scene number or heading text")?),
                    "--needs" => needs = true,
                    "--json" => json = true,
                    other => return Err(format!("unexpected argument {other:?}")),
                }
            }
            rows(script, scene, needs, json, notes)
        }
        _ => Err(format!("wrong arguments for `{cmd}`\n\n{}", crate::USAGE)),
    }
}

/// Open `script` read-only with its pair (loaded and re-found when it was
/// opened). `Ok(None)`: no links file beside it names it; a links file that
/// names it but can't be used is an error.
fn open(script: &str, notes: &mut Vec<String>) -> Result<Option<Opened>, String> {
    let o = Opened::open_read_only(script, notes)?;
    match o.pair() {
        PairState::None => Ok(None),
        PairState::Unusable { reason, .. } => {
            // Said as the error, not again as notes (a reason may join
            // several broken links files' notes).
            notes.retain(|n| !reason.contains(n.as_str()));
            Err(reason.clone())
        }
        PairState::Loaded(_) => Ok(Some(o)),
    }
}

/// The pair `o` was opened with.
fn pair(o: &Opened) -> &LoadedPair {
    o.pair().loaded().expect("opened with a pair")
}

/// `o`'s rows laid out, as the tools see them.
fn ctx(o: &Opened) -> LinksCtx<'_> {
    let pair = pair(o);
    let laid = pair.lay_out(&o.fdx, &o.session, &o.doc);
    LinksCtx::new(&pair.links, pair.this, laid.rows, laid.scripts, laid.placed.iter().map(|p| (p.review, p.item)))
}

fn no_pair(script: &str) -> String {
    format!("no version is lined up with {script}: no *.links.txt beside it names it")
}

/// The order statuses are counted in.
const STATUSES: [RowStatus; 6] = [
    RowStatus::InSync,
    RowStatus::Blank,
    RowStatus::OtherChanged,
    RowStatus::ThisChanged,
    RowStatus::BothChanged,
    RowStatus::OneSided,
];

fn versions(script: &str, json: bool, notes: &mut Vec<String>) -> Result<String, String> {
    let Some(o) = open(script, notes)? else {
        return Ok(if json { json!({ "pair": null }).to_string() + "\n" } else { no_pair(script) + "\n" });
    };
    let pair = pair(&o);
    let ctx = ctx(&o);
    let on_version = pair.this_is_version();
    let member = |m: usize| {
        let x = &pair.links.members[m];
        json!({
            "file": x.file,
            "language": x.language,
            "label": pair.links.label(m),
            "made": x.created.is_some(),
        })
    };
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &ctx.rows {
        *counts.entry(ctx.status_key(r.status)).or_default() += 1;
    }
    let needs = if on_version { ctx.rows.iter().filter(|r| r.needs).count() } else { 0 };
    // Rows with nothing listed on the version's side: lines new in the
    // original (or version lines removed on purpose).
    let new_rows: Vec<usize> = ctx
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.note == Some(Note::NothingOnVersionSide))
        .map(|(n, _)| n + 1)
        .collect();
    let links = pair.path.display().to_string();
    if json {
        let v = json!({
            "pair": {
                "links": links,
                "id": pair.links.id,
                "this": member(pair.this),
                "other": member(pair.other_index()),
                "other_path": pair.other_path.display().to_string(),
                "this_is_version": on_version,
                "rows": ctx.rows.len(),
                "status": counts,
                "needs": needs,
                "new_in_original": new_rows,
            }
        });
        return Ok(serde_json::to_string_pretty(&v).expect("json") + "\n");
    }
    let describe = |m: usize| {
        let x = &pair.links.members[m];
        let mut out = x.file.clone();
        let mut what = vec![];
        if let Some(code) = &x.language {
            what.push(format!("{} ({code})", scriptorium_versions::language_name(code)));
        }
        what.push(if pair.links.is_version(m) { "the version" } else { "the original" }.into());
        if x.created.is_some() {
            what.push("made by Scriptorium".into());
        }
        let _ = write!(out, ": {}", what.join(", "));
        out
    };
    let mut out = String::new();
    let _ = writeln!(out, "this     {}", describe(pair.this));
    let _ = writeln!(out, "other    {}", describe(pair.other_index()));
    let _ = writeln!(out, "links    {links}");
    let mut parts = vec![];
    for st in STATUSES {
        let n = ctx.rows.iter().filter(|r| r.status == st).count();
        if n > 0 {
            parts.push(format!("{n} {}", ctx.status_label(st)));
        }
    }
    let _ = writeln!(out, "rows     {}: {}", ctx.rows.len(), parts.join(", "));
    if on_version {
        let _ = writeln!(out, "needs    {needs}");
    }
    if !new_rows.is_empty() {
        let list: Vec<String> = new_rows.iter().map(|n| format!("#{n}")).collect();
        let _ = writeln!(out, "new in the original  {} ({})", new_rows.len(), list.join(", "));
    }
    Ok(out)
}

fn rows(
    script: &str,
    scene: Option<&str>,
    needs_only: bool,
    json: bool,
    notes: &mut Vec<String>,
) -> Result<String, String> {
    let o = open(script, notes)?.ok_or_else(|| no_pair(script))?;
    let ctx = ctx(&o);
    let scene = scene.map(|q| Value::String(q.to_string()));
    let views = ctx
        .views(&o.session, &o.doc, scene.as_ref(), needs_only)
        .map_err(|e| e.to_string().replace("`scene`", "--scene"))?;
    if json {
        let list: Vec<Value> = views.iter().map(|v| v.to_json()).collect();
        return Ok(serde_json::to_string_pretty(&Value::Array(list)).expect("json") + "\n");
    }
    let pair = pair(&o);
    let (this_m, other_m) = (pair.this, pair.other_index());
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# {} ({}) | {} ({})",
        pair.links.label(this_m),
        pair.links.members[this_m].file,
        pair.links.label(other_m),
        pair.links.members[other_m].file
    );
    let cell = |v: &[ParaView]| -> String {
        if v.is_empty() {
            return "—".into();
        }
        let parts: Vec<String> = v
            .iter()
            .map(|p| {
                let t = p.text.replace(['\r', '\n', '\u{2028}', '\u{2029}'], " ");
                let t = t.trim();
                format!("¶{} {}", p.id, if t.is_empty() { "[empty]" } else { t })
            })
            .collect();
        parts.join("  ")
    };
    for v in &views {
        let mark = if v.needs { " · needs" } else { "" };
        let _ = writeln!(out, "#{} {}{mark}  {} | {}", v.row, v.label, cell(&v.this), cell(&v.other));
        for sg in &v.suggestions {
            let _ = writeln!(out, "    suggestion: {} #{} ({})", sg.review, sg.n, sg.status);
        }
        if let Some(note) = v.note {
            let _ = writeln!(out, "    note: {note}");
        }
    }
    Ok(out)
}
