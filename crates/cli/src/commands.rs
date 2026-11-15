//! The `scriptorium` commands. Each returns the text it prints on stdout, or
//! an error message; things to tell the user besides (a review that can't be
//! read, a session that disagrees with a file) go to `notes`, which the
//! binary prints on stderr.
//!
//! Reviewing works on the same session as the app. Every command that
//! changes something opens the saved session (`<script>.scriptorium.json`),
//! applies one step through the same `scriptorium_review::Session` the app
//! uses, and saves it back (review files first, then the session). A session
//! that can't be replayed is never overwritten. Commands that only read never
//! write anything. A command that writes holds the script's [`SessionLock`]
//! from opening the session to saving it, so another program (the app,
//! another command) never writes it in between.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use scriptorium_core::outline::annotate;
use scriptorium_core::text::looks_same;
use scriptorium_core::{ElementKind, ParaId, Paragraph, Script};
use scriptorium_fdx::FdxFile;
use scriptorium_review::file::{FORMAT_HEADER, append_block, insert_id};
use scriptorium_review::matching::{Para, match_entries};
use scriptorium_review::sessionfile::{self, SessionFile};
use scriptorium_review::{Decision, Entry, Review, Session, Status, Unplaced};
use scriptorium_tools::{Ctx, Outcome, Target, item_numbered, item_numbers};
use serde::Serialize;
use serde_json::Value;

use crate::fsio::{SessionLock, write_atomic};
use crate::opened::{Opened, file_name, load, new_review_id, no_reviews, review_salt, script_dir};

pub const USAGE: &str = "\
scriptorium: screenplay tools

SCRIPTS
  scriptorium info <script.fdx>          summary: paragraphs, scenes, characters
  scriptorium dump <script.fdx> [--json] every paragraph with its id, type, scene and speaker
  scriptorium roundtrip <script.fdx>     check that a no-op save is byte-identical
  info and dump read the script as it is now in the saved session (with its
  decisions and edits); roundtrip reads the file.

REVIEWING
  These work on the same saved session as the app (<script>.scriptorium.json
  next to the script), so an agent and a person see each other's decisions.
  The session remembers which review files are attached to the script; a
  command uses those when you don't name one. Review files named on a command
  that saves (decide, edit, undo, redo, apply) are attached too.
  Suggestions are numbered from 1 in script order, as `suggestions` lists them.
  <review> below is a review file, or --review <name or file> to pick one of
  the attached reviews; it can be left out when only one is attached.

  scriptorium reviews <script.fdx>
        the attached review files, with progress
  scriptorium reviews <script.fdx> add <review.txt>...
  scriptorium reviews <script.fdx> remove <review.txt>...
        attach or detach review files (detaching keeps the text as it is)
  scriptorium review <script.fdx> [<review.txt>...]
        summary of each review: suggestions, decisions, lines not found
  scriptorium suggestions <script.fdx> [<review>] [--pending] [--json]
        list the suggestions, with status and current text
  scriptorium decide <script.fdx> [<review>] <n> accept [<option>]
  scriptorium decide <script.fdx> [<review>] <n> keep | undecide
  scriptorium decide <script.fdx> [<review>] <n> rewrite <text>
        decide suggestion n (option numbers start at 1); n can also be
        e<entry>, the suggestion's stable place in the review file
        (`entry` in `tool ... list_suggestions`)
  scriptorium edit <script.fdx> <paragraph id> <text> [-r <review.txt>]...
        replace one paragraph's text (ids from `dump`)
  scriptorium undo <script.fdx> [-r <review.txt>]...
  scriptorium redo <script.fdx> [-r <review.txt>]...
  scriptorium suggest <review.txt> <script.fdx> (--para <id> | --before <text>)
                      --after <text> [--after <text>]... [--why <text>]
        add a suggestion to a review file (created if missing); checks that
        the line exists and that the change would be visible
  scriptorium progress <script.fdx> [<review>] [-o <file>]
        the review file with every decision written in (STATUS/CHOSEN/MINE),
        to hand on or continue elsewhere; stdout unless -o
  scriptorium apply <script.fdx> [<review.txt>...] [--accept-all] [-o <out.fdx>]
        write the script with the session's decisions and edits
        (--accept-all: also take option 1 of every pending suggestion).
        Default output: \"<script> (scriptorium).fdx\" next to the script

