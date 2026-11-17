//! A script opened with its saved session, the way the CLI commands and the
//! chat host open it (the app has its own, see `script_view.rs`).
//!
//! The session (`<script>.scriptorium.json`) lists the review files attached
//! to the script; [`Opened::open`] reads them (raw text, as on disk) and
//! resumes the session with review's [`sessionfile::resume`]. Nothing here
//! writes until [`Opened::save`] or [`Opened::write_review`] is called, and
//! both go through [`write_atomic`].

use std::path::{Path, PathBuf};

use scriptorium_core::doc::Event;
use scriptorium_core::{Document, ElementKind, ParaId, Selection};
use scriptorium_fdx::FdxFile;
use scriptorium_review::Session;
use scriptorium_review::sessionfile::{self, Resumed, SessionFile, aside};

use crate::versions::Pair;
use crate::{Folders, write_atomic, write_atomic_no_link};

/// A script, its review files and the saved session: the review files
/// attached in the session, plus any given on the command line.
pub struct Opened {
    pub fdx: FdxFile,
    /// The script file's contents.
    pub raw: String,
    pub session: Session,
    /// The script as it is now (decisions and edits applied).
    pub doc: Document,
    pub base: Vec<Event>,
    pub session_path: PathBuf,
    /// The script's folder, canonical (review paths are stored relative to it).
    pub dir: PathBuf,
    /// The script's file name, e.g. `film.fdx`.
    pub script_name: String,
    /// Index into `session.reviews` of each review file given on the command line.
    pub given: Vec<usize>,
    /// Review files newly attached here that had no `Id:` header: their new
    /// text, with the id spliced in, written first when the session is saved.
    pin: Vec<(PathBuf, String)>,
    /// The saved session, as read, when it was made on other bytes of the
    /// script and held only decisions the review files have: opened fresh,
    /// and moved to a set-aside file by the first [`Self::save`].
    aside: Option<String>,
    /// The script's pair, if a links file beside it names it. When this is
    /// the version Scriptorium made, every [`Self::save`] writes progress
    /// into the review files ([`crate::versions::write_progress`]), so the
    /// decisions outlive a Final Draft save of the script (plan C2).
    pair: Option<Pair>,
    /// Write progress on save: this is the made version, or may be (a links
    /// file naming it doesn't parse), see [`crate::versions::Found`].
    progress: bool,
}

impl Opened {
    /// Open `script` with its session and the review files in `extra` (which
    /// are attached when the session is saved). A saved session that can't
    /// be replayed is an error, so it is never overwritten. Things to tell the
    /// user go to `notes`.
    pub fn open(script: &str, extra: &[&str], notes: &mut Vec<String>) -> Result<Self, String> {
        Self::open_with(script, extra, false, notes)
    }

    /// Open for reading only (`info`, `dump`): a saved session that can't be
    /// used is a note, and the script is read as written. Notes about review
    /// files are left out (these commands don't show reviews). Never save the
    /// result.
    pub fn open_read_only(script: &str, notes: &mut Vec<String>) -> Result<Self, String> {
        Self::open_with(script, &[], true, notes)
    }

