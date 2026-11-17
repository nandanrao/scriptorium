//! Loading a script's pair for reading (plan step A5c): the links file beside
//! it, checked against the bytes of both files (C6) and re-found where they
//! differ, under the links lock; the other member read from its file. Every
//! host that reads a pair goes through [`load_pair`]: the CLI's `versions`
//! and `rows`, and later the tool hosts (`list_versions`, `read_rows`).
//!
//! Re-find writes the links file (only ids and the member's hash change,
//! never stamps); nothing else is written. A links file that doesn't parse
//! is never rewritten.

use std::path::{Path, PathBuf};

use scriptorium_core::{Document, ElementKind, Script};
use scriptorium_fdx::FdxFile;
use scriptorium_review::sessionfile;
use scriptorium_review::{Session, Status};
use scriptorium_versions::links::{self, Links, member_letter};
use scriptorium_versions::rows::sides;
use scriptorium_versions::{Alignment, LaidRow, Placed, PlacedState, lay_out, refind};

use super::lock_links;
use crate::write_atomic_no_link;

/// A script's pair as it reads now: the links file re-found where a
/// member's bytes changed, so its ids are positions in both files as they
/// are on disk (C6).
#[derive(Debug, Clone)]
pub struct LoadedPair {
    /// The links file.
    pub path: PathBuf,
    /// Its contents, describing both files' current bytes.
    pub links: Links,
    /// The member index of the script the pair was loaded for.
    pub this: usize,
    /// The other member, read from its file (never its session: it is
    /// opened read-only and never saved, plan "Locking").
    pub other: FdxFile,
    /// Where the other member's file is.
    pub other_path: PathBuf,
}

impl LoadedPair {
    /// The other member's index.
    pub fn other_index(&self) -> usize {
        if self.this == 0 { 1 } else { 0 }
    }

    /// Whether the script it was loaded for is the version (the made member,
    /// or `b` if none is marked): the side that is written and gets `ROW:`
    /// suggestions. Rows shown on the original swap this and other, and no
    /// row there needs work.
    pub fn this_is_version(&self) -> bool {
        sides(&self.links).1 == self.this
    }

    /// What the app and tools call member `m`: its language's name, or its
    /// file stem when it has none, or when both members would read the same
    /// (two files in one language).
    pub fn label(&self, m: usize) -> String {
        let labels: Vec<String> = self.links.members.iter().map(|x| x.label()).collect();
        if labels.iter().filter(|l| **l == labels[m]).count() > 1 {
            let file = &self.links.members[m].file;
            return file.rsplit_once('.').map_or(file.as_str(), |(stem, _)| stem).to_string();
        }
        labels[m].clone()
    }

    /// The pair's rows as they read now ([`lay_out`]). `this` is the file of
    /// the script the pair was loaded for (its bytes are the ones checked),
    /// `this_live` its live script (the session applied). The original is
    /// always read from its file (C8): when `this` is the original,
    /// `this_live` is not used, the version is read from its file, and
    /// `placed` should be empty (the version's session isn't opened).
    /// One `is_caps` for both members, the original's (plan, "Status").
    pub fn lay_out(&self, this: &FdxFile, this_live: &Script, placed: &[Placed]) -> Vec<LaidRow> {
        let (original, version, caps_of) = if self.this_is_version() {
            (self.other.to_script(), this_live.clone(), &self.other)
        } else {
            (this.to_script(), self.other.to_script(), this)
        };
        let is_caps = |k: &ElementKind| caps_of.is_caps(k.name());
        lay_out(&self.links, &original, &version, placed, &is_caps)
    }
}

/// A `ROW:` suggestion of the pair, as review placed it in a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedItem {
    /// Index into `session.reviews`, and into that review's items.
    pub review: usize,
    pub item: usize,
    pub placed: Placed,
}

/// The suggestions of `session` (the version's, over its live `doc`) placed
/// on rows of the pair `links_id`, for [`LoadedPair::lay_out`]. Suggestions
/// whose line is gone (`Status::Removed`) are left out. Placement itself is
/// review's (through the resolver the host gives its open path); this only
/// reads where each item is now.
pub fn placed_items(session: &Session, doc: &Document, links_id: &str) -> Vec<PlacedItem> {
    let mut out = vec![];
    for (r, review) in session.reviews.iter().enumerate() {
        for (i, item) in review.items.iter().enumerate() {
            let Some(row) = item.row.as_ref().filter(|row| row.id == links_id) else { continue };
            let state = match session.status(doc, r, i) {
                Status::Pending => PlacedState::Pending,
                Status::Accepted(_) => PlacedState::Accepted,
                Status::Rewritten => PlacedState::Rewritten,
                Status::Rejected => PlacedState::Kept,
                Status::Removed => continue,
            };
            let cell = session.live_run(doc, r, i);
            out.push(PlacedItem { review: r, item: i, placed: Placed { cell, state, row_text: row.texts.clone() } });
        }
    }
    out
}

