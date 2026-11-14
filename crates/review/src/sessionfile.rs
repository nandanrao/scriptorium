//! The saved review session: the review files attached to a script, the
//! document's event log, and a fingerprint of the script it applies to.
//!
//! The app and the CLI both read and write it (as `<script>.scriptorium.json`
//! next to the script), so a person in the app and an agent on the command
//! line work on the same decisions. Replaying the log over the script (with the
//! review files' saved decisions applied) gives back every edit and decision,
//! with full undo history.
//!
//! Pure: strings in, strings out.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use scriptorium_core::doc::Event;
use scriptorium_core::{Document, Script};
use serde::{Deserialize, Serialize};

use crate::{Review, Session};

/// Version 2 added the attached review files and the starting events.
/// Version 1 files are still read.
pub const VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFile {
    pub version: u32,
    /// [`hash`] of the script file the events apply to.
    pub script_hash: String,
    /// The review files attached to the script, so opening the script brings
    /// them back. Empty in version 1 files.
    #[serde(default)]
    pub reviews: Vec<Attached>,
    /// The events that make the starting document: the decisions saved in the
    /// review files attached when the session began. Undo stops there.
    /// `None` in version 1 files, where the starting point was the review files
    /// passed on the command line, so it's rebuilt from the reviews opened now.
    #[serde(default)]
    pub base: Option<Vec<Event>>,
    pub events: Vec<Event>,
}

/// A review file attached to the script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attached {
    /// Relative to the script's folder when inside it, else absolute
    /// (see [`stored_path`]).
    pub path: String,
    /// The review's id when it was attached ([`Review::id`]). Its saved
    /// decisions are already in the session, in `base` or in `events`; a file
    /// whose contents changed since has a new id.
    pub id: String,
}

impl SessionFile {
    pub fn new(script_contents: &str, reviews: Vec<Attached>, base: &[Event], events: &[Event]) -> Self {
        Self {
            version: VERSION,
            script_hash: hash(script_contents),
            reviews,
            base: Some(base.to_vec()),
            events: events.to_vec(),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("session serializes")
    }

    pub fn from_json(json: &str) -> Result<Self, String> {
        let f: Self = serde_json::from_str(json).map_err(|e| format!("unreadable session file: {e}"))?;
        if !(1..=VERSION).contains(&f.version) {
            return Err(format!("session file version {} is not supported (made by a newer Scriptorium?)", f.version));
        }
        Ok(f)
    }

    /// Whether this session was made on exactly this script.
    pub fn is_for(&self, script_contents: &str) -> bool {
        self.script_hash == hash(script_contents)
    }
}

/// The session file's name for a script file name: `X.fdx` → `X.scriptorium.json`.
pub fn file_name_for(script_file_name: &str) -> String {
    let stem = script_file_name.rsplit_once('.').map_or(script_file_name, |(stem, _)| stem);
    format!("{stem}.scriptorium.json")
}

/// How a review file's path is stored: relative to the script's folder when
/// it's inside it (so a folder can be moved or shared), else absolute. Both
/// paths should be absolute and canonical; that's the caller's job.
pub fn stored_path(script_dir: &Path, review: &Path) -> String {
    match review.strip_prefix(script_dir) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.to_string_lossy().into_owned(),
        _ => review.to_string_lossy().into_owned(),
    }
}

/// The inverse of [`stored_path`].
pub fn resolve_path(script_dir: &Path, stored: &str) -> PathBuf {
    let p = Path::new(stored);
    if p.is_absolute() { p.to_path_buf() } else { script_dir.join(p) }
}

/// How opening a saved session went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resumed {
    /// No saved session: starting from the files.
    Fresh,
    /// The saved events were replayed.
    Resumed { events: usize },
    /// The saved session can't be used (the script changed, or it's unreadable).
    /// Starting from the files; the caller should keep the old file aside.
    Discarded(String),
}

/// An opened session: the reviews, the document, and what to save.
#[derive(Debug, Clone)]
pub struct Opened {
    pub session: Session,
    pub doc: Document,
    /// The events that made `doc`'s starting point (saved as [`SessionFile::base`]).
    pub base: Vec<Event>,
    pub how: Resumed,
    /// Things to tell the user: a duplicate review, a review file that changed.
    pub notes: Vec<String>,
}

impl Opened {
    /// The session file to write for the current state.
    pub fn to_file(&self, script_contents: &str) -> SessionFile {
        SessionFile::new(script_contents, self.session.attached(), &self.base, self.doc.log())
    }
}