EDITING STRUCTURE
  Each is one undoable step (`undo` reverses it), saved to the session; ids
  are from `dump`. TYPE is an element name; case, spaces and dashes don't matter (scene heading,
  scene-heading and SCENEHEADING all work): Scene Heading, Action,
  Character, Parenthetical, Dialogue, Transition, Shot, General. Lines inside
  a dual dialogue block take text edits only: these are refused there.
  With --json, each prints {\"new\":[ids],\"removed\":[ids],\"paras\":[{id,kind,text}]}.

  scriptorium insert <script.fdx> (--after <id> | --start | --end)
                     (--block '<TYPE: text>\\n<TYPE: text>...' | --guess '<plain text>'
                      | <TYPE> <text>) [--json]
        add paragraphs, one `TYPE: text` per line, in one step; --guess takes
        untyped screenplay text and guesses each line's type as the app does
        when pasting plain text (blank lines dropped)
  scriptorium remove <script.fdx> <id>[..<id>] [--json]
        remove a paragraph, or from the first to the second in script order
        (not a numeric range; refused if the second is above the first, and
        for the last remaining paragraph)
  scriptorium split <script.fdx> <id> (--at <text> | --offset <n>) [--json]
        split before the first occurrence of text, or after n characters;
        the original id keeps the original text
  scriptorium join <script.fdx> <first id> <second id> [--json]
        join the next paragraph onto the first
  scriptorium retype <script.fdx> <id> <TYPE> [--json]
  `apply` writes these into the .fdx as far as the writer supports them.

AGENT TOOLS
  scriptorium tool <script.fdx> <name> ['<json args>'] [--review-file <review.txt>]
        run one of the tools the built-in agent and the MCP server use:
        script_info, list_paragraphs, read_scene, read_script, list_reviews,
        list_suggestions, suggest_change, suggest_changes. Arguments are a
        JSON object, e.g. '{\"scene\": 2}'. Suggestions go to --review-file
        (created and attached if missing), else to the only attached review.
        The read-only tools (all but suggest_*) never write, and show the
        script as written if the saved session can't be used.
  scriptorium tool [<script.fdx>] --list [--json]
        the tools with a one-line description each; --json prints their
        full specs (name, description, JSON schema of the arguments)
";

/// Review commands, dispatched by [`run`].
const REVIEW_COMMANDS: &[&str] =
    &["review", "reviews", "suggestions", "decide", "edit", "undo", "redo", "suggest", "progress", "apply", "tool"];

/// Run the command line `args` (without the program name).
pub fn run(args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    match args {
        ["info", path] => info(path, notes),
        ["dump", path] => dump(path, false, notes),
        ["dump", path, "--json"] => dump(path, true, notes),
        ["roundtrip", path] => load(path).and_then(|f| roundtrip(&f)),
        [cmd, rest @ ..] if crate::structure::COMMANDS.contains(cmd) => crate::structure::command(cmd, rest, notes),
        [cmd, rest @ ..] if REVIEW_COMMANDS.contains(cmd) => review_command(cmd, rest, notes),
        ["-h" | "--help" | "help"] => Ok(USAGE.to_string()),
        _ => Err(format!("unrecognized arguments\n\n{USAGE}")),
    }
}

fn review_command(cmd: &str, args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    match (cmd, args) {
        ("review", [script, reviews @ ..]) => {
            let o = Opened::open(script, reviews, notes)?;
            if o.session.reviews.is_empty() {
                return Err(no_reviews(script));
            }
            Ok(summary(&o))
        }
        ("reviews", [script]) => list_reviews(script, notes),
        ("reviews", [script, "add", files @ ..]) if !files.is_empty() => add_reviews(script, files, notes),
        ("reviews", [script, "remove", files @ ..]) if !files.is_empty() => remove_reviews(script, files),
        ("suggestions", [script, rest @ ..]) => suggestions(script, rest, notes),
        ("decide", [script, rest @ ..]) => decide_command(script, rest, notes),
        ("edit", [script, id, text, rest @ ..]) => {
            let para = id.parse::<u64>().map_err(|_| format!("paragraph id must be a number, got {id:?}"))?;
            edit(script, para, text, &review_flags(rest)?, notes)
        }
        ("undo" | "redo", [script, rest @ ..]) => undo_redo(script, cmd == "redo", &review_flags(rest)?, notes),
        ("suggest", [review, script, rest @ ..]) => suggest(review, script, rest, notes),
        ("progress", [script, rest @ ..]) => progress(script, rest, notes),
        ("apply", [script, rest @ ..]) => apply_command(script, rest, notes),
        ("tool", rest) => tool_command(rest, notes),
        _ => Err(format!("wrong arguments for `{cmd}`\n\n{USAGE}")),
    }
}

fn info(path: &str, notes: &mut Vec<String>) -> Result<String, String> {
    let o = Opened::open_read_only(path, notes)?;
    Ok(info_text(o.doc.script()))
}

fn info_text(script: &Script) -> String {
    let ctx = annotate(script);
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut speakers: BTreeMap<String, usize> = BTreeMap::new();
    for (p, c) in script.paragraphs.iter().zip(&ctx) {
        *kinds.entry(p.kind.name().to_string()).or_default() += 1;
        if !c.speaker.is_empty() && p.kind.name() == "Dialogue" {
            *speakers.entry(c.speaker.clone()).or_default() += 1;
        }
    }
    let words: usize = script.paragraphs.iter().map(|p| p.text().split_whitespace().count()).sum();
    let mut out = String::new();
    let _ = writeln!(out, "paragraphs  {}", script.paragraphs.len());
    let _ = writeln!(out, "scenes      {}", ctx.iter().filter(|c| c.heading).count());
    let _ = writeln!(out, "words       {words}");
    let _ = writeln!(out, "\nelements");
    for (k, n) in &kinds {
        let _ = writeln!(out, "  {n:>5}  {k}");
    }
    let mut top: Vec<_> = speakers.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let _ = writeln!(out, "\ncharacters (dialogue paragraphs)");
    for (name, n) in top.iter().take(15) {
        let _ = writeln!(out, "  {n:>5}  {name}");
    }
    out
}

#[derive(Serialize)]
struct Row<'a> {
    id: u64,
    kind: &'a str,
    scene: &'a str,
    speaker: &'a str,
    text: String,
}

