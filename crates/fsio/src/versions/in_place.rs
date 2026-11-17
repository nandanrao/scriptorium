//! Writing the version Scriptorium made over its own file (plan step A7b,
//! "Writing a version in place"). Only the app calls this (its Write .fdx on
//! the made member); the CLI's `apply` and MCP's `write_script` need an
//! output for that file, so nothing writes it under an open window.
//!
//! The caller first saves its session (under the script's lock: reload what
//! changed on disk, write the session file), then calls [`write_in_place`]
//! with the bytes it had opened, still holding the lock. Everything is then
//! read fresh from disk, as [`Opened::open`] reads it, in the plan's order:
//!
//! 1. **Guard:** the `.fdx` on disk is the bytes the caller opened, the
//!    session file is saved for them, and the links file's `hash:` for this
//!    member is theirs. Then the script is opened (the pair re-found where
//!    the original changed, review files read as they are now), and the
//!    links lock taken and held through the links file's write; the links
//!    file and the original must still be what opening read. Refused while
//!    a suggestion's line was split, joined or removed
//!    ([`InPlaceError::Blocked`]).
//! 2. **Progress** into each review file (`STATUS`, `CHOSEN`, `MINE`), with
//!    `PARA` at the written file's positions for entries placed as items,
//!    and dropped for every other entry ([`scriptorium_review::Session::save_progress_at`]).
//! 3. **The `.fdx`**: the live document, spliced into the file's bytes.
//!    Paragraph *i* of the new file is live paragraph *i*. If this write
//!    fails, the review files are put back as they were read.
//! 4. **The links file** ([`after_write`]): ids to positions, the new hash,
//!    `=` stamps on rows in sync.
//! 5. **A fresh session** for the new bytes, as a first open builds it;
//!    decisions come back from step 2.
//!
//! Everything is worked out before the first write, so a refusal writes
//! nothing.
//!
//! **A crash between writes.** After step 2, before step 3: the review files
//! hold the progress (already in the session) with `PARA` hints at positions
//! the `.fdx` doesn't have yet. `ROW:` entries are placed by row content, so
//! they don't move; other entries fall back to their `BEFORE` text when the
//! hinted line doesn't fit them, but one whose hinted line happens to read
//! the same could be placed there (a known limit). After step 3 or 4: the
//! version's bytes no longer match the links file (re-find maps the ids) or
//! the session file (set aside: the app keeps it as a set-aside file; the CLI
//! sets it aside when it held only decisions, else refuses, naming the app),
//! and the decisions are in the review files.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use scriptorium_core::{ElementKind, ParaId};
use scriptorium_fdx::FdxFile;
use scriptorium_review::file::ReviewFile;
use scriptorium_review::sessionfile::{self, ReviewSource, SessionFile};
use scriptorium_review::{Entry, FileStatus, Line, LineChange};
use scriptorium_versions::{Placed, after_write, links};

use super::pair::{lock_links, placed_items};
use crate::opened::Opened;
use crate::{write_atomic, write_atomic_no_link};

/// What [`write_in_place`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenInPlace {
    /// The version's `.fdx`, as written.
    pub path: PathBuf,
    /// `sessionfile::hash` of its new bytes.
    pub hash: String,
    /// Said once in the status bar: "Wrote PURPLE LIST.es.fdx. Undo starts
    /// again from here."
    pub message: String,
    /// Anything else to tell the user (the original re-found).
    pub notes: Vec<String>,
}

/// Why [`write_in_place`] wrote nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InPlaceError {
    /// Suggestions whose line was split or joined (pending ones) or removed
    /// ([`scriptorium_review::Session::blocking_write`]), as (review, item, how) in the session
    /// as saved on disk, which is the caller's own once it has saved: a
    /// host that reloads what changed on disk before it saves numbers them
    /// the same way. The host names them by its numbers with [`blocked`].
    Blocked(Vec<(usize, usize, LineChange)>),
    /// Anything else, as a sentence to show. Nothing was written (any
    /// review file written first was put back).
    Refused(String),
    /// The `.fdx` (and the review files' progress) was written, but the
    /// links file or the fresh session wasn't: the host drops its view
    /// without saving, as after a write that went through, and opens the
    /// written file, which finds its lines again (re-find) and starts its
    /// session fresh from the review files. `message` says so.
    WrittenButIncomplete { path: PathBuf, message: String },
}

