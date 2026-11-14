//! Reviewing from the command line: the same session as the app.
//!
//! Every command that changes something opens the saved session
//! (`<script>.scriptorium.json`), applies one step through the same
//! `scriptorium_review::Session` the app uses, and saves it back. A session
//! that can't be replayed is never overwritten.

use std::path::{Path, PathBuf};

use scriptorium_core::outline::annotate;
use scriptorium_core::text::looks_same;
use scriptorium_core::{Document, ParaId, Selection};
use scriptorium_fdx::FdxFile;
use scriptorium_review::file::FORMAT_HEADER;
use scriptorium_review::sessionfile::{self, Resumed, SessionFile};
use scriptorium_review::{Decision, Entry, Review, ReviewFile, Session, Status};
use serde::Serialize;

use crate::{USAGE, load};

pub const COMMANDS: &[&str] =
    &["review", "suggestions", "decide", "edit", "undo", "redo", "suggest", "progress", "apply"];

pub fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    match (cmd, args) {
        ("review", [script, reviews @ ..]) if !reviews.is_empty() => {
            let o = Opened::open(script, reviews)?;
            summary(&o);
            Ok(())
        }
        ("suggestions", [script, review, flags @ ..]) => suggestions(script, review, flags),
        ("decide", [script, review, n, decision @ ..]) => decide(script, review, n, decision),
        ("edit", [script, id, text, rest @ ..]) => edit(script, id, text, &review_flags(rest)?),
        ("undo" | "redo", [script, rest @ ..]) => undo(script, cmd == "redo", &review_flags(rest)?),
        ("suggest", [review, script, rest @ ..]) => suggest(review, script, rest),
        ("progress", [script, review, rest @ ..]) => progress(script, review, rest),
        ("apply", [script, rest @ ..]) => apply(script, rest),
        _ => Err(format!("wrong arguments for `{cmd}`\n\n{USAGE}")),
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

/// A script, its review files and the saved session, opened like the app does.
struct Opened {
    fdx: FdxFile,
    raw: String,
    session: Session,
    doc: Document,
    session_path: PathBuf,
}

impl Opened {
    fn open(script: &str, reviews: &[&str]) -> Result<Self, String> {
        let fdx = load(script)?;
        let raw = fdx.raw().to_string();
        let script_name = file_name(Path::new(script));
        let s = fdx.to_script();
        let mut opened = vec![];
        for r in reviews {
            let text = std::fs::read_to_string(r).map_err(|e| format!("{r}: {e}"))?;
            opened.push(Review::open(*r, &text, &s, |k| fdx.is_caps(k.name()), &script_name));
        }
        let session = Session::new(opened);
        let session_path = Path::new(script).with_file_name(sessionfile::file_name_for(&script_name));
        let saved = std::fs::read_to_string(&session_path).ok();
        let (doc, how) = sessionfile::resume(&session, s, &raw, saved.as_deref());
        if let Resumed::Discarded(why) = how {
            return Err(format!(
                "can't use the saved session {}: {why}. Pass the same review files as the app (with -r), \
                 or move the session file aside to start fresh.",
                session_path.display()
            ));
        }
        Ok(Self { fdx, raw, session, doc, session_path })
    }

    fn save(&self) -> Result<(), String> {
        let json = SessionFile::new(&self.raw, self.doc.log()).to_json();
        std::fs::write(&self.session_path, json).map_err(|e| format!("{}: {e}", self.session_path.display()))
    }

    fn sel(&self) -> Selection {
        Selection::cursor(self.doc.start())
    }

    fn text(&self, para: ParaId) -> String {
        self.doc.paragraph(para).map(|p| p.text()).unwrap_or_default()
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

fn status_name(s: Status) -> String {
    match s {
        Status::Pending => "pending".into(),
        Status::Accepted(k) => format!("accepted {}", k + 1),
        Status::Rejected => "kept".into(),
        Status::Rewritten => "rewritten".into(),
    }
}

fn summary(o: &Opened) {
    for (r, review) in o.session.reviews.iter().enumerate() {
        let p = o.session.progress(&o.doc, r);
        let fuzzy = review.items.iter().filter(|i| i.fuzzy).count();
        println!("{}", review.name);
        println!("  suggestions   {} ({fuzzy} matched approximately)", p.total);
        println!(
            "  decided       {} accepted, {} kept, {} rewritten, {} pending",
            p.accepted, p.rejected, p.rewritten, p.pending
        );
        if !review.invisible.is_empty() {
            println!(
                "  skipped       {} that change nothing visible (e.g. only letter case in a character name)",
                review.invisible.len()
            );
        }
        if !review.unmatched.is_empty() {
            println!("  not found     {}", review.unmatched.len());
            for u in review.unmatched.iter().take(10) {
                println!("    block {}: {}", u.entry + 1, u.before);
            }
        }
        for w in &review.warnings {
            println!("  warning: {w}");
        }
    }
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

fn suggestions(script: &str, review: &str, flags: &[&str]) -> Result<(), String> {
    let (mut pending_only, mut json) = (false, false);
    for f in flags {
        match *f {
            "--pending" => pending_only = true,
            "--json" => json = true,
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    let o = Opened::open(script, &[review])?;
    let ctx = annotate(o.doc.script());
    let rows: Vec<SuggestionRow> = o.session.reviews[0]
        .items
        .iter()
        .enumerate()
        .filter(|(i, _)| !pending_only || o.session.status(&o.doc, 0, *i) == Status::Pending)
        .map(|(i, it)| {
            let ix = o.doc.index_of(it.para).unwrap_or(it.index);
            SuggestionRow {
                n: i + 1,
                paragraph: it.para.0,
                kind: o.doc.paragraphs()[ix].kind.name().to_string(),
                scene: ctx[ix].scene.clone(),
                speaker: ctx[ix].speaker.clone(),
                status: status_name(o.session.status(&o.doc, 0, i)),
                outdated: o.session.is_outdated(&o.doc, 0, i),
                fuzzy: it.fuzzy,
                current: o.text(it.para),
                before: it.before.clone(),
                options: it.options.clone(),
                why: it.why.clone(),
            }
        })
        .collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&rows).expect("rows serialize"));
        return Ok(());
    }
    for r in rows {
        let flags = [(r.outdated, " (line changed since)"), (r.fuzzy, " (matched approximately)")]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, t)| *t)
            .collect::<String>();
        println!("#{} ¶{} {}{flags}", r.n, r.paragraph, r.status);
        println!("  now: {}", r.current);
        for (k, opt) in r.options.iter().enumerate() {
            println!("  {}: {opt}", k + 1);
        }
        if !r.why.is_empty() {
            println!("  why: {}", r.why);
        }
    }
    Ok(())
}

fn decide(script: &str, review: &str, n: &str, decision: &[&str]) -> Result<(), String> {
    let mut o = Opened::open(script, &[review])?;
    let count = o.session.reviews[0].items.len();
    let i = n
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=count).contains(n))
        .ok_or(format!("no suggestion {n} (1 to {count})"))?
        - 1;
    let options = o.session.item(0, i).options.len();
    let d = match decision {
        ["accept"] => Decision::Accept(0),
        ["accept", k] => {
            let k = k
                .parse::<usize>()
                .ok()
                .filter(|k| (1..=options).contains(k))
                .ok_or(format!("no option {k} (1 to {options})"))?;
            Decision::Accept(k - 1)
        }
        ["keep"] => Decision::KeepMine,
        ["undecide"] => Decision::Undecided,
        ["rewrite", text] => Decision::Rewrite(text.to_string()),
        _ => return Err(format!("decide what? accept [<option>] | keep | undecide | rewrite <text>\n\n{USAGE}")),
    };
    let sel = o.sel();
    o.session.decide(&mut o.doc, sel, 0, i, d).map_err(|e| e.to_string())?;
    o.save()?;
    let item = o.session.item(0, i);
    println!("#{} {}: {}", i + 1, status_name(o.session.status(&o.doc, 0, i)), o.text(item.para));
    Ok(())
}

