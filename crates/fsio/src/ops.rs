//! The file commands that change a script's session: attach and detach
//! review files, decide a suggestion, edit a line, undo or redo, and write
//! the script. The CLI's commands and the agent hosts call these, so every
//! host gives the same results, word for word.
//!
//! Each works over an [`Opened`] (or, for [`detach`], the session file) the
//! caller already holds under the script's [`crate::SessionLock`], taken
//! before opening and held until the op returns (AGENTS.md rule 5). Each
//! returns the text the CLI prints.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use scriptorium_core::{ParaId, Paragraph};
use scriptorium_review::sessionfile::{self, SessionFile};
use scriptorium_review::{Decision, Line, Status};
use scriptorium_tools::{item_numbered, item_numbers};

use crate::opened::{Opened, file_name, script_dir};
use crate::{Folders, write_atomic};

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
        match s.strip_prefix('e').and_then(|d| d.parse::<usize>().ok()) {
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
    /// Replace a block's lines with these (`[]` cuts them); untyped lines
    /// take the kind of the line in their place.
    RewriteBlock(Vec<Line>),
    /// Take the decision back.
    Undecide,
}

/// Save the review files `o` was opened with (`o.given`, attached by
/// opening) into the session; the result lists each with its suggestion
/// count. `script` is the script as the caller named it (for the hint).
pub fn attach(o: &mut Opened, script: &str) -> Result<String, String> {
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

/// Detach review files from `script` (as attached, or paths from the current
/// folder). Only the session file changes, so this reads and writes it
/// directly rather than opening the script: a session that can't be replayed
/// can still lose a review. The caller holds the session lock.
///
/// Callers pass each file as attached (its stored path) or as an absolute or
/// already-resolved path: a relative path that isn't a stored one resolves
/// against the process's working folder, not the script's.
pub fn detach(script: &str, files: &[&str]) -> Result<String, String> {
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
    crate::write_atomic_no_link(&session_path, file.to_json().as_bytes())
        .map_err(|e| format!("{}: {e}", session_path.display()))?;
    Ok(out)
}

/// Decide suggestion `n` of review `r` (as [`Opened::open_one`] picked it),
/// and save. Returns the suggestion's new status and line.
pub fn decide(o: &mut Opened, r: usize, n: SuggestionRef, choice: Choice) -> Result<String, String> {
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
    if matches!(choice, Choice::Rewrite(_)) && o.session.item(r, i).block {
        return Err("that suggestion is a block: plain text would collapse it into one line; rewrite it with \
                    --rewrite-block '<TYPE: text lines>' (MCP: `lines`)"
            .into());
    }
    let d = match choice {
        Choice::Accept(None) => Decision::Accept(0),
        Choice::Accept(Some(k)) if (1..=options).contains(&k) => Decision::Accept(k - 1),
        Choice::Accept(Some(k)) => return Err(format!("no option {k} (1 to {options})")),
        Choice::Keep => Decision::KeepMine,
        Choice::Undecide => Decision::Undecided,
        Choice::Rewrite(text) => Decision::Rewrite(text),
        Choice::RewriteBlock(lines) => {
            if !o.session.item(r, i).block && lines.len() != 1 {
                return Err("that suggestion is for one line: rewrite it with one line (or `rewrite <text>`)".into());
            }
            Decision::RewriteLines(lines)
        }
    };
    let n = item_numbers(&o.doc, &o.session.reviews[r].items)[i];
    let hint = rewrite_hint(n, o.session.item(r, i).block);
    o.session.check(&o.doc, r, i, &d).map_err(|e| format!("#{n}: {}", e.message(&hint)))?;
    let sel = o.sel();
    o.session.decide(&mut o.doc, sel, r, i, d).map_err(|e| e.to_string())?;
    o.save()?;
    let item = o.session.item(r, i);
    let status = o.session.status_label(&o.doc, r, i);
    if item.block {
        let mut out = format!("#{n} {status}:\n");
        let lines = o.session.mine(&o.doc, r, i);
        if lines.is_empty() {
            out.push_str("  [cut]\n");
        }
        for (id, l) in o.session.live_run(&o.doc, r, i).iter().zip(&lines) {
            let kind = l.kind.as_ref().map(|k| k.name().to_uppercase()).unwrap_or_default();
            let _ = writeln!(out, "  ¶{} {kind}: {}", id.0, l.text);
        }
        return Ok(out);
    }
    Ok(format!("#{n} {status}: {}\n", o.text(item.para)))
}

/// How the CLI tells someone to rewrite suggestion `n` (see
/// [`scriptorium_review::Refusal::message`]).
pub fn rewrite_hint(n: usize, block: bool) -> String {
    if block {
        format!("use decide {n} --rewrite-block '<TYPE: text lines>'")
    } else {
        format!("use decide {n} rewrite <text>")
    }
}

/// Refuse text that can't be one paragraph: a line break (`\r` counts like
/// `\n`) or a character Final Draft's XML can't hold (control characters
/// other than tab, U+FFFE, U+FFFF).
pub fn check_text(text: &str) -> Result<(), String> {
    match text.chars().find(|&c| scriptorium_fdx::is_unwritable(c)) {
        None => Ok(()),
        Some('\n' | '\r') => Err("one paragraph at a time: the text can't contain newlines".into()),
        Some(c) => Err(format!("the text can't contain the control character U+{:04X}", c as u32)),
    }
}

/// Replace paragraph `para`'s whole text, and save (with any review files
/// `o` was opened with).
pub fn edit(o: &mut Opened, para: u64, text: &str) -> Result<String, String> {
    let id = para;
    let para = ParaId(para);
    if o.doc.paragraph(para).is_none() {
        return Err(format!("no paragraph {id} (use list_paragraphs to find ids)"));
    }
    check_text(text)?;
    let sel = o.sel();
    o.doc.replace_text(sel, para, text, &[]).map_err(|e| e.to_string())?;
    o.save()?;
    Ok(format!("¶{id}: {}\n", o.text(para)))
}

/// Undo (or redo) the last change to the script, and save. Lists the
/// paragraphs it changed, as they are now.
pub fn undo_redo(o: &mut Opened, redo: bool) -> Result<String, String> {
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

/// "<name> (scriptorium).fdx" next to the script, as the app writes it.
pub fn output_path(script: &Path) -> PathBuf {
    let stem = script.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    script.with_file_name(format!("{stem} (scriptorium).fdx"))
}

/// Write the script (`script`, as the caller named it) with the session's
/// decisions and edits to `output` (default [`output_path`], replaced if it
/// exists); never over the script itself. `accept_all` also takes option 1
/// of every pending suggestion, in this output only. Review files `o` was
/// opened with are saved into the session first.
///
/// The version Scriptorium made is written in place from the app only, so
/// for it (or a script that may be it) an `output` is required. A person's
/// command may name any output; an agent's goes through [`agent_output`]
/// first.
pub fn write(o: &mut Opened, script: &str, accept_all: bool, output: Option<&Path>) -> Result<String, String> {
    if output.is_none() && o.pair().is_made() {
        // The version Scriptorium made is written in place, from the app
        // only (plan, "Writing a version in place"); elsewhere it needs -o.
        return Err(format!("{} is written from the app: Write .fdx (or give -o <file>)", o.script_name));
    }
    if output.is_none() && o.pair().maybe_made() {
        // A links file naming it can't be read: it may be the made version.
        return Err(format!(
            "{} may be the version Scriptorium made (a links file naming it can't be used), which is written from \
             the app: Write .fdx (or give -o <file>)",
            o.script_name
        ));
    }
    // Review files named here are attached, as on every command that takes
    // them. Saved before accept_all, which only shapes this output.
    if !o.given.is_empty() {
        o.save()?;
    }
    let mut skipped = String::new();
    if accept_all {
        let sel = o.sel();
        for r in 0..o.session.reviews.len() {
            let numbers = item_numbers(&o.doc, &o.session.reviews[r].items);
            let mut order: Vec<usize> = (0..numbers.len()).collect();
            order.sort_by_key(|&i| numbers[i]);
            for i in order {
                // A gone, split or joined line can't take a whole-line option:
                // left as it is, and said.
                let pending = matches!(o.session.status(&o.doc, r, i), Status::Pending | Status::Removed);
                if !pending {
                    continue;
                }
                match o.session.check(&o.doc, r, i, &Decision::Accept(0)) {
                    Ok(()) => {
                        o.session.decide(&mut o.doc, sel, r, i, Decision::Accept(0)).map_err(|e| e.to_string())?;
                    }
                    Err(e) => {
                        let n = numbers[i];
                        let name = &o.session.reviews[r].name;
                        let _ = writeln!(
                            skipped,
                            "skipped {name} #{n}: {}",
                            e.message(&rewrite_hint(n, o.session.item(r, i).block))
                        );
                    }
                }
            }
        }
    }
    let input = Path::new(script);
    let out = output.map(Path::to_path_buf).unwrap_or_else(|| output_path(input));
    if crate::same_file(&out, input) {
        return Err("refusing to overwrite the original script; give an output with another name".into());
    }
    let raw = o.fdx.write_script(o.doc.script()).map_err(|e| e.to_string())?;
    let original = o.fdx.to_script();
    let changed = changed_paragraphs(&original.paragraphs, o.doc.paragraphs());
    write_atomic(&out, raw.as_bytes()).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(format!("{skipped}wrote {} ({changed} paragraphs changed)\n", out.display()))
}

/// Where an agent's `write_script` may write (`script` as the agent named
/// it; `output` with `~` already expanded, `None` for the default): always a
/// `.fdx` inside the script's folder, never through a link, and never over
/// an existing `.fdx` but this script's own default output. With no
/// `output`, the version Scriptorium made is refused (the writer saves it in
/// the app). Pass the result to [`write`].
pub fn agent_output(o: &Opened, script: &str, output: Option<&str>) -> Result<PathBuf, String> {
    let dir = script_dir(script)?;
    // The default output of the script itself (through a link, the file it
    // points at: its name, in its folder).
    let canonical = Path::new(script).canonicalize().map_err(|e| format!("{script}: {e}"))?;
    let own = dir.join(output_path(&canonical).file_name().unwrap_or_default());
    let output = match output {
        Some(o) => o.to_string(),
        None => {
            if o.pair().is_made() || o.pair().maybe_made() {
                return Err(format!(
                    "{} is the version Scriptorium made: the writer saves it in the app with Write .fdx. To write a \
                     copy here, give `output`",
                    o.script_name
                ));
            }
            own.to_string_lossy().into_owned()
        }
    };
    // The default name too: a link planted there is refused.
    let out = crate::agent_file_in_folder(&dir, &output, ".fdx", Folders::MustExist)?;
    // The model never replaces another screenplay: an existing .fdx is
    // replaced only if it is exactly this script's own output.
    if out.exists() && out != own {
        let name = out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        return Err(format!(
            "{name} already exists and isn't this script's own output (\"{}\"): give an output with another name",
            own.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    Ok(out)
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

#[cfg(test)]
mod tests {
    use super::*;
    use scriptorium_core::ElementKind;

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

    #[test]
    fn suggestion_refs_parse() {
        assert_eq!(SuggestionRef::parse("3"), Some(SuggestionRef::Number(3)));
        assert_eq!(SuggestionRef::parse("e3"), Some(SuggestionRef::Entry(3)));
        assert_eq!(SuggestionRef::parse("x"), None);
    }

    #[test]
    fn check_text_refuses_breaks_and_controls() {
        assert!(check_text("fine\ttext").is_ok());
        assert!(check_text("a\nb").unwrap_err().contains("newlines"));
        assert!(check_text("a\rb").unwrap_err().contains("newlines"));
        assert!(check_text("a\u{1}b").unwrap_err().contains("U+0001"));
    }
}
