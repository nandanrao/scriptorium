//! A script opened with its saved session, the way the app opens it.
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
use scriptorium_review::sessionfile::{self, Resumed, SessionFile};

use crate::write_atomic;

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
            if given {
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
        if let Resumed::Discarded(why) = &opened.how {
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
        Ok(Self { fdx, raw, session, doc, base, session_path, dir, script_name, given, pin })
    }

    /// Open, and pick the one review a command works on: the file given
    /// (positionally or with `--review`), else the only one attached.
    ///
    /// `--review` (`pick`) is, in this order: a file, when absolute; a file
    /// in the script's folder, when relative; an attached review's name or
    /// stored path; and, only with `from_cwd`, a file from the current folder
    /// (the command line's convention). The MCP server passes `from_cwd`
    /// false, so it never attaches a file from its own folder.
    pub fn open_one(
        script: &str,
        file: Option<&str>,
        pick: Option<&str>,
        from_cwd: bool,
        notes: &mut Vec<String>,
    ) -> Result<(Self, usize), String> {
        let dir = script_dir(script)?;
        let in_dir = pick.and_then(|p| {
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
            o.find_attached(p).ok_or_else(|| format!("no attached review matches {p:?}. Attached: {}", o.names()))?
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

    /// Save the session (and pin new reviews' ids into their files first).
    pub fn save(&self) -> Result<(), String> {
        // Attached now: pin each new review's id into its file, so later edits
        // to the file don't orphan the decisions stored under that id. First,
        // so a session never names a review whose file couldn't be pinned.
        for (at, text) in &self.pin {
            write_atomic(at, text.as_bytes()).map_err(|e| format!("{}: {e}", at.display()))?;
        }
        let json = SessionFile::new(&self.raw, self.session.attached(), &self.base, self.doc.log()).to_json();
        write_atomic(&self.session_path, json.as_bytes())
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

/// Read and parse a Final Draft file.
pub fn load(path: &str) -> Result<FdxFile, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    FdxFile::parse(raw).map_err(|e| format!("{path}: {e}"))
}

/// The script's folder, canonical (so review paths compare and relativize).
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
