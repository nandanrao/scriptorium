//! Loading a script's pair (plan steps A5c, I1): the links file beside it,
//! checked against the bytes of both files (C6) and re-found where they
//! differ, under the links lock; the other member read from its file. Every
//! host opens a script through [`resume`] (fsio's `Opened`, the app), which
//! loads the pair *before* resuming the session, so the ids it places `ROW:`
//! suggestions on are the file's; the resolver and the rows then come from
//! that one [`LoadedPair`].
//!
//! Re-find writes the links file (only ids and the member's hash change,
//! never stamps); nothing else is written. A links file that doesn't parse
//! is never rewritten. One re-find implementation, [`refind_locked`], serves
//! opening and `version new`.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::Duration;

use scriptorium_core::{Document, ElementKind, ParaId, Script};
use scriptorium_fdx::FdxFile;
use scriptorium_review::file::{Line, RowRef};
use scriptorium_review::sessionfile::{self, ResumedSession, ReviewSource, RowResolverFactory};
use scriptorium_review::{RowResolver, Session, Status};
use scriptorium_versions::links::{self, Links, member_letter};
use scriptorium_versions::{Alignment, LaidRow, PairView, Placed, PlacedState, Resolver, RowQuery, lay_out, refind};

use crate::{LOCK_WAIT, LockError, SessionLock, write_atomic_no_link};

/// Tries before re-find gives up on files that keep changing (or a member
/// that doesn't parse: Final Draft may be halfway through saving it).
const TRIES: usize = 5;
/// The pause between tries.
const RETRY_PAUSE: Duration = Duration::from_millis(100);

/// The links lock, held: from reading a links file to writing it
/// ([`lock_links`]). On a file system without locks it holds nothing (a
/// note said so) but is still the token [`refind_locked`] asks for.
pub struct LinksGuard {
    /// The links file it guards.
    at: PathBuf,
    _lock: Option<SessionLock>,
}

impl LinksGuard {
    /// The links file it guards.
    pub fn path(&self) -> &Path {
        &self.at
    }
}

/// Take the links file's lock, `<links stem>.lock` (`PURPLE LIST.es.links.lock`
/// for `PURPLE LIST.es.links.txt`): held from reading the links file to
/// writing it, after any script lock (plan, "Locking"). The lock is not
/// reentrant: never take it twice in one process (opening takes and releases
/// it; `version new` takes it after). On a file system without locks: a
/// guard holding nothing, with a note.
pub fn lock_links(links: &Path, notes: &mut Vec<String>) -> Result<LinksGuard, String> {
    let name = crate::opened::file_name(links);
    let stem = name.rsplit_once('.').map_or(name.as_str(), |(stem, _)| stem);
    let lock = match SessionLock::acquire(&links.with_file_name(format!("{stem}.lock")), LOCK_WAIT) {
        Ok(l) => Some(l),
        Err(e @ LockError::NoLocks) => {
            notes.push(e.to_string());
            None
        }
        Err(e) => return Err(e.to_string()),
    };
    Ok(LinksGuard { at: links.to_path_buf(), _lock: lock })
}

/// A script's place in a pair, as opening found it.
#[derive(Debug, Clone, Default)]
pub enum PairState {
    /// No links file beside the script names it.
    #[default]
    None,
    /// The pair, re-found where a member's bytes changed.
    Loaded(Box<LoadedPair>),
    /// A links file names the script but can't be used: it doesn't parse,
    /// or the other member is missing or isn't a Final Draft file. The
    /// script still opens; `ROW:` suggestions are not found, with `reason`
    /// (also told as a note).
    Unusable {
        /// The links file, when one parsed and named the script.
        links: Option<PathBuf>,
        reason: String,
        /// The script may be the version Scriptorium made (see
        /// [`super::Found::maybe_made`]).
        maybe_made: bool,
    },
}