fn edit(script: &str, id: &str, text: &str, reviews: &[&str]) -> Result<(), String> {
    let mut o = Opened::open(script, reviews)?;
    let para = ParaId(id.parse::<u64>().map_err(|_| format!("paragraph id must be a number, got {id:?}"))?);
    if o.doc.paragraph(para).is_none() {
        return Err(format!("no paragraph {id} (see `scriptorium dump`)"));
    }
    if text.contains('\n') {
        return Err("one paragraph at a time: the text can't contain newlines".into());
    }
    let sel = o.sel();
    o.doc.replace_text(sel, para, text, &[]).map_err(|e| e.to_string())?;
    o.save()?;
    println!("¶{id}: {}", o.text(para));
    Ok(())
}

fn undo(script: &str, redo: bool, reviews: &[&str]) -> Result<(), String> {
    let mut o = Opened::open(script, reviews)?;
    let paras = o.doc.next_undo_paras(redo);
    let done = if redo { o.doc.redo() } else { o.doc.undo() };
    if done.is_none() {
        return Err(if redo { "nothing to redo".into() } else { "nothing to undo".into() });
    }
    o.save()?;
    for p in paras {
        println!("¶{}: {}", p.0, o.text(p));
    }
    Ok(())
}

fn suggest(review: &str, script: &str, rest: &[&str]) -> Result<(), String> {
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
    let fdx = load(script)?;
    let s = fdx.to_script();
    let p = match (para, before) {
        (Some(id), None) => {
            let id = ParaId(id.parse::<u64>().map_err(|_| format!("--para must be a number, got {id:?}"))?);
            s.get(id).ok_or(format!("no paragraph {} (see `scriptorium dump`)", id.0))?.clone()
        }
        (None, Some(b)) => {
            // Found the same way the review will find it.
            let probe = Review::open(
                "",
                &format!("---\nBEFORE: {b}\nAFTER: {}\n", after[0]),
                &s,
                |k| fdx.is_caps(k.name()),
                "",
            );
            let ix =
                probe.items.first().map(|i| i.index).or_else(|| (!probe.invisible.is_empty()).then_some(usize::MAX));
            match ix {
                Some(usize::MAX) => return Err("that change wouldn't be visible in Final Draft".into()),
                Some(ix) => s.paragraphs[ix].clone(),
                None => return Err(format!("no line in the script matches {b:?}")),
            }
        }
        _ => return Err("give exactly one of --para or --before".into()),
    };
    let text = p.text();
    if !p.kind.is_editable() {
        return Err(format!("paragraph {} ({}) isn't reviewable", p.id.0, p.kind.name()));
    }
    let caps = fdx.is_caps(p.kind.name());
    if after.iter().all(|a| looks_same(a, &text, caps)) {
        return Err("that change wouldn't be visible in Final Draft".into());
    }
    let path = Path::new(review);
    let mut file = match std::fs::read_to_string(path) {
        Ok(t) => ReviewFile::parse(&t),
        Err(_) => ReviewFile { script: file_name(Path::new(script)), ..ReviewFile::default() },
    };
    file.entries.push(Entry { before: text.trim().to_string(), options: after, why, ..Entry::default() });
    let out = file.serialize();
    debug_assert!(out.starts_with(FORMAT_HEADER));
    // Number it as `suggestions` and `decide` do: by position in the script.
    let reopened = Review::open("", &out, &s, |k| fdx.is_caps(k.name()), "");
    let n = reopened.items.iter().position(|it| it.entry == file.entries.len() - 1).map_or(0, |i| i + 1);
    std::fs::write(path, out).map_err(|e| format!("{review}: {e}"))?;
    match n {
        0 => println!(
            "added to {review}, but it doesn't match a line on its own: another suggestion already covers ¶{}",
            p.id.0
        ),
        n => println!("added suggestion #{n} (¶{}) to {review}", p.id.0),
    }
    Ok(())
}