fn dump(path: &str, json: bool, notes: &mut Vec<String>) -> Result<String, String> {
    // JSON is read by programs (MCP parses it along with stderr): no notes.
    let mut dropped = vec![];
    let o = Opened::open_read_only(path, if json { &mut dropped } else { notes })?;
    let script = o.doc.script();
    let ctx = annotate(script);
    let rows: Vec<Row> = script
        .paragraphs
        .iter()
        .zip(&ctx)
        .map(|(p, c)| Row { id: p.id.0, kind: p.kind.name(), scene: &c.scene, speaker: &c.speaker, text: p.text() })
        .collect();
    if json {
        return Ok(serde_json::to_string_pretty(&rows).expect("rows serialize") + "\n");
    }
    let mut out = String::new();
    for r in rows {
        let _ = writeln!(out, "{:>5}  {:<14} {}", r.id, r.kind, r.text);
    }
    Ok(out)
}

fn roundtrip(f: &FdxFile) -> Result<String, String> {
    let out = f.write_script(&f.to_script()).map_err(|e| e.to_string())?;
    if out == f.raw() {
        Ok(format!("ok: no-op save is byte-identical ({} bytes, {} paragraphs)\n", out.len(), f.paragraphs().len()))
    } else {
        Err("no-op save changed the file".into())
    }
}

/// `-r <review.txt>` pairs.
fn review_flags<'a>(rest: &[&'a str]) -> Result<Vec<&'a str>, String> {
    let mut out = vec![];
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match *a {
            "-r" => out.push(*it.next().ok_or("-r needs a review file")?),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    Ok(out)
}

/// Takes `--review <name or file>` out of `args`.
fn take_review_flag<'a>(args: &[&'a str]) -> Result<(Option<&'a str>, Vec<&'a str>), String> {
    let (mut pick, mut rest) = (None, vec![]);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--review" => pick = Some(*it.next().ok_or("--review needs a review name or file")?),
            other => rest.push(other),
        }
    }
    Ok((pick, rest))
}

fn list_reviews(script: &str, notes: &mut Vec<String>) -> Result<String, String> {
    let o = Opened::open(script, &[], notes)?;
    let mut out = String::new();
    if o.session.reviews.is_empty() {
        let _ = writeln!(out, "no review files attached (add one: scriptorium reviews {script:?} add <review.txt>)");
    }
    for (r, review) in o.session.reviews.iter().enumerate() {
        let p = o.session.progress(&o.doc, r);
        let _ = writeln!(
            out,
            "{}  {} of {} decided ({} accepted, {} kept, {} rewritten, {} pending)",
            review.path,
            p.done(),
            p.total,
            p.accepted,
            p.rejected,
            p.rewritten,
            p.pending
        );
    }
    Ok(out)
}

/// Attach review files (paths from the current folder) to `script`, saving
/// the session; the result lists each with its suggestion count.
pub fn add_reviews(script: &str, files: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let _lock = lock(script)?;
    let o = Opened::open(script, files, notes)?;
    o.save()?;
    let mut out = String::new();
    for &r in &o.given {
        let review = &o.session.reviews[r];
        let _ = writeln!(out, "attached {} ({} suggestions)", review.path, review.items.len());
        if !review.unmatched.is_empty() {
            let _ = writeln!(
                out,
                "  {} suggestions didn't match a line (`scriptorium review {script:?}` lists them)",
                review.unmatched.len()
            );
        }
        for w in &review.warnings {
            let _ = writeln!(out, "  warning: {w}");
        }
    }
    Ok(out)
}

/// Detach review files (as attached, or paths from the current folder).
pub fn remove_reviews(script: &str, files: &[&str]) -> Result<String, String> {
    let _lock = lock(script)?;
    let dir = script_dir(script)?;
    let session_path = dir.join(sessionfile::file_name_for(&file_name(Path::new(script))));
    let json = std::fs::read_to_string(&session_path).map_err(|_| format!("no review files attached to {script}"))?;
    let mut file = SessionFile::from_json(&json).map_err(|e| format!("{}: {e}", session_path.display()))?;
    let mut out = String::new();
    for f in files {
        let at = Path::new(f).canonicalize().unwrap_or_else(|_| dir.join(f));
        let before = file.reviews.len();
        file.reviews.retain(|a| {
            let p = sessionfile::resolve_path(&dir, &a.path);
            a.path != *f && p.canonicalize().unwrap_or(p) != at
        });
        if file.reviews.len() == before {
            return Err(format!("{f} isn't attached to {script} (list_reviews shows what is)"));
        }
        let _ = writeln!(out, "removed {f} (the script's text keeps its decisions)");
    }
    write_atomic(&session_path, file.to_json().as_bytes()).map_err(|e| format!("{}: {e}", session_path.display()))?;
    Ok(out)
}

fn status_name(s: Status) -> String {
    match s {
        Status::Pending => "pending".into(),
        Status::Accepted(k) => format!("accepted {}", k + 1),
        Status::Rejected => "kept".into(),
        Status::Rewritten => "rewritten".into(),
    }
}

fn summary(o: &Opened) -> String {
    let mut out = String::new();
    for (r, review) in o.session.reviews.iter().enumerate() {
        let p = o.session.progress(&o.doc, r);
        let fuzzy = review.items.iter().filter(|i| i.fuzzy).count();
        let _ = writeln!(out, "{}", review.name);
        let _ = writeln!(out, "  suggestions   {} ({fuzzy} matched approximately)", p.total);
        let _ = writeln!(
            out,
            "  decided       {} accepted, {} kept, {} rewritten, {} pending",
            p.accepted, p.rejected, p.rewritten, p.pending
        );
        if !review.invisible.is_empty() {
            let _ = writeln!(
                out,
                "  skipped       {} that change nothing visible (e.g. only letter case in a character name)",
                review.invisible.len()
            );
        }
        if !review.unmatched.is_empty() {
            let _ = writeln!(out, "  not found     {}", review.unmatched.len());
            for u in review.unmatched.iter().take(10) {
                let _ = writeln!(out, "    block {}: {}", u.entry + 1, u.before);
            }
        }
        for w in &review.warnings {
            let _ = writeln!(out, "  warning: {w}");
        }
    }
    out
}