impl std::fmt::Display for InPlaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blocked(items) => {
                let n = items.len();
                write!(
                    f,
                    "{n} suggestion{} on lines split, joined or removed since {} made: deal with {} first.",
                    if n == 1 { " is" } else { "s are" },
                    if n == 1 { "it was" } else { "they were" },
                    if n == 1 { "it" } else { "them" },
                )
            }
            Self::Refused(why) | Self::WrittenButIncomplete { message: why, .. } => f.write_str(why),
        }
    }
}

impl std::error::Error for InPlaceError {}

impl From<String> for InPlaceError {
    fn from(why: String) -> Self {
        Self::Refused(why)
    }
}

/// What the status bar says for [`InPlaceError::Blocked`], given each
/// item's number as the host shows it and how its line changed: "Decide or
/// rewrite #4 and #9 first." for split or joined lines; "#7's line was
/// removed: undo that first." (a suggestion on a removed line can't be
/// decided, and the written file could place it on another line that reads
/// the same).
pub fn blocked(items: &[(usize, LineChange)]) -> String {
    let list = |ns: Vec<usize>| {
        let named: Vec<String> = ns.iter().map(|n| format!("#{n}")).collect();
        match named.as_slice() {
            [] => String::new(),
            [one] => one.clone(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        }
    };
    let removed: Vec<usize> = items.iter().filter(|(_, c)| *c == LineChange::Removed).map(|(n, _)| *n).collect();
    let changed: Vec<usize> = items.iter().filter(|(_, c)| *c != LineChange::Removed).map(|(n, _)| *n).collect();
    let mut out = vec![];
    if !changed.is_empty() {
        out.push(format!("Decide or rewrite {} first.", list(changed)));
    }
    match removed.len() {
        0 => {}
        1 => out.push(format!("{}'s line was removed: undo that first.", list(removed))),
        _ => out.push(format!("The lines of {} were removed: undo that first.", list(removed))),
    }
    out.join(" ")
}

/// Write the version Scriptorium made (`script`, the made member of its
/// pair) over its own file, with the links file, its review files' progress
/// and a fresh session to match (module docs). `opened_raw` is the file's
/// bytes as the caller opened them; the caller has saved its session for
/// them and holds the script's [`crate::SessionLock`]. On success it drops
/// its session without saving it again and opens the written file.
pub fn write_in_place(script: &Path, opened_raw: &str) -> Result<WrittenInPlace, InPlaceError> {
    write_in_place_with(script, opened_raw, &|at, bytes| write_atomic(at, bytes))
}

/// [`write_in_place`] with the `.fdx` written by `write_fdx`: tests make it
/// fail.
#[doc(hidden)]
pub fn write_in_place_with(
    script: &Path,
    opened_raw: &str,
    write_fdx: &dyn Fn(&Path, &[u8]) -> io::Result<()>,
) -> Result<WrittenInPlace, InPlaceError> {
    let script_path = script.canonicalize().map_err(|e| format!("{}: {e}", script.display()))?;
    let name = crate::opened::file_name(&script_path);
    let dir = script_path.parent().map(Path::to_path_buf).unwrap_or_default();
    let changed = || {
        InPlaceError::Refused(format!(
            "{name} changed since you opened it. Close it in Final Draft and reopen it here."
        ))
    };
    let read_script = || std::fs::read_to_string(&script_path).map_err(|e| format!("{}: {e}", script_path.display()));

    // 1. The guard: the bytes, the session saved for them, the links file.
    if read_script()? != opened_raw {
        return Err(changed());
    }
    let session_path = dir.join(sessionfile::file_name_for(&name));
    let saved = std::fs::read_to_string(&session_path).ok().and_then(|j| SessionFile::from_json(&j).ok());
    if !saved.is_some_and(|s| s.is_for(opened_raw)) {
        return Err(InPlaceError::Refused(format!(
            "{name}'s session isn't saved for the file you opened; reload it and try again."
        )));
    }
    let found = super::find_pair(&dir, &name);
    let Some(pair) = found.pair.filter(|p| p.is_made) else {
        return Err(InPlaceError::Refused(format!(
            "{name} isn't a version Scriptorium made, so it isn't written over{}",
            if found.notes.is_empty() { String::new() } else { format!(" ({})", found.notes.join("; ")) }
        )));
    };
    if pair.hash != sessionfile::hash(opened_raw) {
        return Err(changed());
    }

    let mut notes = vec![];
    let o = Opened::open(&script_path.to_string_lossy(), &[], &mut notes)?;
    let Some(loaded) = o.pair().loaded().filter(|p| p.this_is_made() && p.path == pair.links) else {
        return Err(InPlaceError::Refused(format!("{name}'s links file can't be used now; reopen {name}")));
    };
    let links_name = crate::opened::file_name(&loaded.path);
    if o.raw != opened_raw {
        return Err(changed());
    }
    // Held from here through the links file's write.
    let guard = lock_links(&loaded.path, &mut notes)?;
    let links_now = std::fs::read_to_string(&loaded.path).map_err(|e| format!("{links_name}: {e}"))?;
    let other_now = std::fs::read_to_string(&loaded.other_path).unwrap_or_default();
    if sessionfile::hash(&links_now) != loaded.links_hash || sessionfile::hash(&other_now) != loaded.other_hash {
        return Err(InPlaceError::Refused(format!(
            "{links_name} or the file lined up with {name} changed while it was being written; nothing was written. \
             Try again."
        )));
    }
    let (session, doc) = (&o.session, &o.doc);
    let blocking = session.blocking_write(doc);
    if !blocking.is_empty() {
        return Err(InPlaceError::Blocked(blocking));
    }

    // Everything to write, worked out first.
    let new_raw = o.fdx.write_script(doc.script()).map_err(|e| format!("{name} can't be written: {e}"))?;
    let new_fdx = FdxFile::parse(new_raw.clone()).map_err(|e| format!("{name} can't be written: {e}"))?;
    let written = new_fdx.to_script();
    let new_hash = sessionfile::hash(&new_raw);
    let position: HashMap<ParaId, ParaId> =
        doc.paragraphs().iter().enumerate().map(|(i, p)| (p.id, ParaId(i as u64))).collect();
    // (review, where, the text read, the text to write)
    let mut progress = vec![];
    let mut texts = vec![];
    for (r, review) in session.reviews.iter().enumerate() {
        let read = review.text().to_string();
        let new = session.save_progress_at(doc, r, &position);
        texts.push(new.clone());
        let (now, was) = (ReviewFile::parse(&new).entries, ReviewFile::parse(&read).entries);
        if now == was {
            *texts.last_mut().expect("pushed") = read;
            continue;
        }
        let at = sessionfile::resolve_path(&dir, &review.path);
        if let Err(why) = crate::opened::agent_may_write(&dir, &at) {
            // Its decisions or its `PARA` hints must change with the file,
            // and stale hints would name other lines once it is written.
            return Err(InPlaceError::Refused(format!(
                "{} must be updated with {name} ({}), and it can't be ({why}). Add `{}` as its first line, or move \
                 it into the folder of {name}, then try again.",
                review.path,
                if decided(&now) != decided(&was) { "its decisions" } else { "where its suggestions are" },
                scriptorium_review::file::FORMAT_HEADER,
            )));
        }
        progress.push((at, read, new));
    }
    let is_caps = |k: &ElementKind| loaded.other.is_caps(k.name());
    let placed: Vec<Placed> = placed_items(session, doc, &loaded.links.id).into_iter().map(|p| p.placed).collect();
    let new_links = after_write(&loaded.links, &loaded.other_script, doc.script(), &placed, &new_hash, &is_caps);
    let links_text = links::write(&new_links).map_err(|e| format!("{links_name}: {e}"))?;

    // The fresh session over the review files as they will read.
    let sources: Vec<ReviewSource> = session
        .reviews
        .iter()
        .zip(texts)
        .map(|(r, text)| ReviewSource { name: r.name.clone(), path: r.path.clone(), text })
        .collect();
    let fresh_pair = super::LoadedPair {
        links: new_links,
        links_hash: sessionfile::hash(&links_text),
        refound: vec![],
        ..loaded.clone()
    };
    let mut fresh = {
        let rows = fresh_pair.rows_factory(&new_fdx);
        let caps = |k: &ElementKind| new_fdx.is_caps(k.name());
        let reader_1 = Some(new_fdx.legacy_script());
        sessionfile::resume(sources, written, reader_1.as_ref(), &new_raw, None, &caps, &name, Some(&*rows))
    };
    // The session's choice under "Lined up on the left" stays.
    fresh.links = o.links.clone();
    let session_json = fresh.to_file(&new_raw).to_json();

    // 2. Progress. Under the script lock nothing else writes them; still,
    // never over a file that changed since it was read.
    let put_back = |done: &[(PathBuf, String, String)]| {
        for (at, read, _) in done {
            let _ = write_atomic(at, read.as_bytes());
        }
    };
    for (k, (at, read, new)) in progress.iter().enumerate() {
        let wrote = match std::fs::read_to_string(at) {
            Ok(disk) if disk == *read => write_atomic(at, new.as_bytes()).map_err(|e| format!("{}: {e}", at.display())),
            Ok(_) => Err(format!("{} changed on disk while {name} was being written", at.display())),
            Err(e) => Err(format!("{}: {e}", at.display())),
        };
        if let Err(why) = wrote {
            put_back(&progress[..k]);
            return Err(InPlaceError::Refused(format!("{why}; nothing was written. Try again.")));
        }
    }
    // 3. The version: Final Draft takes no lock, so its bytes are checked
    // once more right before.
    let wrote = match read_script() {
        Ok(now) if now == opened_raw => {
            write_fdx(&script_path, new_raw.as_bytes()).map_err(|e| format!("{name} wasn't written: {e}"))
        }
        Ok(_) => Err(changed().to_string()),
        Err(e) => Err(e),
    };
    if let Err(why) = wrote {
        put_back(&progress);
        return Err(InPlaceError::Refused(why));
    }
    // 4. The links file, still under its lock.
    let incomplete = |message: String| InPlaceError::WrittenButIncomplete { path: script_path.clone(), message };
    write_atomic_no_link(&loaded.path, links_text.as_bytes()).map_err(|e| {
        incomplete(format!(
            "Wrote {name}, but not {links_name} ({e}). Reopening {name} finds its lines again; the decisions are in \
             the review files."
        ))
    })?;
    drop(guard);
    // 5. The fresh session.
    write_atomic_no_link(&session_path, session_json.as_bytes()).map_err(|e| {
        incomplete(format!(
            "Wrote {name}, but not its session ({}: {e}). Reopening {name} starts it fresh; the decisions are in the \
             review files.",
            session_path.display()
        ))
    })?;
    notes.extend(fresh.notes);
    Ok(WrittenInPlace {
        path: script_path,
        hash: new_hash,
        message: format!("Wrote {name}. Undo starts again from here."),
        notes,
    })
}

/// What a review file records as decided, entry by entry.
fn decided(entries: &[Entry]) -> Vec<(FileStatus, Option<usize>, Option<Vec<Line>>)> {
    entries.iter().map(|e| (e.status, e.chosen, e.mine.clone())).collect()
}