impl PairState {
    /// The pair, when it loaded.
    pub fn loaded(&self) -> Option<&LoadedPair> {
        match self {
            PairState::Loaded(p) => Some(p),
            _ => None,
        }
    }

    /// The links file in use or found broken (when one parsed).
    pub fn links_path(&self) -> Option<&Path> {
        match self {
            PairState::None => None,
            PairState::Loaded(p) => Some(&p.path),
            PairState::Unusable { links, .. } => links.as_deref(),
        }
    }

    /// This script is the version Scriptorium made (`created:`).
    pub fn is_made(&self) -> bool {
        self.loaded().is_some_and(LoadedPair::this_is_made)
    }

    /// This script is, or may be, the version Scriptorium made: its sessions
    /// write their decisions into the review files on every save (plan C2),
    /// and the CLI writes it only with `-o`.
    pub fn maybe_made(&self) -> bool {
        match self {
            PairState::None => false,
            PairState::Loaded(p) => p.maybe_made,
            PairState::Unusable { maybe_made, .. } => *maybe_made,
        }
    }

    /// The pair's rows for the tools ([`LoadedPair::view`]): `None` without
    /// a pair, the reason when a links file names the script but can't be
    /// used. `this`, `session` and `doc`: the script's file as opened, its
    /// session and live document (the app passes its own).
    pub fn view<'a>(
        &'a self,
        this: &FdxFile,
        session: &Session,
        doc: &'a Document,
    ) -> Option<Result<PairView<'a>, String>> {
        match self {
            PairState::None => None,
            PairState::Unusable { reason, .. } => Some(Err(reason.clone())),
            PairState::Loaded(pair) => Some(Ok(pair.view(this, session, doc))),
        }
    }

    /// The resolver for `ROW:` suggestions on `doc`, the live document of the
    /// script `this` (its file as opened): `None` without a pair, or when
    /// on the original, every `ROW:` entry is not found (rows are suggested
    /// on the version only); unusable, likewise, with the reason.
    pub fn resolver(&self, this: &FdxFile, doc: &Document) -> Option<Box<RowResolver<'static>>> {
        match self {
            PairState::None => None,
            PairState::Loaded(p) => Some(p.resolver(this, doc)),
            PairState::Unusable { reason, .. } => Some(refusing(unusable(reason))),
        }
    }

    /// [`Self::resolver`] for review's open path, which builds it for the
    /// document it opens reviews against (`sessionfile::resume`).
    pub fn rows_factory<'a>(&'a self, this: &'a FdxFile) -> Option<Box<RowResolverFactory<'a>>> {
        match self {
            PairState::None => None,
            PairState::Loaded(p) => Some(p.rows_factory(this)),
            PairState::Unusable { reason, .. } => {
                Some(Box::new(move |_: &Document| -> Box<RowResolver<'a>> { refusing(unusable(reason)) }))
            }
        }
    }
}

/// A resolver that finds nothing, saying why.
fn refusing(why: String) -> Box<RowResolver<'static>> {
    Box::new(move |_: &RowRef, _: &[Vec<Line>], _: &[ParaId]| Err(why.clone()))
}

/// Why `ROW:` entries aren't found when the pair can't be used.
fn unusable(reason: &str) -> String {
    format!("the links file can't be used: {reason}")
}

/// Open the script `fdx` (named `name` in `dir`) with its session: find its
/// pair and load it, re-finding first (C6), then resume the session
/// (`saved`) with the review files in `sources`, placing `ROW:` suggestions
/// through the pair's resolver. Every host opens this way: fsio's `Opened`
/// (the CLI, MCP and chat hosts) and the app. Holds the links lock only
/// while loading the pair (after any script lock the caller holds). Notes
/// about the pair (re-finds, a broken or unusable links file) go to `notes`;
/// an unusable pair doesn't stop the script opening.
pub fn resume(
    fdx: &FdxFile,
    dir: &Path,
    name: &str,
    sources: Vec<ReviewSource>,
    saved: Option<&str>,
    notes: &mut Vec<String>,
) -> (ResumedSession, PairState) {
    let pair = load(dir, name, fdx, notes);
    let caps = |k: &ElementKind| fdx.is_caps(k.name());
    // Always checked: reader 1 also misnumbered files without DualDialogue
    // (nested paragraphs in scene summaries and arc beats), and it's cheap.
    let reader_1 = Some(fdx.legacy_script());
    let opened = {
        let rows = pair.rows_factory(fdx);
        sessionfile::resume(sources, fdx.to_script(), reader_1.as_ref(), fdx.raw(), saved, &caps, name, rows.as_deref())
    };
    (opened, pair)
}