/// The pair of the script `this` (its file as read, named `script_name`, in
/// `dir`), checked against the bytes of both files and re-found where they
/// differ (C6): `Ok(None)` when no links file beside it names it. Holds the
/// links lock from reading the links file to writing it (after any script
/// lock the caller holds, plan "Locking").
///
/// A re-find rewrites the links file, re-reading the other member just
/// before and starting over if it changed meanwhile; if `this` changed on
/// disk since the caller read it, that is an error (its live script would
/// no longer be what the ids describe). Re-find messages ("PURPLE LIST.fdx
/// changed outside Scriptorium: …") and notes about other links files go to
/// `notes`.
///
/// Errors: a links file naming the script that can't be read (nothing is
/// rewritten), the other member missing (with the fix, and any `.fdx` in
/// the folder holding the recorded bytes) or not a Final Draft file.
pub fn load_pair(
    dir: &Path,
    script_name: &str,
    this: &FdxFile,
    notes: &mut Vec<String>,
) -> Result<Option<LoadedPair>, String> {
    let found = super::find_pair(dir, script_name);
    let Some(pair) = found.pair else {
        if found.notes.is_empty() {
            return Ok(None);
        }
        return Err(found.notes.join("; "));
    };
    notes.extend(found.notes);
    load_pair_at(&pair.links, dir, script_name, this, notes).map(Some)
}

/// [`load_pair`] for the links file at `at`, which names `script_name`.
pub fn load_pair_at(
    at: &Path,
    dir: &Path,
    script_name: &str,
    this: &FdxFile,
    notes: &mut Vec<String>,
) -> Result<LoadedPair, String> {
    let links_name = crate::opened::file_name(at);
    let _lock = lock_links(at, notes)?;
    for _ in 0..5 {
        let text = std::fs::read_to_string(at).map_err(|e| format!("{}: {e}", at.display()))?;
        let mut pair =
            links::parse(&text).map_err(|e| format!("{links_name} can't be read ({e}); it is left as it is"))?;
        let this_m = pair.member_of(script_name).ok_or_else(|| format!("{links_name} doesn't name {script_name}"))?;
        let other_m = if this_m == 0 { 1 } else { 0 };
        let other_file = pair.members[other_m].file.clone();
        let other_path = dir.join(&other_file);
        let other_raw = std::fs::read_to_string(&other_path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => missing(dir, &text, &links_name, other_m, &pair),
            _ => format!("{}: {e}", other_path.display()),
        })?;
        let other = FdxFile::parse(other_raw).map_err(|e| format!("{}: {e}", other_path.display()))?;

        let mut msgs = vec![];
        for (m, fdx) in [(this_m, this), (other_m, &other)] {
            let hash = sessionfile::hash(fdx.raw());
            if pair.members[m].hash == hash {
                continue;
            }
            // Each member's own caps styles for its own re-find.
            let is_caps = |k: &ElementKind| fdx.is_caps(k.name());
            let (refound, alignment) = refind(&pair, m, &fdx.to_script(), &hash, &is_caps);
            msgs.push(refind_message(&pair.members[m].file, &alignment));
            pair = refound;
        }
        if !msgs.is_empty() {
            // Just before writing: both files still the bytes re-found.
            let this_path = dir.join(script_name);
            let now = std::fs::read_to_string(&this_path).map_err(|e| format!("{}: {e}", this_path.display()))?;
            if now != this.raw() {
                return Err(format!("{script_name} changed while this command ran; run it again"));
            }
            let again = std::fs::read_to_string(&other_path).map_err(|e| format!("{}: {e}", other_path.display()))?;
            if again != other.raw() {
                continue;
            }
            let out = links::write(&pair).map_err(|e| format!("{links_name}: {e}"))?;
            write_atomic_no_link(at, out.as_bytes()).map_err(|e| format!("{}: {e}", at.display()))?;
            notes.extend(msgs);
        }
        return Ok(LoadedPair { path: at.to_path_buf(), links: pair, this: this_m, other, other_path });
    }
    Err(format!("the files of {links_name} keep changing; try again when they're saved"))
}

/// The message for a member file that's missing: which line to edit, and
/// any `.fdx` in the folder whose bytes are the ones the links file records.
pub(super) fn missing(dir: &Path, text: &str, links_name: &str, m: usize, pair: &Links) -> String {
    let file = &pair.members[m].file;
    let letter = member_letter(m);
    let line = text
        .lines()
        .position(|l| {
            l.split_once(':')
                .is_some_and(|(k, v)| k.trim().eq_ignore_ascii_case(&letter.to_string()) && v.trim() == file)
        })
        .map_or(String::new(), |n| format!("line {} of ", n + 1));
    let mut out = format!("{file} is missing: if it was renamed, edit {line}{links_name} to the new name");
    let mut same: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("fdx")))
        .filter(|p| std::fs::read_to_string(p).is_ok_and(|raw| sessionfile::hash(&raw) == pair.members[m].hash))
        .map(|p| crate::opened::file_name(&p))
        .collect();
    same.sort();
    if !same.is_empty() {
        out.push_str(&format!(" ({} holds the bytes it records)", same.join(", ")));
    }
    out
}

/// "PURPLE LIST.fdx changed outside Scriptorium: 2,914 lines found again, 3
/// changed, 4 new, 1 gone."
pub fn refind_message(file: &str, found: &Alignment) -> String {
    format!(
        "{file} changed outside Scriptorium: {} lines found again, {} changed, {} new, {} gone.",
        thousands(found.exact),
        thousands(found.changed()),
        thousands(found.new.len()),
        thousands(found.gone.len())
    )
}

pub(super) fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(2914), "2,914");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
