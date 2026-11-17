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

use scriptorium_core::doc::{DocError, Event, Op};
use scriptorium_core::{Document, ElementKind, ParaId, Paragraph, Script};
use serde::{Deserialize, Serialize};

use crate::{Review, RowResolver, Session};

pub mod aside;
pub mod sync;
pub use sync::{
    Change, Disk, Renumber, Replayed, Seen, check, common_prefix, rebuild, review_changed, still_synced, with_unsaved,
};

/// Version 2 added the attached review files and the starting events.
/// Version 1 files are still read.
pub const VERSION: u32 = 2;

/// Version of the script reader whose paragraph ids the saved events use
/// (`scriptorium_fdx::READER_VERSION`; a test keeps them equal). Session files
/// without it were made by reader 1.
pub const READER: u32 = 2;

fn reader_1() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFile {
    pub version: u32,
    /// [`hash`] of the script file the events apply to.
    pub script_hash: String,
    /// The script reader version that assigned the paragraph ids ([`READER`]
    /// when written now; 1 in files saved before this was recorded).
    #[serde(default = "reader_1")]
    pub reader: u32,
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
    /// [`hash`] of the file's text when it was attached or last written
    /// through the library ([`Session::attached`]). Empty in sessions saved
    /// before this was recorded. Its `STATUS` lines are compared with the
    /// session only when the file has changed since. Any command that saves
    /// the session records the file's current hash (that of
    /// [`Review::text`], as reopened), so a hand edit to the file is reported
    /// once, on the next open, not on every open after it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text_hash: String,
}