/// Open the document for `script` with `reviews` attached (each with its
/// [`Review::path`] set). `saved` is the session file's contents, if any.
///
/// Fresh (or a session that can't be used): the review files' saved decisions
/// are the starting point. Resumed: the saved starting point and events are
/// replayed; a review not attached last time (or whose file changed) has its
/// saved decisions applied on top as one undo step.
pub fn resume(reviews: Vec<Review>, script: Script, script_contents: &str, saved: Option<&str>) -> Opened {
    let mut session = Session::default();
    let mut notes = vec![];
    for r in reviews {
        if let Err(e) = session.add(r) {
            notes.push(e);
        }
    }
    let fresh = |session: Session, how: Resumed, notes: Vec<String>| {
        let (doc, base) = session.open_document_with_base(script.clone());
        Opened { session, doc, base, how, notes }
    };
    let Some(saved) = saved else { return fresh(session, Resumed::Fresh, notes) };
    let file = match SessionFile::from_json(saved) {
        Ok(f) => f,
        Err(e) => return fresh(session, Resumed::Discarded(e), notes),
    };
    if !file.is_for(script_contents) {
        let why = "the script has changed since the session was saved".into();
        return fresh(session, Resumed::Discarded(why), notes);
    }
    for a in &file.reviews {
        if let Some(r) = session.reviews.iter().find(|r| r.path == a.path && r.id != a.id) {
            notes.push(format!(
                "{} changed since last time; its earlier decisions are kept in the text but no longer marked.",
                r.name
            ));
        }
    }
    let (start, base, known): (Document, Vec<Event>, HashSet<String>) = match file.base {
        Some(base) => match Document::new(script.clone()).replay_onto(&base) {
            Ok(d) => (d.rebase(), base, file.reviews.iter().map(|a| a.id.clone()).collect()),
            Err(e) => {
                let why = format!("the saved session doesn't apply: {e}");
                return fresh(session, Resumed::Discarded(why), notes);
            }
        },
        None => {
            let (d, base) = session.open_document_with_base(script.clone());
            (d, base, session.reviews.iter().map(|r| r.id.clone()).collect())
        }
    };
    let mut doc = match start.replay_onto(&file.events) {
        Ok(d) => d,
        Err(e) => {
            let why = format!("the saved session doesn't apply: {e}");
            return fresh(session, Resumed::Discarded(why), notes);
        }
    };
    for r in 0..session.reviews.len() {
        if !known.contains(&session.reviews[r].id) {
            session.apply_saved(&mut doc, r);
        }
    }
    Opened { session, doc, base, how: Resumed::Resumed { events: file.events.len() }, notes }
}

/// FNV-1a, 64-bit, as 16 hex digits. Stable across platforms and Rust versions
/// (unlike `std`'s hasher); not cryptographic.
pub fn hash(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable() {
        assert_eq!(hash(""), "cbf29ce484222325");
        assert_eq!(hash("a"), "af63dc4c8601ec8c");
    }

    #[test]
    fn file_names() {
        assert_eq!(file_name_for("PURPLE LIST.fdx"), "PURPLE LIST.scriptorium.json");
        assert_eq!(file_name_for("a.b.fdx"), "a.b.scriptorium.json");
        assert_eq!(file_name_for("noext"), "noext.scriptorium.json");
    }

    #[test]
    fn round_trips() {
        let reviews = vec![Attached { path: "a.review.txt".into(), id: "0123456789ab".into() }];
        let f = SessionFile::new("<FinalDraft/>", reviews, &[Event::Redo], &[Event::Undo, Event::Redo]);
        let back = SessionFile::from_json(&f.to_json()).unwrap();
        assert_eq!(back, f);
        assert_eq!(back.version, 2);
        assert!(back.is_for("<FinalDraft/>"));
        assert!(!back.is_for("<FinalDraft />"));
    }

    #[test]
    fn reads_version_1_as_no_reviews() {
        let v1 = format!(r#"{{"version":1,"script_hash":"{}","events":["Undo"]}}"#, hash("x"));
        let f = SessionFile::from_json(&v1).unwrap();
        assert!(f.reviews.is_empty());
        assert_eq!(f.base, None);
        assert_eq!(f.events, vec![Event::Undo]);
        assert!(SessionFile::from_json(&v1.replace(r#""version":1"#, r#""version":3"#)).is_err());
    }

    #[test]
    fn paths_are_relative_inside_the_script_folder() {
        let dir = Path::new("/scripts/film");
        assert_eq!(stored_path(dir, Path::new("/scripts/film/a.review.txt")), "a.review.txt");
        assert_eq!(stored_path(dir, Path::new("/scripts/film/notes/b.txt")), "notes/b.txt");
        assert_eq!(stored_path(dir, Path::new("/elsewhere/c.txt")), "/elsewhere/c.txt");
        assert_eq!(stored_path(dir, Path::new("/scripts/filmic/d.txt")), "/scripts/filmic/d.txt");
        for p in ["/scripts/film/a.review.txt", "/scripts/film/notes/b.txt", "/elsewhere/c.txt"] {
            assert_eq!(resolve_path(dir, &stored_path(dir, Path::new(p))), Path::new(p));
        }
    }
}