#[derive(Serialize)]
struct SuggestionRow {
    n: usize,
    paragraph: u64,
    kind: String,
    scene: String,
    speaker: String,
    status: String,
    /// Pending, but the line changed since the suggestion was written.
    outdated: bool,
    fuzzy: bool,
    current: String,
    before: String,
    options: Vec<String>,
    why: String,
}

fn suggestions(script: &str, args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let (pick, rest) = take_review_flag(args)?;
    let (mut pending_only, mut json, mut file) = (false, false, None);
    for f in rest {
        match f {
            "--pending" => pending_only = true,
            "--json" => json = true,
            other if other.starts_with('-') || file.is_some() => {
                return Err(format!("unexpected argument {other:?}"));
            }
            review => file = Some(review),
        }
    }
    let (o, r) = Opened::open_one(script, file, pick, true, notes)?;
    let ctx = annotate(o.doc.script());
    let items = &o.session.reviews[r].items;
    let numbers = item_numbers(&o.doc, items);
    let mut rows: Vec<SuggestionRow> = items
        .iter()
        .enumerate()
        .filter(|(i, _)| !pending_only || o.session.status(&o.doc, r, *i) == Status::Pending)
        .map(|(i, it)| {
            let ix = o.doc.index_of(it.para).unwrap_or(it.index);
            SuggestionRow {
                n: numbers[i],
                paragraph: it.para.0,
                kind: o.doc.paragraphs()[ix].kind.name().to_string(),
                scene: ctx[ix].scene.clone(),
                speaker: ctx[ix].speaker.clone(),
                status: status_name(o.session.status(&o.doc, r, i)),
                outdated: o.session.is_outdated(&o.doc, r, i),
                fuzzy: it.fuzzy,
                current: o.text(it.para),
                before: it.before.clone(),
                options: it.options.clone(),
                why: it.why.clone(),
            }
        })
        .collect();
    rows.sort_by_key(|row| row.n);
    if json {
        return Ok(serde_json::to_string_pretty(&rows).expect("rows serialize") + "\n");
    }
    let mut out = String::new();
    for r in rows {
        let flags = [(r.outdated, " (line changed since)"), (r.fuzzy, " (matched approximately)")]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, t)| *t)
            .collect::<String>();
        let _ = writeln!(out, "#{} ¶{} {}{flags}", r.n, r.paragraph, r.status);
        let _ = writeln!(out, "  now: {}", r.current);
        for (k, opt) in r.options.iter().enumerate() {
            let _ = writeln!(out, "  {}: {opt}", k + 1);
        }
        if !r.why.is_empty() {
            let _ = writeln!(out, "  why: {}", r.why);
        }
    }
    Ok(out)
}

/// `e<entry>`: a suggestion by its stable place in the review file.
fn entry_ref(s: &str) -> Option<usize> {
    s.strip_prefix('e').and_then(|d| d.parse::<usize>().ok())
}

/// Which suggestion [`decide`] decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionRef {
    /// Its number `n` (from 1, in script order, as `suggestions` lists them).
    Number(usize),
    /// `e<entry>`: its stable place in the review file.
    Entry(usize),
}

impl SuggestionRef {
    /// `3` or `e3`.
    pub fn parse(s: &str) -> Option<Self> {
        match entry_ref(s) {
            Some(e) => Some(Self::Entry(e)),
            None => s.parse::<usize>().ok().map(Self::Number),
        }
    }
}

/// What [`decide`] does with a suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Take an option, numbered from 1 (`None`: the first).
    Accept(Option<usize>),
    /// Keep the line as it is.
    Keep,
    /// Replace the line with this text.
    Rewrite(String),
    /// Take the decision back.
    Undecide,
}

/// `decide <script> [<review>] [--review <r>] <n> <decision...>`.
fn decide_command(script: &str, args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let (pick, rest) = take_review_flag(args)?;
    // `<review.txt> <n> ...` or just `<n> ...`.
    let (file, n, decision) = match rest.as_slice() {
        [n, decision @ ..] if SuggestionRef::parse(n).is_some() => (None, *n, decision),
        [file, n, decision @ ..] => (Some(*file), *n, decision),
        _ => return Err(format!("decide which suggestion?\n\n{USAGE}")),
    };
    let n = SuggestionRef::parse(n).ok_or(format!("no suggestion {n}: give its number, or e<entry>"))?;
    let choice = match decision {
        ["accept"] => Choice::Accept(None),
        ["accept", k] => Choice::Accept(Some(k.parse::<usize>().map_err(|_| format!("no option {k}"))?)),
        ["keep"] => Choice::Keep,
        ["undecide"] => Choice::Undecide,
        ["rewrite", text] => Choice::Rewrite(text.to_string()),
        _ => return Err(format!("decide what? accept [<option>] | keep | undecide | rewrite <text>\n\n{USAGE}")),
    };
    decide_in(script, file, pick, true, n, choice, notes)
}