/// The pair of the script `this` (its file as read, named `script_name`, in
/// `dir`), re-found where a member's bytes changed ([`load_pair_at`]).
fn load(dir: &Path, script_name: &str, this: &FdxFile, notes: &mut Vec<String>) -> PairState {
    let found = super::find_pair(dir, script_name);
    let maybe_made = found.maybe_made;
    let broken = found.notes.clone();
    notes.extend(found.notes);
    let Some(pair) = found.pair else {
        if maybe_made {
            return PairState::Unusable { links: None, reason: broken.join("; "), maybe_made };
        }
        return PairState::None;
    };
    match load_pair_at(&pair.links, dir, script_name, this, notes) {
        Ok(mut loaded) => {
            loaded.maybe_made = maybe_made;
            PairState::Loaded(Box::new(loaded))
        }
        Err(reason) => {
            notes.push(reason.clone());
            PairState::Unusable { links: Some(pair.links), reason, maybe_made }
        }
    }
}

/// A script's pair as it reads now: the links file re-found where a
/// member's bytes changed, so its ids are positions in both files as they
/// are on disk (C6).
#[derive(Debug, Clone)]
pub struct LoadedPair {
    /// The links file.
    pub path: PathBuf,
    /// Its contents, describing both files' current bytes.
    pub links: Links,
    /// `sessionfile::hash` of the links file as it is on disk after loading
    /// (the text written, if re-find rewrote it): with [`Self::other_hash`],
    /// what a host checks to know whether to load the pair again.
    pub links_hash: String,
    /// The member index of the script the pair was loaded for.
    pub this: usize,
    /// The other member, read from its file (never its session: it is
    /// opened read-only and never saved, plan "Locking").
    pub other: FdxFile,
    /// Its script, read once ([`FdxFile::to_script`]).
    pub other_script: Script,
    /// Where the other member's file is.
    pub other_path: PathBuf,
    /// The other member's hash as loaded (`sessionfile::hash` of its bytes).
    pub other_hash: String,
    /// What re-find said while loading (already in the notes).
    pub refound: Vec<String>,
    /// The script is, or may be, the version Scriptorium made
    /// ([`PairState::maybe_made`]).
    pub maybe_made: bool,
}

/// The pair's rows ([`LoadedPair::lay_out`]), with the suggestions placed on
/// them.
#[derive(Debug, Clone)]
pub struct LaidOut<'a> {
    pub rows: Vec<LaidRow>,
    /// The version's `ROW:` suggestions ([`LaidRow::suggestions`] indexes
    /// these); none when the rows are laid out on the original.
    pub placed: Vec<PlacedItem>,
    /// Each member's script as the rows read it (indexed by member): the
    /// version's live script, the original's file (C8). Borrowed where they
    /// already exist (the live document, the other member).
    pub scripts: Vec<Cow<'a, Script>>,
}

impl LoadedPair {
    /// The other member's index.
    pub fn other_index(&self) -> usize {
        if self.this == 0 { 1 } else { 0 }
    }

    /// Whether the script it was loaded for is the version
    /// ([`Links::is_version`]): the side that is written and gets `ROW:`
    /// suggestions. Rows shown on the original swap this and other, and no
    /// row there needs work.
    pub fn this_is_version(&self) -> bool {
        self.links.is_version(self.this)
    }