    fn open_with(script: &str, extra: &[&str], read_only: bool, notes: &mut Vec<String>) -> Result<Self, String> {
        let fdx = load(script)?;
        let raw = fdx.raw().to_string();
        let script_name = file_name(Path::new(script));
        let dir = script_dir(script)?;
        let s = fdx.to_script();
        let session_path = dir.join(sessionfile::file_name_for(&script_name));
        let saved = std::fs::read_to_string(&session_path).ok();
        let attached: Vec<String> = saved
            .as_deref()
            .and_then(|j| SessionFile::from_json(j).ok())
            .map(|f| f.reviews.into_iter().map(|a| a.path).collect())
            .unwrap_or_default();

        let was_attached = attached.clone();
        // (where it is, how it's stored, given on the command line)
        let mut paths: Vec<(PathBuf, String, bool)> = vec![];
        for stored in attached {
            let at = sessionfile::resolve_path(&dir, &stored);
            let at = at.canonicalize().unwrap_or(at);
            paths.push((at, stored, false));
        }
        for r in extra {
            let at = Path::new(r).canonicalize().map_err(|e| format!("{r}: {e}"))?;
            match paths.iter_mut().find(|(p, _, _)| *p == at) {
                Some(found) => found.2 = true,
                None => paths.push((at.clone(), sessionfile::stored_path(&dir, &at), true)),
            }
        }
        let mut sources = vec![];
        // Stored paths of the files given here, and (where it is, how it's
        // stored) for those not attached before.
        let (mut given_paths, mut newly): (Vec<String>, Vec<(PathBuf, String)>) = (vec![], vec![]);
        for (at, stored, given) in paths {
            // Raw contents: the session compares their hash with what it saved.
            let text = match std::fs::read_to_string(&at) {
                Ok(t) => t,
                Err(e) if !given => {
                    if !read_only {
                        notes.push(format!("attached review {stored} can't be read ({e}); left out"));
                    }
                    continue;
                }
                Err(e) => return Err(format!("{}: {e}", at.display())),
            };
            if !given && scriptorium_review::file::is_links_text(&text) {
                // Listed in the session, but a links file: never read (or
                // written) as a review.
                if !read_only {
                    notes.push(format!("attached review {stored} is a links file, not a review; left out"));
                }
                continue;
            }
            if given {
                // A links file named as a review is refused, not attached.
                scriptorium_review::file::refuse_links(&text).map_err(|e| format!("{stored}: {e}"))?;
                given_paths.push(stored.clone());
                if !was_attached.contains(&stored) {
                    newly.push((at, stored.clone()));
                }
            }
            sources.push(sessionfile::ReviewSource { name: stored.clone(), path: stored, text });
        }
        let caps = |k: &ElementKind| fdx.is_caps(k.name());
        // Always checked: reader 1 also misnumbered files without DualDialogue
        // (nested paragraphs in scene summaries and arc beats), and it's cheap.
        let reader_1 = Some(fdx.legacy_script());
        let opened = sessionfile::resume(sources, s, reader_1.as_ref(), &raw, saved.as_deref(), &caps, &script_name);
        let mut aside = None;
        if let Resumed::Discarded(_) = &opened.how
            && !read_only
            && let Some(saved) = saved.as_deref()
            && let Ok(file) = SessionFile::from_json(saved)
            && !file.is_for(&raw)
        {
            // The script's bytes changed (Final Draft saved it). Started
            // fresh from the files; that's safe only if the old session
            // holds nothing they don't.
            if let Err(kept) = aside::only_decisions(&file, &opened.doc, &opened.session) {
                return Err(format!(
                    "{script_name} changed outside Scriptorium, and its session holds {}. Open the script in the \
                     Scriptorium app: it keeps that session as a set-aside file and starts fresh.",
                    kept.what()
                ));
            }
            notes.push(format!(
                "{script_name} changed outside Scriptorium; its session held only decisions, which are back from the \
                 review files. The old session file is set aside when a change is saved."
            ));
            aside = Some(saved.to_string());
        } else if let Resumed::Discarded(why) = &opened.how {
            if !read_only {
                return Err(format!(
                    "can't use the saved session {}: {why}. Move the session file aside to start fresh.",
                    session_path.display()
                ));
            }
            notes.push(format!(
                "the saved session {} isn't used ({why}); showing the script as written",
                session_path.display()
            ));
        }
        // Mapped by path, not id: two files may share an `Id:`, and a given
        // file refused as a duplicate must not act on the other one.
        let mut given = vec![];
        for stored in &given_paths {
            match opened.session.reviews.iter().position(|r| r.path == *stored) {
                Some(r) => given.push(r),
                None => {
                    let why = opened.notes.iter().find(|n| n.contains(stored.as_str())).cloned();
                    return Err(
                        why.unwrap_or_else(|| format!("{stored} couldn't be opened with this script's reviews"))
                    );
                }
            }
        }
        if !read_only {
            notes.extend(opened.notes.iter().cloned());
        }
        let sessionfile::ResumedSession { mut session, doc, base, .. } = opened;
        let mut pin = vec![];
        for (at, stored) in newly {
            if let Some(review) = session.reviews.iter_mut().find(|r| r.path == stored)
                && let Some(text) = review.pin_id()
            {
                pin.push((at, text));
            }
        }
        let found = crate::versions::find_pair(&dir, &script_name);
        if !read_only {
            notes.extend(found.notes);
        }
        let (pair, progress) = (found.pair, found.maybe_made);
        Ok(Self { fdx, raw, session, doc, base, session_path, dir, script_name, given, pin, aside, pair, progress })
    }

