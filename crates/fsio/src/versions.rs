//! The shell around `scriptorium-versions`: finding a script's pair (the
//! `*.links.txt` beside it), and what opening, saving and writing a version
//! do on disk (`docs/plans/versions.md`). Built step by step.

use std::path::{Path, PathBuf};

use scriptorium_core::Document;
use scriptorium_review::Session;
use scriptorium_review::file::{FileStatus, Line, ReviewFile};
use scriptorium_review::sessionfile;
use scriptorium_versions::links::{self, member_letter};

use crate::write_atomic;

// `version new` (A5b): making a version and its links file.
mod version_new;
pub use version_new::{Made, NewVersion, check_version_args, default_version_name, links_name, version_new};

// Writing the made version over its own file (A7b): the app's Write .fdx.
mod in_place;
pub use in_place::{InPlaceError, WrittenInPlace, blocked, write_in_place, write_in_place_with};

// Opening a script with its pair (A5c, I1): found, re-found first, the
// resolver for `ROW:` suggestions, the rows.
mod pair;
pub use pair::{
    LaidOut, LinksGuard, LoadedPair, PairState, Patience, PlacedItem, lock_links, placed_items, refind_message, resume,
    resume_with,
};

/// A script's place in its pair, as the links file beside it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pair {
    /// The links file.
    pub links: PathBuf,
    /// This script's member index in it, and its letter (`a`, `b`).
    pub member: usize,
    pub letter: char,
    /// The member's `hash:`: the bytes its ids refer to.
    pub hash: String,
    /// Whether this script is the version Scriptorium made (`created:`): its
    /// sessions write their decisions into the review files on every save
    /// (plan C2), and only it is written in place.
    pub is_made: bool,
}

/// What [`find_pair`] found beside a script.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    /// The pair, from a links file that parses and names the script.
    pub pair: Option<Pair>,
    /// Whether the script may be the version Scriptorium made: the pair says
    /// so, or a links file that names it doesn't parse (it can't say
    /// otherwise, and writing progress loses nothing).
    pub maybe_made: bool,
    /// Things to tell the user: broken links files, several pairs.
    pub notes: Vec<String>,
}

/// The pair of the script `script_name` in `dir`: a `*.links.txt` there that
/// parses and names it as a member, preferring one where it is the made
/// member, then the first by name (with a note when that choice wasn't the
/// only one). A links file that names the script (on an `a:`/`b:` header
/// line) but doesn't parse is noted, and makes the script count as possibly
/// made.
pub fn find_pair(dir: &Path, script_name: &str) -> Found {
    let mut found = Found::default();
    let Ok(entries) = std::fs::read_dir(dir) else { return found };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.to_lowercase().ends_with(".links.txt")))
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    let mut pairs = vec![];
    for at in files {
        let Ok(text) = std::fs::read_to_string(&at) else { continue };
        match links::parse(&text) {
            Ok(parsed) => {
                let Some(member) = parsed.member_of(script_name) else { continue };
                pairs.push(Pair {
                    member,
                    letter: member_letter(member),
                    hash: parsed.members[member].hash.clone(),
                    is_made: parsed.made() == Some(member),
                    links: at,
                });
            }
            Err(e) if names(&text, script_name) => {
                found.maybe_made = true;
                found.notes.push(format!("{} names {script_name} but can't be read: {e}", file_name(&at)));
            }
            Err(_) => {}
        }
    }
    let made = pairs.iter().filter(|p| p.is_made).count();
    let candidates = if made > 0 { made } else { pairs.len() };
    let chosen = pairs.iter().position(|p| p.is_made).or((!pairs.is_empty()).then_some(0));
    if candidates > 1
        && let Some(i) = chosen
    {
        found.notes.push(format!(
            "several links files name {script_name} ({}); using {}",
            pairs.iter().map(|p| file_name(&p.links)).collect::<Vec<_>>().join(", "),
            file_name(&pairs[i].links)
        ));
    }
    found.pair = chosen.map(|i| pairs.swap_remove(i));
    found.maybe_made |= found.pair.as_ref().is_some_and(|p| p.is_made);
    found
}

/// Whether a links file's text names `script_name` on a member line
/// (`a: <file>`, `b: <file>`, … before the first `---`), read without
/// parsing the rest.
fn names(text: &str, script_name: &str) -> bool {
    let want = script_name.to_lowercase();
    text.lines().take_while(|l| l.trim() != "---").any(|l| {
        l.split_once(':').is_some_and(|(k, v)| {
            let k = k.trim();
            k.len() == 1 && k.chars().all(|c| c.is_ascii_alphabetic()) && v.trim().to_lowercase() == want
        })
    })
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Write each attached review's progress (`STATUS`, `CHOSEN`, `MINE`) into
/// its file, where it differs from what the file records, and keep the new
/// text in `session` (so the session file records its hash and the next open
/// sees no outside change). The caller holds the script's session lock and
/// saves the session afterwards; the CLI's save and the app's both call this
/// for the made version (plan C2).
///
/// Only files an agent could write are written ([`crate::opened::agent_may_write`]:
/// inside the script's folder `dir`, starting with `SCRIPT REVIEW`); the
/// others are named in the notes returned. A file that changed on disk since
/// it was read (an agent appended to it) is an error: reload and save again.
pub fn write_progress(dir: &Path, session: &mut Session, doc: &Document) -> Result<Vec<String>, String> {
    let mut notes = vec![];
    for r in 0..session.reviews.len() {
        let text = session.save_progress(doc, r);
        if progress(&text) == progress(session.reviews[r].text()) {
            continue;
        }
        let path = sessionfile::resolve_path(dir, &session.reviews[r].path);
        if let Err(why) = crate::opened::agent_may_write(dir, &path) {
            notes.push(format!("decisions not written into {}: {why}", session.reviews[r].path));
            continue;
        }
        let on_disk = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if on_disk != session.reviews[r].text() {
            return Err(format!("{} changed on disk since it was opened; reopen it and try again", path.display()));
        }
        let mut review = session.reviews[r].clone();
        review.wrote_progress(text.clone())?;
        write_atomic(&path, text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
        session.reviews[r] = review;
    }
    Ok(notes)
}

/// What a review file records as decided, entry by entry.
fn progress(text: &str) -> Vec<(FileStatus, Option<usize>, Option<Vec<Line>>)> {
    ReviewFile::parse(text).entries.into_iter().map(|e| (e.status, e.chosen, e.mine)).collect()
}