    /// Whether the script it was loaded for is the member `created:` marks.
    pub fn this_is_made(&self) -> bool {
        self.links.made() == Some(self.this)
    }

    /// The pair's rows as they read now ([`lay_out`]), for the script the
    /// pair was loaded for: `this` is its file as opened, `session` and
    /// `doc` its session and live document. On the version: its live
    /// document and the `ROW:` suggestions of its session. On the original:
    /// its file (C8) and the version's file, with no suggestions (the
    /// version's session isn't opened). One `is_caps` for both members, the
    /// original's (plan, "Status"). Every host calls this (fsio's `Opened`
    /// through [`crate::opened::Opened::lay_out_pair`], the app with its own session).
    pub fn lay_out<'a>(&'a self, this: &FdxFile, session: &Session, doc: &'a Document) -> LaidOut<'a> {
        let (original, version, caps_of, placed): (Cow<Script>, Cow<Script>, &FdxFile, _) = if self.this_is_version() {
            let placed = placed_items(session, doc, &self.links.id);
            (Cow::Borrowed(&self.other_script), Cow::Borrowed(doc.script()), &self.other, placed)
        } else {
            (Cow::Owned(this.to_script()), Cow::Borrowed(&self.other_script), this, vec![])
        };
        let is_caps = |k: &ElementKind| caps_of.is_caps(k.name());
        let only: Vec<Placed> = placed.iter().map(|p| p.placed.clone()).collect();
        let rows = lay_out(&self.links, &original, &version, &only, &is_caps);
        let (o_m, v_m) = self.links.sides();
        let mut scripts = vec![Cow::Owned(Script::default()), Cow::Owned(Script::default())];
        scripts[o_m] = original;
        scripts[v_m] = version;
        LaidOut { rows, placed, scripts }
    }

