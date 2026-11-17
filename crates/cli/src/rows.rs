//! `versions` and `rows` (plan step A5c): the version lined up with a script,
//! and its rows with their status. Both only read: the one write is re-find
//! rewriting the links file when a member's bytes changed (C6), under the
//! links lock, said in a note ([`scriptorium_fsio::versions::load_pair`]).
//!
//! Rows are numbered from 1 in the links file's order (after re-find).
//! "This" is the script the command is called on, "other" the file lined up
//! with it. Statuses are worked out from the version's side
//! (`scriptorium_versions::RowStatus`); called on the original they are
//! swapped for this/other, and no row needs work there.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::Path;

use scriptorium_core::outline::annotate;
use scriptorium_core::{ParaId, Paragraph, Script};
use scriptorium_fsio::opened::{Opened, file_name, script_dir};
use scriptorium_fsio::versions::{LoadedPair, PlacedItem, load_pair, placed_items};
use scriptorium_tools::item_numbers;
use scriptorium_versions::{LaidRow, Note, RowStatus};
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

/// A script opened with its pair, laid out.
struct Shown {
    o: Opened,
    pair: LoadedPair,
    /// The version's `ROW:` suggestions (none when called on the original).
    placed: Vec<PlacedItem>,
    rows: Vec<LaidRow>,
    /// The script the command is called on, as rows read it: the version's
    /// live script, or the original's file (C8).
    this: Script,
    other: Script,
}

/// Open `script` read-only and load its pair (re-finding first). `Ok(None)`:
/// no links file beside it names it.
fn open(script: &str, notes: &mut Vec<String>) -> Result<Option<Shown>, String> {
    let o = Opened::open_read_only(script, notes)?;
    let dir = script_dir(script)?;
    let name = file_name(Path::new(script));
    let Some(pair) = load_pair(&dir, &name, &o.fdx, notes)? else { return Ok(None) };
    let this_live = o.doc.script().clone();
    let (this, placed) = if pair.this_is_version() {
        (this_live.clone(), placed_items(&o.session, &o.doc, &pair.links.id))
    } else {
        (o.fdx.to_script(), vec![])
    };
    let only: Vec<_> = placed.iter().map(|p| p.placed.clone()).collect();
    let rows = pair.lay_out(&o.fdx, &this_live, &only);
    let other = pair.other.to_script();
    Ok(Some(Shown { o, pair, placed, rows, this, other }))
}

fn no_pair(script: &str) -> String {
    format!("no version is lined up with {script}: no *.links.txt beside it names it")
}

/// The status as `this`/`other` words for JSON, from the script called on.
fn status_key(status: RowStatus, on_version: bool) -> &'static str {
    match (status, on_version) {
        (RowStatus::InSync, _) => "in_sync",
        (RowStatus::ThisChanged, true) | (RowStatus::OtherChanged, false) => "this_changed",
        (RowStatus::OtherChanged, true) | (RowStatus::ThisChanged, false) => "other_changed",
        (RowStatus::BothChanged, _) => "both_changed",
        (RowStatus::Blank, _) => "blank",
        (RowStatus::OneSided, _) => "one_sided",
    }
}