/// Decide one suggestion of `review` (an attached review's name or file, or
/// a review file, absolute or from the script's folder; never from the
/// current folder; `None`: the only attached review), and save. Returns the suggestion's new status and line.
pub fn decide(
    script: &str,
    review: Option<&str>,
    n: SuggestionRef,
    choice: Choice,
    notes: &mut Vec<String>,
) -> Result<String, String> {
    decide_in(script, None, review, false, n, choice, notes)
}

fn decide_in(
    script: &str,
    file: Option<&str>,
    pick: Option<&str>,
    from_cwd: bool,
    n: SuggestionRef,
    choice: Choice,
    notes: &mut Vec<String>,
) -> Result<String, String> {
    let _lock = lock(script)?;
    let (mut o, r) = Opened::open_one(script, file, pick, from_cwd, notes)?;
    let count = o.session.reviews[r].items.len();
    let i = match n {
        SuggestionRef::Entry(e) => o.session.reviews[r].items.iter().position(|it| it.entry == e).ok_or(format!(
            "no suggestion e{e} in {} (it isn't in the file, or didn't match a line)",
            o.session.reviews[r].name
        ))?,
        SuggestionRef::Number(n) => {
            item_numbered(&o.doc, &o.session.reviews[r].items, n).ok_or(format!("no suggestion {n} (1 to {count})"))?
        }
    };
    let options = o.session.item(r, i).options.len();
    let d = match choice {
        Choice::Accept(None) => Decision::Accept(0),
        Choice::Accept(Some(k)) if (1..=options).contains(&k) => Decision::Accept(k - 1),
        Choice::Accept(Some(k)) => return Err(format!("no option {k} (1 to {options})")),
        Choice::Keep => Decision::KeepMine,
        Choice::Undecide => Decision::Undecided,
        Choice::Rewrite(text) => Decision::Rewrite(text),
    };
    let sel = o.sel();
    o.session.decide(&mut o.doc, sel, r, i, d).map_err(|e| e.to_string())?;
    o.save()?;
    let item = o.session.item(r, i);
    let n = item_numbers(&o.doc, &o.session.reviews[r].items)[i];
    Ok(format!("#{n} {}: {}\n", status_name(o.session.status(&o.doc, r, i)), o.text(item.para)))
}

/// Replace paragraph `para`'s whole text, and save. `reviews` (paths from the
/// current folder) are attached too.
pub fn edit(script: &str, para: u64, text: &str, reviews: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let _lock = lock(script)?;
    let mut o = Opened::open(script, reviews, notes)?;
    let id = para;
    let para = ParaId(para);
    if o.doc.paragraph(para).is_none() {
        return Err(format!("no paragraph {id} (use list_paragraphs to find ids)"));
    }
    crate::structure::check_text(text)?;
    let sel = o.sel();
    o.doc.replace_text(sel, para, text, &[]).map_err(|e| e.to_string())?;
    o.save()?;
    Ok(format!("¶{id}: {}\n", o.text(para)))
}

/// Undo (or redo) the last change to the script, and save. `reviews` (paths
/// from the current folder) are attached too.
pub fn undo_redo(script: &str, redo: bool, reviews: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let _lock = lock(script)?;
    let mut o = Opened::open(script, reviews, notes)?;
    let paras = o.doc.next_undo_paras(redo);
    let done = if redo { o.doc.redo() } else { o.doc.undo() };
    if done.is_none() {
        return Err(if redo { "nothing to redo".into() } else { "nothing to undo".into() });
    }
    o.save()?;
    let mut out = String::new();
    for p in paras {
        let _ = writeln!(out, "¶{}: {}", p.0, o.text(p));
    }
    Ok(out)
}