    /// The resolver for `ROW:` suggestions on `doc`, the live document of
    /// the script `this` (its file as opened): on the version, the rows
    /// whose original side reads an entry's `ROW TEXT`, read from the
    /// original's file (C8); resolves nothing (with the reason) unless the
    /// pair describes `this`'s bytes (C6). On the original every `ROW:`
    /// entry is not found: rows are suggested on the version.
    pub fn resolver(&self, this: &FdxFile, doc: &Document) -> Box<RowResolver<'static>> {
        if !self.this_is_version() {
            let version = &self.links.members[self.links.sides().1].file;
            return refusing(format!("suggestions on rows belong to {version}"));
        }
        if self.links.members[self.this].hash != sessionfile::hash(this.raw()) {
            let file = &self.links.members[self.this].file;
            return refusing(format!("{file} changed outside Scriptorium; open it again"));
        }
        let is_caps = |k: &ElementKind| self.other.is_caps(k.name());
        let resolver = Resolver::new(&self.links, &self.other_script, doc.script(), &is_caps);
        Box::new(move |row: &RowRef, texts: &[Vec<Line>], para: &[ParaId]| {
            let texts: Vec<Vec<String>> = texts.iter().map(|t| t.iter().map(|l| l.text.clone()).collect()).collect();
            let q = RowQuery { links_id: &row.id, row_text: &row.texts, texts: &texts, para };
            resolver.candidates(&q).map_err(|e| e.text())
        })
    }

    /// The rows laid out ([`Self::lay_out`]) as one value for the tools
    /// (`scriptorium_tools::LinksCtx::from`).
    pub fn view<'a>(&'a self, this: &FdxFile, session: &Session, doc: &'a Document) -> PairView<'a> {
        let laid = self.lay_out(this, session, doc);
        PairView {
            links: &self.links,
            path: &self.path,
            this: self.this,
            rows: laid.rows,
            scripts: laid.scripts,
            placed: laid.placed.iter().map(|p| (p.review, p.item)).collect(),
        }
    }

    /// [`Self::resolver`] for review's open path (`sessionfile::resume`).
    pub fn rows_factory<'a>(&'a self, this: &'a FdxFile) -> Box<RowResolverFactory<'a>> {
        Box::new(move |doc: &Document| -> Box<RowResolver<'a>> { self.resolver(this, doc) })
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

/// The pair in the links file at `at`, which names `script_name` (in `dir`;
/// `this` is its file as read), checked against the bytes of both files and
/// re-found where they differ (C6): lock, parse, [`refind_locked`]. Holds
/// the links lock from reading the links file to writing it, and releases
/// it before returning.
///
/// Errors: a links file that can't be read (nothing is rewritten), the
/// other member missing (with the fix, and any `.fdx` in the folder holding
/// the recorded bytes) or not a Final Draft file, `this` changed on disk.
pub(crate) fn load_pair_at(
    at: &Path,
    dir: &Path,
    script_name: &str,
    this: &FdxFile,
    notes: &mut Vec<String>,
) -> Result<LoadedPair, String> {
    let links_name = crate::opened::file_name(at);
    let guard = lock_links(at, notes)?;
    let text = std::fs::read_to_string(at).map_err(|e| format!("{}: {e}", at.display()))?;
    let pair = links::parse(&text).map_err(|e| format!("{links_name} can't be read ({e}); it is left as it is"))?;
    let this_m = pair.member_of(script_name).ok_or_else(|| format!("{links_name} doesn't name {script_name}"))?;
    let found = refind_locked(&guard, dir, pair, &text, this_m, &dir.join(script_name), this)?;
    drop(guard);
    notes.extend(found.msgs.iter().cloned());
    let other_hash = sessionfile::hash(found.other.raw());
    let other_script = found.other.to_script();
    Ok(LoadedPair {
        path: at.to_path_buf(),
        links: found.links,
        links_hash: found.links_hash,
        this: this_m,
        other_script,
        other: found.other,
        other_path: found.other_path,
        other_hash,
        refound: found.msgs,
        maybe_made: false,
    })
}

/// What [`refind_locked`] found.
pub(crate) struct Refound {
    /// The pair, describing both files' bytes now (written if it changed).
    pub links: Links,
    /// The other member as read (and checked unchanged before any write).
    pub other: FdxFile,
    pub other_path: PathBuf,
    /// `sessionfile::hash` of the links file on disk now: `text`, or what
    /// was written.
    pub links_hash: String,
    /// One message per member re-found ("PURPLE LIST.fdx changed outside
    /// Scriptorium: …"); empty when nothing changed and nothing was written.
    pub msgs: Vec<String>,
}

/// Re-find each member of `links` (read from `text`, the links file `guard`
/// holds) whose file isn't the bytes the links file describes, and write
/// the links file if anything changed. `this` is member `this_m`'s file as
/// the caller read it (from `this_path`); the other member is read here.
/// Just before writing, both files are read again: `this` changed is an
/// error (the caller's ids would no longer be what the links file says);
/// the other changed starts over. The other member not parsing (Final Draft
/// may be halfway through saving it) is tried again after a pause too.
/// Nothing is written unless every check passed.
pub(crate) fn refind_locked(
    guard: &LinksGuard,
    dir: &Path,
    links: Links,
    text: &str,
    this_m: usize,
    this_path: &Path,
    this: &FdxFile,
) -> Result<Refound, String> {
    let at = guard.path();
    let links_name = crate::opened::file_name(at);
    let this_name = links.members[this_m].file.clone();
    let other_m = if this_m == 0 { 1 } else { 0 };
    let other_file = links.members[other_m].file.clone();
    let other_path = dir.join(&other_file);
    let mut unparsed = String::new();
    for attempt in 0..TRIES {
        if attempt > 0 {
            std::thread::sleep(RETRY_PAUSE);
        }
        let other_raw = std::fs::read_to_string(&other_path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => missing(dir, text, &links_name, other_m, &links),
            _ => format!("{}: {e}", other_path.display()),
        })?;
        let other = match FdxFile::parse(other_raw) {
            Ok(f) => f,
            Err(e) => {
                unparsed =
                    format!("{other_file} can't be read ({e}); if Final Draft is saving it, try again in a moment");
                continue;
            }
        };
        unparsed.clear();

        let mut pair = links.clone();
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
        let mut links_hash = sessionfile::hash(text);
        if !msgs.is_empty() {
            // Just before writing: both files still the bytes re-found.
            let now = std::fs::read_to_string(this_path).map_err(|e| format!("{}: {e}", this_path.display()))?;
            if now != this.raw() {
                return Err(format!("{this_name} changed while it was being opened; open it again"));
            }
            let again = std::fs::read_to_string(&other_path).map_err(|e| format!("{}: {e}", other_path.display()))?;
            if again != other.raw() {
                continue;
            }
            let out = links::write(&pair).map_err(|e| format!("{links_name}: {e}"))?;
            write_atomic_no_link(at, out.as_bytes()).map_err(|e| format!("{}: {e}", at.display()))?;
            links_hash = sessionfile::hash(&out);
        }
        return Ok(Refound { links: pair, other, other_path, links_hash, msgs });
    }
    if !unparsed.is_empty() {
        return Err(unparsed);
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

    use scriptorium_versions::{Member, blank_copy};

    const SCRIPT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Scene Heading">
      <Text>EXT. FARM HOUSE - NIGHT</Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text>Laura smiles.</Text>
    </Paragraph>
    <Paragraph Type="Character">
      <Text>CESAR</Text>
    </Paragraph>
    <Paragraph Type="Dialogue">
      <Text>I am going to town tomorrow.</Text>
    </Paragraph>
  </Content>
</FinalDraft>
"#;

    /// A folder with `film.fdx`, its placeholder version `film.es.fdx` and
    /// `film.es.links.txt`; removed on drop.
    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("scriptorium-pair-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let dir = dir.canonicalize().unwrap();
            std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
            let fdx = FdxFile::parse(SCRIPT.to_string()).unwrap();
            let a = Member {
                file: "film.fdx".into(),
                language: Some("en".into()),
                hash: sessionfile::hash(SCRIPT),
                created: None,
            };
            let b =
                Member { file: "film.es.fdx".into(), language: Some("es".into()), hash: String::new(), created: None };
            let (script, mut pair) =
                blank_copy(&fdx.to_script(), "lk-test", a, b, |k: &ElementKind| fdx.is_caps(k.name())).unwrap();
            let written = scriptorium_fdx::clear_text_offsets(&fdx.write_script(&script).unwrap());
            pair.members[1].hash = sessionfile::hash(&written);
            std::fs::write(dir.join("film.es.fdx"), &written).unwrap();
            std::fs::write(dir.join("film.es.links.txt"), links::write(&pair).unwrap()).unwrap();
            Self(dir)
        }
        fn at(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
        fn read(&self, name: &str) -> String {
            std::fs::read_to_string(self.at(name)).unwrap()
        }
        fn links(&self) -> Links {
            links::parse(&self.read("film.es.links.txt")).unwrap()
        }
        /// Load the pair for `name`, read from disk now.
        fn load(&self, name: &str, notes: &mut Vec<String>) -> Result<LoadedPair, String> {
            let this = FdxFile::parse(self.read(name)).unwrap();
            load_pair_at(&self.at("film.es.links.txt"), &self.0, name, &this, notes)
        }
        /// What Final Draft does when a line is typed into the version.
        fn translate(&self) -> String {
            let es = self.read("film.es.fdx").replacen(
                "<Paragraph Type=\"Dialogue\">\n      <Text></Text>",
                "<Paragraph Type=\"Dialogue\">\n      <Text>Mañana voy al pueblo.</Text>",
                1,
            );
            assert_ne!(es, self.read("film.es.fdx"));
            std::fs::write(self.at("film.es.fdx"), &es).unwrap();
            es
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The original with a line added before the cue.
    fn english_with_a_dog() -> String {
        SCRIPT.replace(
            "    <Paragraph Type=\"Character\">",
            "    <Paragraph Type=\"Action\">\n      <Text>A dog barks.</Text>\n    </Paragraph>\n    <Paragraph \
             Type=\"Character\">",
        )
    }

    #[test]
    fn the_version_side_is_found_again() {
        let d = Dir::new("version-side");
        let es = d.translate();
        // Loaded for the original: the other member (the version) changed.
        let mut notes = vec![];
        let p = d.load("film.fdx", &mut notes).unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("film.es.fdx changed outside Scriptorium"), "{notes:?}");
        assert_eq!(p.refound, notes);
        assert_eq!(p.other_hash, sessionfile::hash(&es));
        assert_eq!(p.links_hash, sessionfile::hash(&d.read("film.es.links.txt")), "the text written");
        assert_eq!(p.other_script, FdxFile::parse(es.clone()).unwrap().to_script());
        assert_eq!(d.links().members[1].hash, sessionfile::hash(&es));
        // Found once: nothing more to say or write.
        let before = d.read("film.es.links.txt");
        let mut notes = vec![];
        d.load("film.es.fdx", &mut notes).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(d.read("film.es.links.txt"), before);
    }

    #[test]
    fn both_members_changed_at_once() {
        let d = Dir::new("both");
        let es = d.translate();
        let en = english_with_a_dog();
        std::fs::write(d.at("film.fdx"), &en).unwrap();
        let mut notes = vec![];
        let p = d.load("film.es.fdx", &mut notes).unwrap();
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes.iter().any(|n| n.starts_with("film.fdx changed") && n.contains("1 new")), "{notes:?}");
        assert!(notes.iter().any(|n| n.starts_with("film.es.fdx changed")), "{notes:?}");
        let links = d.links();
        assert_eq!(links.members[0].hash, sessionfile::hash(&en));
        assert_eq!(links.members[1].hash, sessionfile::hash(&es));
        assert_eq!(links.rows.len(), 5, "the dog has a row of its own");
        assert_eq!(p.links, links);
    }

    #[test]
    fn this_changed_during_the_run_is_an_error_and_nothing_is_written() {
        let d = Dir::new("this-changed");
        let before = d.read("film.es.links.txt");
        // The caller read other bytes than are on disk now (and than the
        // links file describes): re-find would describe bytes no longer there.
        let read = FdxFile::parse(english_with_a_dog()).unwrap();
        let e = load_pair_at(&d.at("film.es.links.txt"), &d.0, "film.fdx", &read, &mut vec![]).unwrap_err();
        assert!(e.contains("film.fdx changed while it was being opened"), "{e}");
        assert_eq!(d.read("film.es.links.txt"), before);
    }

    #[test]
    fn another_member_that_doesnt_parse_is_tried_again_then_refused() {
        let d = Dir::new("unparsed");
        let es = d.translate();
        let before = d.read("film.es.links.txt");
        std::fs::write(d.at("film.es.fdx"), "<FinalDraft").unwrap();
        let e = d.load("film.fdx", &mut vec![]).unwrap_err();
        assert!(e.contains("film.es.fdx can't be read") && e.contains("if Final Draft is saving it, try again"), "{e}");
        assert_eq!(d.read("film.es.links.txt"), before, "nothing rewritten");

        // Final Draft finishes saving while re-find waits: found on a retry.
        let at = d.at("film.es.fdx");
        let saver = std::thread::spawn(move || {
            std::thread::sleep(RETRY_PAUSE + RETRY_PAUSE / 2);
            std::fs::write(at, es).unwrap();
        });
        let mut notes = vec![];
        let p = d.load("film.fdx", &mut notes);
        saver.join().unwrap();
        let p = p.unwrap();
        assert!(notes[0].starts_with("film.es.fdx changed outside Scriptorium"), "{notes:?}");
        assert_eq!(d.links().members[1].hash, p.other_hash);
    }
}