    /// Open, and pick the one review a command works on: the file given
    /// (positionally or with `--review`), else the only one attached.
    ///
    /// `--review` (`pick`) is, in this order: a file, when absolute; a file
    /// in the script's folder, when relative; an attached review's name or
    /// stored path; and, only with `from_cwd`, a file from the current folder
    /// (the command line's convention). The MCP server passes `from_cwd`
    /// false, so it never attaches a file from its own folder, and a file
    /// it names is attached only if it is a review inside the script's
    /// folder ([`crate::review_in_folder`], starting with `SCRIPT REVIEW`).
    pub fn open_one(
        script: &str,
        file: Option<&str>,
        pick: Option<&str>,
        from_cwd: bool,
        notes: &mut Vec<String>,
    ) -> Result<(Self, usize), String> {
        let dir = script_dir(script)?;
        // Why an agent's pick isn't a file to attach, told if it isn't an
        // attached review's name either.
        let mut not_file = None;
        let in_dir = pick.and_then(|p| {
            if !from_cwd {
                // An agent's pick (MCP): a file is attached only if it is a
                // review inside the script's folder; anything else can only
                // name a review already attached.
                return match agent_review(&dir, p, Folders::MustExist) {
                    Ok((at, true)) => Some(at.to_string_lossy().into_owned()),
                    Ok((_, false)) => None,
                    Err(e) => {
                        not_file = Some(e);
                        None
                    }
                };
            }
            let at = Path::new(p);
            let at = if at.is_absolute() { at.to_path_buf() } else { dir.join(at) };
            at.is_file().then(|| at.to_string_lossy().into_owned())
        });
        let extra: Vec<&str> = file.into_iter().chain(in_dir.as_deref()).collect();
        // Kept aside until it's settled which open is used.
        let mut first = vec![];
        let o = Self::open(script, &extra, &mut first)?;
        if let Some(p) = pick
            && from_cwd
            && o.given.is_empty()
            && o.find_attached(p).is_none()
            && Path::new(p).is_file()
        {
            // Not attached, nor in the script's folder: a file from here.
            drop(o);
            let o = Self::open(script, &[p], notes)?;
            let r = o.given[0];
            return Ok((o, r));
        }
        notes.append(&mut first);
        let r = if let Some(&r) = o.given.first() {
            r
        } else if let Some(p) = pick {
            o.find_attached(p).ok_or_else(|| match &not_file {
                // Named like a file (a path or a review file name): say why
                // it can't be used as one.
                Some(why) if p.contains(['/', '\\', '.']) => {
                    format!("no attached review matches {p:?}, and as a file: {why}. Attached: {}", o.names())
                }
                _ => format!("no attached review matches {p:?}. Attached: {}", o.names()),
            })?
        } else {
            match o.session.reviews.len() {
                0 => return Err(format!("no review files attached to {script}; attach one first")),
                1 => 0,
                _ => return Err(format!("several reviews are attached; name one with review: {}", o.names())),
            }
        };
        Ok((o, r))
    }