/// The status as the app labels it: "in sync", "Español changed", "English
/// changed", "both changed", "not yet translated" (between two files in one
/// language: "blank"), "nothing on one side".
fn status_label(status: RowStatus, pair: &LoadedPair) -> String {
    let (o, v) = scriptorium_versions::rows::sides(&pair.links);
    match status {
        RowStatus::InSync => "in sync".into(),
        RowStatus::ThisChanged => format!("{} changed", pair.label(v)),
        RowStatus::OtherChanged => format!("{} changed", pair.label(o)),
        RowStatus::BothChanged => "both changed".into(),
        RowStatus::Blank if pair.links.members[o].language == pair.links.members[v].language => "blank".into(),
        RowStatus::Blank => "not yet translated".into(),
        RowStatus::OneSided => "nothing on one side".into(),
    }
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
    let Some(s) = open(script, notes)? else {
        return Ok(if json { json!({ "pair": null }).to_string() + "\n" } else { no_pair(script) + "\n" });
    };
    let pair = &s.pair;
    let on_version = pair.this_is_version();
    let member = |m: usize| {
        let x = &pair.links.members[m];
        json!({
            "file": x.file,
            "language": x.language,
            "label": pair.label(m),
            "made": x.created.is_some(),
        })
    };
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &s.rows {
        *counts.entry(status_key(r.status, on_version)).or_default() += 1;
    }
    let needs = if on_version { s.rows.iter().filter(|r| r.needs).count() } else { 0 };
    // Rows with nothing listed on the version's side: lines new in the
    // original (or version lines removed on purpose).
    let new_rows: Vec<usize> = s
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
                "rows": s.rows.len(),
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
        what.push(
            if m == scriptorium_versions::rows::sides(&pair.links).1 { "the version" } else { "the original" }.into(),
        );
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
        let n = s.rows.iter().filter(|r| r.status == st).count();
        if n > 0 {
            parts.push(format!("{n} {}", status_label(st, pair)));
        }
    }
    let _ = writeln!(out, "rows     {}: {}", s.rows.len(), parts.join(", "));
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
    let s = open(script, notes)?.ok_or_else(|| no_pair(script))?;
    let pair = &s.pair;
    let on_version = pair.this_is_version();
    let (this_m, other_m) = (pair.this, pair.other_index());
    let in_scene = match scene {
        Some(q) => Some(scene_rows(&s.this, &s.rows, this_m, q)?),
        None => None,
    };
    let this_paras: HashMap<ParaId, &Paragraph> = s.this.paragraphs.iter().map(|p| (p.id, p)).collect();
    let other_paras: HashMap<ParaId, &Paragraph> = s.other.paragraphs.iter().map(|p| (p.id, p)).collect();
    let numbers: Vec<Vec<usize>> = s.o.session.reviews.iter().map(|r| item_numbers(&s.o.doc, &r.items)).collect();
    let suggestion = |n: usize| {
        let p = &s.placed[n];
        let name = &s.o.session.reviews[p.review].name;
        let status = s.o.session.status_label(&s.o.doc, p.review, p.item);
        (name.clone(), numbers[p.review][p.item], status)
    };

    let mut out = String::new();
    let mut list = vec![];
    if !json {
        let _ = writeln!(
            out,
            "# {} ({}) | {} ({})",
            pair.label(this_m),
            pair.links.members[this_m].file,
            pair.label(other_m),
            pair.links.members[other_m].file
        );
    }
    for (k, r) in s.rows.iter().enumerate() {
        let needs = r.needs && on_version;
        if needs_only && !needs || in_scene.as_ref().is_some_and(|set| !set[k]) {
            continue;
        }
        let side = |cell: &[ParaId], paras: &HashMap<ParaId, &Paragraph>| -> Vec<(u64, String, String)> {
            cell.iter().filter_map(|id| paras.get(id)).map(|p| (p.id.0, p.kind.name().to_string(), p.text())).collect()
        };
        let this = side(&r.cells[this_m], &this_paras);
        let other = side(&r.cells[other_m], &other_paras);
        let sugg: Vec<(String, usize, String)> = r.suggestions.iter().map(|&n| suggestion(n)).collect();
        if json {
            let paras = |v: &[(u64, String, String)]| -> Vec<Value> {
                v.iter().map(|(id, kind, text)| json!({ "id": id, "kind": kind, "text": text })).collect()
            };
            let mut row = json!({
                "row": k + 1,
                "status": status_key(r.status, on_version),
                "label": status_label(r.status, pair),
                "needs": needs,
                "this": paras(&this),
                "other": paras(&other),
            });
            if let Some(note) = r.note {
                row["note"] = json!(note.text());
            }
            if let Some((review, n, status)) = sugg.first() {
                row["suggestion"] = json!({ "review": review, "n": n, "status": status });
            }
            list.push(row);
            continue;
        }
        let cell = |v: &[(u64, String, String)]| -> String {
            if v.is_empty() {
                return "—".into();
            }
            let parts: Vec<String> = v
                .iter()
                .map(|(id, _, text)| {
                    let t = text.replace(['\r', '\n', '\u{2028}', '\u{2029}'], " ");
                    let t = t.trim();
                    format!("¶{id} {}", if t.is_empty() { "[empty]" } else { t })
                })
                .collect();
            parts.join("  ")
        };
        let mark = if needs { " · needs" } else { "" };
        let _ = writeln!(out, "#{} {}{mark}  {} | {}", k + 1, status_label(r.status, pair), cell(&this), cell(&other));
        for (review, n, status) in &sugg {
            let _ = writeln!(out, "    suggestion: {review} #{n} ({status})");
        }
        if let Some(note) = r.note {
            let _ = writeln!(out, "    note: {}", note.text());
        }
    }
    if json {
        return Ok(serde_json::to_string_pretty(&Value::Array(list)).expect("json") + "\n");
    }
    Ok(out)
}

/// Which rows are in the scene `q` names (a number, 1 = the first, or part
/// of its heading, as `read_scene` takes it) on the side of the script the
/// command is called on. A row is in the scene of its first paragraph on
/// that side; a row with none there stays in the scene of the row above.
fn scene_rows(this: &Script, rows: &[LaidRow], this_m: usize, q: &str) -> Result<Vec<bool>, String> {
    let cx = annotate(this);
    // Scene index per paragraph: 0 before the first heading, then 1, 2, …
    let mut scene_of: HashMap<ParaId, usize> = HashMap::new();
    let mut headings: Vec<String> = vec![];
    for (p, c) in this.paragraphs.iter().zip(&cx) {
        if c.heading {
            headings.push(c.scene.clone());
        }
        scene_of.insert(p.id, headings.len());
    }
    let count = headings.len();
    let q = q.trim();
    if q.is_empty() {
        return Err("--scene is empty: give a scene number or heading text".into());
    }
    let number = (!q.starts_with('0') && q.bytes().all(|b| b.is_ascii_digit()))
        .then(|| q.parse::<usize>().ok())
        .flatten()
        .filter(|n| (1..=count).contains(n));
    let want = match number {
        Some(n) => n,
        None => {
            let lower = q.to_lowercase();
            let found = headings.iter().position(|h| h.to_lowercase().contains(&lower));
            match found {
                Some(i) => i + 1,
                None if q.bytes().all(|b| b.is_ascii_digit()) => {
                    return Err(format!("no scene {q} (1 to {count}) and no scene heading contains {q:?}"));
                }
                None => return Err(format!("no scene heading contains {q:?}")),
            }
        }
    };
    let mut cur = 0;
    Ok(rows
        .iter()
        .map(|r| {
            if let Some(s) = r.cells[this_m].first().and_then(|id| scene_of.get(id)) {
                cur = *s;
            }
            cur == want
        })
        .collect())
}