fn suggest(review: &str, script: &str, rest: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let (mut para, mut before, mut after, mut why) = (None, None, vec![], String::new());
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().map(|v| v.to_string()).ok_or(format!("{a} needs a value"));
        match *a {
            "--para" => para = Some(val()?),
            "--before" => before = Some(val()?),
            "--after" => after.push(val()?),
            "--why" => why = val()?,
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    if after.is_empty() {
        return Err("give at least one --after".into());
    }
    // Matched and numbered against the live document (the saved session), as
    // every other command sees the script; the session is only saved if the
    // review file is attached (to record its new text).
    let _lock = lock(script)?;
    let mut o = Opened::open(script, &[], notes)?;
    let fdx = &o.fdx;
    let caps = |k: &ElementKind| fdx.is_caps(k.name());
    let p = match (para, before) {
        (Some(id), None) => {
            let id = ParaId(id.parse::<u64>().map_err(|_| format!("--para must be a number, got {id:?}"))?);
            o.doc.paragraph(id).ok_or(format!("no paragraph {} (see `scriptorium dump`)", id.0))?.clone()
        }
        (None, Some(b)) => {
            let texts: Vec<String> = o.doc.paragraphs().iter().map(|p| p.text()).collect();
            let paras: Vec<Para> = o
                .doc
                .paragraphs()
                .iter()
                .zip(&texts)
                .map(|(p, t)| Para { id: p.id, text: t, editable: p.kind.is_editable(), caps: caps(&p.kind) })
                .collect();
            let probe = Entry { before: b.clone(), options: after.clone(), ..Entry::default() };
            match match_entries(&paras, &[probe]).pop().flatten() {
                Some(m) => o.doc.paragraphs()[m.index].clone(),
                None => return Err(format!("no line in the script matches {b:?}")),
            }
        }
        _ => return Err("give exactly one of --para or --before".into()),
    };
    let text = p.text();
    if !p.kind.is_editable() {
        return Err(format!("paragraph {} ({}) isn't reviewable", p.id.0, p.kind.name()));
    }
    if after.iter().all(|a| looks_same(a, &text, caps(&p.kind))) {
        return Err("that change wouldn't be visible in Final Draft".into());
    }
    let path = Path::new(review);
    let entry = Entry { before: text.trim().to_string(), para: Some(p.id), options: after, why, ..Entry::default() };
    let stored = path.canonicalize().ok().map(|at| sessionfile::stored_path(&o.dir, &at));
    let attached = stored.and_then(|st| o.session.reviews.iter().position(|r| r.path == st));
    // The attached review, or the file opened on its own (not attached here).
    let (mut lone, r) = match attached {
        Some(r) => (None, r),
        None => {
            let t = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(_) => {
                    let header = format!("{FORMAT_HEADER}\nScript: {}\n\n", file_name(Path::new(script)));
                    let id = new_review_id(&append_block(&header, &entry, ""), &review_salt(path));
                    insert_id(&header, &id)
                }
            };
            let original = o.fdx.to_script();
            let review = Review::open_live(review, &t, &original, &o.doc, caps, "");
            (Some(Session::new(vec![review])), 0)
        }
    };
    let session = lone.as_mut().unwrap_or(&mut o.session);
    // Spliced onto the file (never reserialized), on exactly this paragraph.
    let many = session.append_entries(&o.doc, r, vec![entry], true, caps)?;
    let result = many.results[0];
    let Some(i) = result.item else {
        return Err(match result.unplaced {
            Some(Unplaced::Taken { .. }) => {
                format!("not added: another suggestion in {review} already covers ¶{}", p.id.0)
            }
            Some(Unplaced::Invisible) => "that change wouldn't be visible in Final Draft".into(),
            _ => format!("not added: ¶{} can't take a suggestion", p.id.0),
        });
    };
    // Numbered as `suggestions` and `decide` will, by position in the script.
    let n = item_numbers(&o.doc, &session.reviews[r].items)[i];
    let is_lone = lone.is_some();
    o.write_review(path, &many.file_text).map_err(|e| format!("{review}: {e}"))?;
    if !is_lone {
        o.save()?;
    }
    Ok(format!("added suggestion #{n} (¶{}) to {review}\n", p.id.0))
}

fn progress(script: &str, args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let (pick, rest) = take_review_flag(args)?;
    let (file, rest) = match rest.as_slice() {
        [f, rest @ ..] if !f.starts_with('-') => (Some(*f), rest.to_vec()),
        rest => (None, rest.to_vec()),
    };
    // Only `-o` can write the review file and the session.
    let _lock = if rest.is_empty() { None } else { Some(lock(script)?) };
    let (mut o, r) = Opened::open_one(script, file, pick, true, notes)?;
    let out = o.session.save_progress(&o.doc, r);
    match rest.as_slice() {
        [] => Ok(out),
        ["-o", file] => {
            let own = o.review_path(r);
            let over_itself =
                own.canonicalize().ok().is_some_and(|own| Path::new(file).canonicalize().ok() == Some(own));
            if over_itself {
                // Through the session, which records the file's new text, so
                // its STATUS lines aren't taken for a change made elsewhere.
                // Checked before writing, so a refusal leaves the file alone.
                let mut review = o.session.reviews[r].clone();
                review.wrote_progress(out.clone())?;
                o.write_review(Path::new(file), &out)?;
                o.session.reviews[r] = review;
                o.save()?;
            } else {
                write_atomic(Path::new(file), out.as_bytes()).map_err(|e| format!("{file}: {e}"))?;
            }
            Ok(format!("wrote {file}\n"))
        }
        _ => Err(format!("unexpected arguments {rest:?}")),
    }
}

fn apply_command(script: &str, rest: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let (mut reviews, mut accept_all, mut out) = (vec![], false, None);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match *a {
            "--accept-all" => accept_all = true,
            "-o" => out = Some(Path::new(*it.next().ok_or("-o needs a file name")?)),
            r => reviews.push(r),
        }
    }
    apply(script, &reviews, accept_all, out, notes)
}

/// Write the script with the session's decisions and edits to `output`
/// (default "<script> (scriptorium).fdx" next to it, replaced if it exists);
/// never over the script itself. `accept_all` also takes option 1 of every
/// pending suggestion, in this output only. `reviews` (paths from the
/// current folder) are attached first.
pub fn apply(
    script: &str,
    reviews: &[&str],
    accept_all: bool,
    output: Option<&Path>,
    notes: &mut Vec<String>,
) -> Result<String, String> {
    let _lock = lock(script)?;
    let mut o = Opened::open(script, reviews, notes)?;
    // Review files named here are attached, as on every command that takes
    // them. Saved before accept_all, which only shapes this output.
    if !o.given.is_empty() {
        o.save()?;
    }
    if accept_all {
        let sel = o.sel();
        for r in 0..o.session.reviews.len() {
            for i in 0..o.session.reviews[r].items.len() {
                if o.session.status(&o.doc, r, i) == Status::Pending {
                    o.session.decide(&mut o.doc, sel, r, i, Decision::Accept(0)).map_err(|e| e.to_string())?;
                }
            }
        }
    }
    let input = Path::new(script);
    let out = output.map(Path::to_path_buf).unwrap_or_else(|| output_path(input));
    if out.canonicalize().ok() == input.canonicalize().ok() {
        return Err("refusing to overwrite the original script; give an output with another name".into());
    }
    let raw = o.fdx.write_script(o.doc.script()).map_err(|e| e.to_string())?;
    let original = o.fdx.to_script();
    let changed = changed_paragraphs(&original.paragraphs, o.doc.paragraphs());
    write_atomic(&out, raw.as_bytes()).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(format!("wrote {} ({changed} paragraphs changed)\n", out.display()))
}