    /// The attached review `p` names: its stored path, its name, or its file
    /// name (or the start of it), ignoring case.
    fn find_attached(&self, p: &str) -> Option<usize> {
        let p_lower = p.to_lowercase();
        self.session.reviews.iter().position(|r| {
            r.path == p
                || r.name.to_lowercase() == p_lower
                || file_name(Path::new(&r.path)).to_lowercase() == p_lower
                || file_name(Path::new(&r.path)).to_lowercase().starts_with(&p_lower)
        })
    }

    pub fn names(&self) -> String {
        self.session.reviews.iter().map(|r| r.path.as_str()).collect::<Vec<_>>().join(", ")
    }

    /// Element kinds this script shows in capitals.
    pub fn is_caps(&self, k: &ElementKind) -> bool {
        self.fdx.is_caps(k.name())
    }

    /// Where an attached review's file is (its stored path resolved).
    pub fn review_path(&self, r: usize) -> PathBuf {
        sessionfile::resolve_path(&self.dir, &self.session.reviews[r].path)
    }

    /// Write a review file's new text. Call it before [`Self::save`], so a
    /// session never records text its review file doesn't hold.
    pub fn write_review(&mut self, path: &Path, text: &str) -> Result<(), String> {
        write_atomic(path, text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
        // Written whole: a pending `Id:` pin for this file is part of `text` (or moot).
        let at = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.pin.retain(|(p, _)| *p != at);
        Ok(())
    }

    /// The script's pair, if a links file beside it names it.
    pub fn pair(&self) -> Option<&Pair> {
        self.pair.as_ref()
    }

    /// Save the session (and pin new reviews' ids into their files first).
    /// For the version Scriptorium made (or one that may be, its links file
    /// broken), each attached review's progress is written into its file
    /// first too. A session opened fresh because the
    /// old one no longer fit the script is moved aside first
    /// ([`crate::set_aside_path`]), never overwritten. The caller holds the
    /// session lock.
    pub fn save(&mut self) -> Result<(), String> {
        // First, before any file is written: only the session file judged at
        // open is set aside; anything else there now (another program saved
        // meanwhile) is left alone and this save refused.
        if let Some(was) = &self.aside {
            match std::fs::read_to_string(&self.session_path) {
                Ok(now) if now == *was => {
                    let to = crate::set_aside_path(&self.dir, &self.script_name);
                    std::fs::rename(&self.session_path, &to).map_err(|e| {
                        format!("setting {} aside as {}: {e}", self.session_path.display(), to.display())
                    })?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                _ => {
                    return Err(format!(
                        "{} changed while this command ran; run it again",
                        self.session_path.display()
                    ));
                }
            }
            self.aside = None;
        }
        // Attached now: pin each new review's id into its file, so later edits
        // to the file don't orphan the decisions stored under that id. Before
        // the session, so it never names a review whose file couldn't be
        // pinned. A pin is dropped only once written.
        while let Some((at, text)) = self.pin.first() {
            write_atomic(at, text.as_bytes()).map_err(|e| format!("{}: {e}", at.display()))?;
            self.pin.remove(0);
        }
        if self.progress {
            // Notes (files agents can't write) have nowhere to go from here;
            // their decisions stay in the session, and opening after a Final
            // Draft save refuses rather than lose them.
            crate::versions::write_progress(&self.dir, &mut self.session, &self.doc)?;
        }
        let json = SessionFile::new(&self.raw, self.session.attached(), &self.base, self.doc.log()).to_json();
        write_atomic_no_link(&self.session_path, json.as_bytes())
            .map_err(|e| format!("{}: {e}", self.session_path.display()))?;
        Ok(())
    }

    pub fn sel(&self) -> Selection {
        Selection::cursor(self.doc.start())
    }

    /// A paragraph's text now ("" if it's gone).
    pub fn text(&self, para: ParaId) -> String {
        self.doc.paragraph(para).map(|p| p.text()).unwrap_or_default()
    }
}

/// What a writer does to let agents use a review file that isn't one yet
/// (or is elsewhere): told in every refusal, whichever program refused.
pub const ASK_TO_ATTACH: &str =
    "ask the writer to attach it (File › Add review in the app, or `scriptorium reviews <script> add <file>`)";

/// The refusal for a file an agent named that exists but doesn't read as a
/// review (every host words it the same).
pub fn not_a_review(path: &str) -> String {
    format!(
        "{path} isn't a review file yet (it doesn't start with {}); {ASK_TO_ATTACH}",
        scriptorium_review::file::FORMAT_HEADER
    )
}

/// A review file an agent names that isn't attached: where it is under the
/// one rule ([`crate::agent_file_in_folder`]: a `*.review.txt` inside the
/// script's folder `dir`), and whether it exists; an existing file must
/// start with `SCRIPT REVIEW` ([`scriptorium_review::file::is_review_text`]).
/// With [`Folders::MayCreate`] its folder may be missing; the caller makes
/// it when it writes. The CLI `tool` and the MCP server use this; the chat
/// agent applies the same checks through its store.
pub fn agent_review(dir: &Path, path: &str, folders: Folders) -> Result<(PathBuf, bool), String> {
    let at = crate::agent_file_in_folder(dir, path, ".review.txt", folders)?;
    if !at.exists() {
        return Ok((at, false));
    }
    let text = std::fs::read_to_string(&at).map_err(|e| format!("{path}: {e}"))?;
    if !scriptorium_review::file::is_review_text(&text) {
        return Err(not_a_review(path));
    }
    Ok((at, true))
}

/// Whether an agent may write into the attached file at `path`: inside the
/// script's folder `dir`, no hidden part, not a script, session or lock
/// file ([`crate::agent_write_place`]), and its text on disk reads as a
/// review (a file the writer attached starts with `SCRIPT REVIEW` whatever
/// its name; a session file listing `run.sh` can't make an agent append to
/// it). Every agent write into a review goes through this: the CLI `tool`,
/// the MCP server, and the chat agent (its stores' default).
pub fn agent_may_write(dir: &Path, path: &Path) -> Result<(), String> {
    let at = crate::agent_write_place(dir, path)?;
    let text = std::fs::read_to_string(&at).unwrap_or_default();
    if !scriptorium_review::file::is_review_text(&text) {
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        // A text file may be a review missing its first line; anything
        // else (`run.sh`) gets no suggestion to make it one.
        let text_file = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("txt"));
        let remedy = text_file.then_some(
            "The writer can add `SCRIPT REVIEW v1` as its first line, or let the assistant start its own review.",
        );
        return Err(crate::not_writable(
            path,
            "doesn't start with SCRIPT REVIEW, so agents treat it as read-only",
            remedy,
            &dir,
        ));
    }
    Ok(())
}

/// Read and parse a Final Draft file.
pub fn load(path: &str) -> Result<FdxFile, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    FdxFile::parse(raw).map_err(|e| format!("{path}: {e}"))
}

/// The script's folder, canonical (so review paths compare and relativize).
/// A script opened through a link uses the folder of the file it points at:
/// that is where its session, reviews and output go.
pub fn script_dir(script: &str) -> Result<PathBuf, String> {
    let p = Path::new(script).canonicalize().map_err(|e| format!("{script}: {e}"))?;
    Ok(p.parent().map(Path::to_path_buf).unwrap_or_default())
}

pub fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

pub fn no_reviews(script: &str) -> String {
    format!("no review files attached to {script}. Attach one with `scriptorium reviews {script:?} add <review.txt>`")
}

/// Something no other new review file will share: the file's path, the time
/// and the process id (this is the shell; [`new_review_id`] stays pure).
pub fn review_salt(path: &Path) -> String {
    let at = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{}\n{nanos}\n{}", at.display(), std::process::id())
}

/// The `Id:` for a new review file: a hash of the text it starts with and
/// `salt`, so two files started with the same suggestion don't share an id.
pub fn new_review_id(text: &str, salt: &str) -> String {
    sessionfile::hash(&format!("{salt}\n{text}"))[..12].to_string()
}