impl SessionFile {
    pub fn new(script_contents: &str, reviews: Vec<Attached>, base: &[Event], events: &[Event]) -> Self {
        Self {
            version: VERSION,
            script_hash: hash(script_contents),
            reader: READER,
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

/// A session opened by [`resume`]: the reviews, the document, and what to save.
#[derive(Debug, Clone)]
pub struct ResumedSession {
    pub session: Session,
    pub doc: Document,
    /// The events that made `doc`'s starting point (saved as [`SessionFile::base`]).
    pub base: Vec<Event>,
    pub how: Resumed,
    /// Things to tell the user: a duplicate review, a review file that changed.
    pub notes: Vec<String>,
}

impl ResumedSession {
    /// The session file to write for the current state.
    pub fn to_file(&self, script_contents: &str) -> SessionFile {
        SessionFile::new(script_contents, self.session.attached(), &self.base, self.doc.log())
    }
}

/// A review file to open with a session: its contents, a display name, and
/// its path as the session file stores it (see [`stored_path`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSource {
    pub name: String,
    pub path: String,
    pub text: String,
}

/// Builds the [`RowResolver`] for the document reviews are opened against
/// (see [`resume`]). The host's factory reads the links file and
/// the other script; `doc` is the version's live document (its text, after
/// any replayed edits, is what rows are read from).
pub type RowResolverFactory<'a> = dyn for<'d> Fn(&'d Document) -> Box<RowResolver<'a>> + 'a;

/// Open the document for `script` (`script_contents` is its file's text)
/// with the review files in `sources` attached. `saved` is the session file's
/// contents, if any; `script_name` is the script's file name (to check each
/// review's `Script:` header).
///
/// Fresh, version 1, or a session that can't be used: the reviews are
/// matched against the script and their saved decisions are the starting
/// point (version 1 then replays its events on top; its starting point is
/// built with decision values as version 1 wrote them, without `@<para>`,
/// since its events check the values they replace).
///
/// Version 2: the saved starting point and events are replayed first (they
/// need no reviews), then the reviews are matched against the original script
/// and the replayed document (so a `PARA:` on a line edited or created since
/// is found), then a review the session didn't know (new, or whose file
/// changed) has its saved decisions applied on top as one undo step. A known
/// review's file decisions are not applied: the session wins, and a note says
/// so when the file's `STATUS` lines disagree with it, and the file has
/// changed since the session last attached or wrote it ([`Attached::text_hash`];
/// always checked for sessions saved before that was recorded).
///
/// `reader_1` is the same script file as reader version 1 read it
/// (`FdxFile::legacy_script`; the CLI and the app always pass it), or `None`
/// only if reader 1 is known to have numbered the file as `script`. A
/// session saved by reader 1 is used only if every paragraph id means the
/// same line under both readers ([`same_ids`], given the ids its saved
/// events touch); otherwise its ids mean other lines now and it's discarded
/// with a note (the caller sets the old file aside).
///
/// `ROW:` entries (suggestions for rows of a pair of versions) are placed
/// through a resolver `rows` builds for the document the reviews are opened
/// against: the replayed document, or (fresh, version 1, discarded) the
/// unedited script. See [`Review::open_live`]. `None`: they are not found
/// ([`crate::NO_LINKS`]).
#[allow(clippy::too_many_arguments)]
pub fn resume(
    sources: Vec<ReviewSource>,
    script: Script,
    reader_1: Option<&Script>,
    script_contents: &str,
    saved: Option<&str>,
    is_caps: &dyn Fn(&ElementKind) -> bool,
    script_name: &str,
    rows: Option<&RowResolverFactory>,
) -> ResumedSession {
    // Opens every review against `live` (or the unedited script).
    let open_all = |live: Option<&Document>, notes: &mut Vec<String>| {
        let mut session = Session::default();
        // With rows, the unedited script is opened as a document too, so the
        // resolver reads the same text matching does.
        let unedited = match (live, rows) {
            (None, Some(_)) => Some(Document::new(script.clone())),
            _ => None,
        };
        let live = live.or(unedited.as_ref());
        let resolver = rows.zip(live).map(|(make, doc)| make(doc));
        for s in &sources {
            let review = match live {
                Some(doc) => {
                    Review::open_live(s.name.clone(), &s.text, &script, doc, is_caps, script_name, resolver.as_deref())
                }
                None => Review::open(s.name.clone(), &s.text, &script, is_caps, script_name),
            };
            if let Err(e) = session.add(review.with_path(s.path.clone())) {
                notes.push(e);
            }
        }
        session
    };
    let fresh = |how: Resumed| {
        let mut notes = vec![];
        let session = open_all(None, &mut notes);
        let (doc, base) = session.open_document_with_base(script.clone());
        ResumedSession { session, doc, base, how, notes }
    };
    let Some(saved) = saved else { return fresh(Resumed::Fresh) };
    let file = match SessionFile::from_json(saved) {
        Ok(f) => f,
        Err(e) => return fresh(Resumed::Discarded(e)),
    };
    if !file.is_for(script_contents) {
        return fresh(Resumed::Discarded("the script has changed since the session was saved".into()));
    }
    if file.reader > READER {
        return fresh(Resumed::Discarded(format!(
            "the session was saved by a newer Scriptorium (script reader {})",
            file.reader
        )));
    }
    if file.reader < READER {
        let touched = touched_ids(file.base.iter().flatten().chain(&file.events));
        if !reader_1.is_none_or(|old| same_ids(old, &script, &touched)) {
            return fresh(Resumed::Discarded(
                "it was saved by an older Scriptorium that numbered this script's lines differently (around \
                 dual dialogue or scene summaries), so its line numbers no longer fit"
                    .into(),
            ));
        }
    }
    let doesnt_apply = |e: DocError| Resumed::Discarded(format!("the saved session doesn't apply: {e}"));
    let how = Resumed::Resumed { events: file.events.len() };
    let mut notes = vec![];
    let Some(base) = file.base else {
        // Version 1: the starting point is the review files' saved decisions.
        let session = open_all(None, &mut notes);
        let (start, base) = session.document_with_base(script.clone(), true);
        return match start.replay_onto(&file.events) {
            Ok(doc) => ResumedSession { session, doc, base, how, notes },
            Err(e) => fresh(doesnt_apply(e)),
        };
    };
    let replayed = Document::new(script.clone()).replay_onto(&base).and_then(|d| d.rebase().replay_onto(&file.events));
    let mut doc = match replayed {
        Ok(d) => d,
        Err(e) => return fresh(doesnt_apply(e)),
    };
    let mut session = open_all(Some(&doc), &mut notes);
    for a in &file.reviews {
        if let Some(r) = session.reviews.iter().find(|r| r.path == a.path && r.id != a.id) {
            notes.push(format!(
                "{} changed since last time; its earlier decisions are kept in the text but no longer marked.",
                r.name
            ));
        }
    }
    let known: HashSet<&str> = file.reviews.iter().map(|a| a.id.as_str()).collect();
    for r in 0..session.reviews.len() {
        if !known.contains(session.reviews[r].id.as_str()) {
            session.apply_saved(&mut doc, r);
            continue;
        }
        let unchanged = file.reviews.iter().any(|a| {
            a.id == session.reviews[r].id && !a.text_hash.is_empty() && a.text_hash == hash(session.reviews[r].text())
        });
        if unchanged {
            continue;
        }
        let n = session.file_disagreements(&doc, r);
        if n > 0 {
            notes.push(format!(
                "{}: {n} decision{} saved in the file differ{} from this session; the session's are kept.",
                session.reviews[r].name,
                if n == 1 { "" } else { "s" },
                if n == 1 { "s" } else { "" },
            ));
        }
    }
    session.note_removed_by(&doc);
    ResumedSession { session, doc, base, how, notes }
}

/// Whether every paragraph id means the same line in both scripts: same
/// count, same ids, same text. Kinds may differ (the old reader mistyped the
/// first line of a dual dialogue block). Two Scene Headings at the same id
/// count as the same line whatever their text (reader 1 read a heading's
/// scene summary as (part of) its text, which doesn't move any id), unless
/// the id is in `touched` (see [`touched_ids`]): saved edits to that heading
/// were made on reader 1's text, so they'd land on the wrong bytes.
pub fn same_ids(a: &Script, b: &Script, touched: &HashSet<ParaId>) -> bool {
    let heading = |p: &Paragraph| p.kind == ElementKind::SceneHeading && !touched.contains(&p.id);
    a.paragraphs.len() == b.paragraphs.len()
        && a.paragraphs
            .iter()
            .zip(&b.paragraphs)
            .all(|(x, y)| x.id == y.id && (x.text() == y.text() || heading(x) && heading(y)))
}

/// The paragraphs that the ops in `events` name (edited, split, joined,
/// retyped, inserted or removed). Undo and redo replay those same ops, and
/// metadata names no paragraph, so neither adds any. Decision meta values
/// carry `@<para>` but are ignored on purpose: a decision doesn't depend on
/// the paragraph's text, so it doesn't make that paragraph's text matter.
pub fn touched_ids<'a>(events: impl IntoIterator<Item = &'a Event>) -> HashSet<ParaId> {
    let mut ids = HashSet::new();
    for event in events {
        let Event::Edit(t) = event else { continue };
        for op in &t.ops {
            match op {
                Op::Splice { para, .. } | Op::SetKind { para, .. } => {
                    ids.insert(*para);
                }
                Op::Split { para, new_para: other, .. } | Op::Merge { para, next: other, .. } => {
                    ids.extend([*para, *other]);
                }
                Op::InsertPara { para, .. } | Op::RemovePara { para, .. } => {
                    ids.insert(para.id);
                }
                Op::SetMeta { .. } => {}
            }
        }
    }
    ids
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
        let reviews =
            vec![Attached { path: "a.review.txt".into(), id: "0123456789ab".into(), text_hash: "00ff".into() }];
        let f = SessionFile::new("<FinalDraft/>", reviews, &[Event::Redo], &[Event::Undo, Event::Redo]);
        let back = SessionFile::from_json(&f.to_json()).unwrap();
        assert_eq!(back, f);
        assert_eq!(back.version, 2);
        assert_eq!(back.reader, READER);
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
        assert_eq!(f.reader, 1);
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