fn progress(script: &str, review: &str, rest: &[&str]) -> Result<(), String> {
    let o = Opened::open(script, &[review])?;
    let out = o.session.save_progress(&o.doc, 0);
    match rest {
        [] => print!("{out}"),
        ["-o", file] => {
            std::fs::write(file, out).map_err(|e| format!("{file}: {e}"))?;
            println!("wrote {file}");
        }
        _ => return Err(format!("unexpected arguments {rest:?}")),
    }
    Ok(())
}

fn apply(script: &str, rest: &[&str]) -> Result<(), String> {
    let (mut reviews, mut accept_all, mut out) = (vec![], false, None);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match *a {
            "--accept-all" => accept_all = true,
            "-o" => out = Some(it.next().ok_or("-o needs a file name")?.to_string()),
            r => reviews.push(r),
        }
    }
    let mut o = Opened::open(script, &reviews)?;
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
    let out = out.map(PathBuf::from).unwrap_or_else(|| output_path(input));
    if out.canonicalize().ok() == input.canonicalize().ok() {
        return Err("refusing to overwrite the original script; pass -o with another name".into());
    }
    let raw = o.fdx.write_script(o.doc.script()).map_err(|e| e.to_string())?;
    let original = o.fdx.to_script();
    let changed = o.doc.paragraphs().iter().zip(&original.paragraphs).filter(|(a, b)| a.text() != b.text()).count();
    std::fs::write(&out, raw).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("wrote {} ({changed} paragraphs changed)", out.display());
    Ok(())
}

/// "<name> (scriptorium).fdx" next to the script, as the app writes it.
fn output_path(script: &Path) -> PathBuf {
    let stem = script.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    script.with_file_name(format!("{stem} (scriptorium).fdx"))
}