/// Paragraphs that differ between the original and the edited script, by id:
/// inserted, removed, and those whose text or kind changed.
fn changed_paragraphs(original: &[Paragraph], edited: &[Paragraph]) -> usize {
    let old: std::collections::HashMap<_, _> = original.iter().map(|p| (p.id, p)).collect();
    let new: std::collections::HashSet<_> = edited.iter().map(|p| p.id).collect();
    let removed = original.iter().filter(|p| !new.contains(&p.id)).count();
    let rest =
        edited.iter().filter(|p| old.get(&p.id).is_none_or(|o| o.text() != p.text() || o.kind != p.kind)).count();
    removed + rest
}

/// The session lock of `script`, for a command that writes (see the module
/// docs); a script that isn't there fails as opening it would.
/// The script's session lock; None (with a note on stderr) when the file
/// system has no locks, as the app goes on. Busy past the wait is an error.
pub(crate) fn lock(script: &str) -> Result<Option<SessionLock>, String> {
    let at = Path::new(script).canonicalize().map_err(|e| format!("{script}: {e}"))?;
    match SessionLock::for_script(&at) {
        Ok(l) => Ok(Some(l)),
        Err(e) if e == crate::fsio::NO_LOCKS => {
            eprintln!("note: {e}");
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// "<name> (scriptorium).fdx" next to the script, as the app writes it.
fn output_path(script: &Path) -> PathBuf {
    let stem = script.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    script.with_file_name(format!("{stem} (scriptorium).fdx"))
}

/// Where a review file named on the command line is (or will be): canonical,
/// so its stored form matches the session's even before the file exists.
fn review_location(file: &str) -> Result<PathBuf, String> {
    let p = Path::new(file);
    if let Ok(at) = p.canonicalize() {
        return Ok(at);
    }
    let abs = std::path::absolute(p).map_err(|e| format!("{file}: {e}"))?;
    let name = abs.file_name().ok_or(format!("{file}: not a file name"))?;
    let parent = abs.parent().ok_or(format!("{file}: not a file name"))?;
    Ok(parent.canonicalize().map_err(|e| format!("{file}: {e}"))?.join(name))
}

/// The tools that only read: run on a read-only open, so a session that
/// can't be used is a note (the script as written), as for `info` and `dump`.
const READ_ONLY_TOOLS: &[&str] =
    &["script_info", "list_paragraphs", "read_scene", "read_script", "list_reviews", "list_suggestions"];

/// The tool specs `tool` accepts: the agent's catalogue, plus `before` on
/// `suggest_change` and `review_file` on the suggesting tools.
pub fn tool_specs() -> Vec<scriptorium_tools::ToolSpec> {
    let mut specs = scriptorium_tools::catalogue();
    for spec in &mut specs {
        if spec.name == "suggest_change" {
            scriptorium_tools::add_before(spec);
        }
        scriptorium_tools::add_host_fields(spec, &[scriptorium_tools::HostField::ReviewFile]);
    }
    specs
}

/// `tool --list [--json]`: each tool's name and the first line of its
/// description, or the full specs as JSON.
fn tool_list(json: bool) -> String {
    let specs = tool_specs();
    if json {
        return serde_json::to_string_pretty(&specs).expect("specs serialize") + "\n";
    }
    let width = specs.iter().map(|s| s.name.len()).max().unwrap_or(0);
    let mut out = String::new();
    for s in &specs {
        let line = s.description.lines().next().unwrap_or("");
        let first = match line.find(". ") {
            Some(end) => &line[..=end],
            None => line,
        };
        let _ = writeln!(out, "{:<width$}  {}", s.name, first.trim());
    }
    out
}

/// `scriptorium tool ...`: parses the command line and runs [`tool`], or
/// lists the tools.
fn tool_command(args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    if args.contains(&"--list") {
        // The script, if given, isn't needed: every script has the same tools.
        let json = args.contains(&"--json");
        let rest: Vec<&str> = args.iter().copied().filter(|a| !matches!(*a, "--list" | "--json")).collect();
        if rest.len() > 1 || rest.first().is_some_and(|a| a.starts_with('-')) {
            return Err(format!("unexpected arguments with --list: {rest:?}"));
        }
        return Ok(tool_list(json));
    }
    let [script, rest @ ..] = args else { return Err(format!("wrong arguments for `tool`\n\n{USAGE}")) };
    let (mut name, mut json, mut review_file) = (None, None, None);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match *a {
            "--review-file" => review_file = Some(*it.next().ok_or("--review-file needs a file")?),
            other if name.is_none() => name = Some(other),
            other if json.is_none() => json = Some(other),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    let name = name.ok_or(format!("which tool?\n\n{USAGE}"))?;
    let args: Value = match json {
        Some(j) => serde_json::from_str(j).map_err(|e| format!("the tool arguments aren't JSON: {e}"))?,
        None => Value::Object(Default::default()),
    };
    tool(script, name, &args, review_file, notes)
}

/// Run one `scriptorium_tools` tool on the live document of `script` and
/// carry out what it asks (the host contract: the tool never writes; here
/// the review file is written first, then the session). `args` is the
/// tool's JSON arguments; `review_file` (or `review_file` in `args`) is
/// where suggestions go, as a path from the current folder, created and
/// attached if missing; else they go to the only attached review. Read-only
/// tools never write, and a saved session that can't be used is a note for
/// them rather than an error. Returns the tool result, ending in a newline.
pub fn tool(
    script: &str,
    name: &str,
    args: &Value,
    review_file: Option<&str>,
    notes: &mut Vec<String>,
) -> Result<String, String> {
    let mut args = args.clone();
    let _lock = if READ_ONLY_TOOLS.contains(&name) { None } else { Some(lock(script)?) };
    let mut o = if READ_ONLY_TOOLS.contains(&name) {
        Opened::open_read_only(script, notes)?
    } else {
        Opened::open(script, &[], notes)?
    };
    // `review_file` (the parameter, or in the JSON): given as a path from
    // here, passed on in the form the session stores, so `find_review` sees
    // an attached file as attached.
    let from_json = args.get("review_file").and_then(Value::as_str).map(str::to_string);
    let review_at = match review_file.map(str::to_string).or(from_json) {
        Some(f) => {
            let at = review_location(&f)?;
            let map = args.as_object_mut().ok_or("the tool arguments must be a JSON object")?;
            map.insert("review_file".into(), Value::String(sessionfile::stored_path(&o.dir, &at)));
            Some(at)
        }
        None => None,
    };
    let outcome = {
        let fdx = &o.fdx;
        let caps = |k: &ElementKind| fdx.is_caps(k.name());
        let ctx = Ctx {
            doc: &o.doc,
            session: &o.session,
            script_name: &o.script_name,
            is_caps: &caps,
            default_review: (o.session.reviews.len() == 1).then_some(0),
        };
        scriptorium_tools::call(&ctx, name, &args).map_err(|e| e.to_string())?
    };
    let append = match outcome {
        Outcome::Text(t) => return Ok(t + "\n"),
        Outcome::Append(a) => a,
    };
    // Never reached for a read-only open (only suggesting tools append), but
    // a session discarded by one must never be saved over.
    if READ_ONLY_TOOLS.contains(&name) {
        return Err(format!("{name} can't change anything"));
    }
    // The review it goes into, and whether it was attached before this run
    // (only then can its file have changed since it was read).
    let (mut r, was_attached) = match &append.target {
        Target::Own => {
            return Err(
                "no review to add to (several or none are attached): give review_file, the review file to add to"
                    .into(),
            );
        }
        Target::Review { index, .. } => (*index, true),
        Target::File(stored) => match scriptorium_tools::find_review(&o.session, stored) {
            Some(r) => (r, true),
            None => {
                let at = review_at.clone().expect("a file target comes from review_file");
                if at.exists() {
                    // Attached the way `reviews add` does (its saved
                    // decisions applied, its `Id:` pinned).
                    let at_str = at.to_string_lossy().to_string();
                    let mut more = vec![];
                    o = Opened::open(script, &[&at_str], &mut more)?;
                    notes.extend(more.into_iter().filter(|n| !notes.contains(n)).collect::<Vec<_>>());
                    (o.given[0], false)
                } else {
                    let header = format!("{FORMAT_HEADER}\nScript: {}\n\n", o.script_name);
                    let text = insert_id(&header, &new_review_id(&header, &review_salt(&at)));
                    let original = o.fdx.to_script();
                    let fdx = &o.fdx;
                    let caps = |k: &ElementKind| fdx.is_caps(k.name());
                    let review = Review::open_live(stored.clone(), &text, &original, &o.doc, caps, &o.script_name)
                        .with_path(stored.clone());
                    (o.session.add(review)?, false)
                }
            }
        },
    };
    // Another process may have written the file since it was read: then
    // start again from what's on disk (apply re-validates against it).
    if was_attached {
        let stored = o.session.reviews[r].path.clone();
        let disk = std::fs::read_to_string(o.review_path(r)).ok();
        if disk.is_some_and(|d| d != o.session.reviews[r].text()) {
            let mut more = vec![];
            o = Opened::open(script, &[], &mut more)?;
            notes.extend(more.into_iter().filter(|n| !notes.contains(n)).collect::<Vec<_>>());
            r = scriptorium_tools::find_review(&o.session, &stored)
                .ok_or(format!("{stored} was detached meanwhile; run the tool again"))?;
        }
    }
    let fdx = &o.fdx;
    let caps = |k: &ElementKind| fdx.is_caps(k.name());
    let applied = scriptorium_tools::apply(&mut o.session, &o.doc, r, append, &caps).map_err(|e| e.to_string())?;
    if let Some(text) = &applied.file_text {
        let path = o.review_path(r);
        o.write_review(&path, text)?;
        o.save()?;
    }
    Ok(applied.message + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_paragraphs_count_by_id() {
        let p = |id, k: ElementKind, t: &str| Paragraph::plain(ParaId(id), k, t);
        let a = ElementKind::Action;
        let original = [p(0, a.clone(), "x"), p(1, a.clone(), "y"), p(2, a.clone(), "z"), p(3, a.clone(), "w")];
        // 0 same, 1 removed, 2 retyped, 3 retexted, 9 inserted.
        let edited = [p(0, a.clone(), "x"), p(9, a.clone(), "n"), p(2, ElementKind::Shot, "z"), p(3, a, "w2")];
        assert_eq!(changed_paragraphs(&original, &edited), 4);
        assert_eq!(changed_paragraphs(&original, &original), 0);
    }
}
