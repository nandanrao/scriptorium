//! The CLI's store: the script opened from its files the way every CLI
//! command opens it ([`Opened`]), under the session lock when writing.

use std::path::{Path, PathBuf};
use std::time::Duration;

use scriptorium_core::{Document, ElementKind};
use scriptorium_fsio::opened::{Opened, file_name};
use scriptorium_fsio::ops::{self, Choice, SuggestionRef};
use scriptorium_fsio::versions::{Made, NewVersion};
use scriptorium_fsio::{LockError, SessionLock};
use scriptorium_review::{RowResolver, Session};
use scriptorium_versions::PairView;

use super::{ReviewError, ReviewStore, Script, create_new, resolve_in};

/// Opens a script from its files for each tool call.
#[derive(Debug)]
pub struct FileScript {
    /// The script's path as given (a UTF-8 path: [`Opened`] takes `&str`).
    script: String,
    /// The script's path, canonical (for the lock).
    canonical: PathBuf,
    lock_wait: Duration,
    notes: Vec<String>,
}

impl FileScript {
    /// The script at `script`; it must exist.
    pub fn new(script: &Path) -> Result<Self, String> {
        // A regular .fdx: never a lock beside a dotfile or a folder.
        let canonical = scriptorium_fsio::check_script(script)?;
        let script = script.to_str().ok_or_else(|| format!("{}: not a UTF-8 path", script.display()))?.to_string();
        Ok(FileScript { script, canonical, lock_wait: Duration::ZERO, notes: vec![] })
    }

    /// How long each opening waits for the session lock. The default is not
    /// at all: a busy lock comes back as [`ReviewError::Busy`] at once, and
    /// [`crate::host::run`] tries the call again every
    /// [`super::BUSY_RETRY`] for up to [`super::BUSY_FOR`].
    pub fn with_lock_wait(mut self, wait: Duration) -> Self {
        self.lock_wait = wait;
        self
    }
}

impl Script for FileScript {
    /// Things opening the script turned up (a review file that can't be
    /// read, no file locks here…), each once.
    fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    fn open(&mut self, writes: bool) -> Result<Box<dyn ReviewStore + '_>, ReviewError> {
        let lock = if writes { self.lock()? } else { None };
        let mut more = vec![];
        let o = if writes {
            Opened::open(&self.script, &[], &mut more)
        } else {
            Opened::open_read_only(&self.script, &mut more)
        }
        .map_err(ReviewError::Failed)?;
        more.into_iter().for_each(|n| note(&mut self.notes, n));
        let caps = caps(&o);
        Ok(Box::new(FileStore { o, caps, writes, script: &self.script, notes: &mut self.notes, _lock: lock }))
    }

    /// Straight on the session file, under the lock, without opening the
    /// script ([`ops::detach`]): a session that can't be replayed can still
    /// lose a review, as with `reviews remove`.
    fn detach(&mut self, files: &[String]) -> Result<String, ReviewError> {
        let _lock = self.lock()?;
        let dir = self.canonical.parent().unwrap_or(Path::new("")).to_path_buf();
        let files: Vec<String> = files.iter().map(|f| resolve_in(&dir, f)).collect();
        let files: Vec<&str> = files.iter().map(String::as_str).collect();
        ops::detach(&self.script, &files).map_err(ReviewError::Failed)
    }
}

impl FileScript {
    /// The session lock, waited for up to the lock wait; `None` (and a
    /// note) where the disk has no file locks.
    fn lock(&mut self) -> Result<Option<SessionLock>, ReviewError> {
        match SessionLock::acquire(&SessionLock::path_for(&self.canonical), self.lock_wait) {
            Ok(l) => Ok(Some(l)),
            Err(LockError::Busy) => Err(ReviewError::Busy),
            Err(e @ LockError::NoLocks) => {
                note(&mut self.notes, e.to_string());
                Ok(None)
            }
            Err(e) => Err(ReviewError::Failed(e.to_string())),
        }
    }
}

/// The `new_version` tool result for what `version_new` made or found: the
/// same text from every store (the CLI's and the app's). `switch`: the
/// caller is an outside agent, which names the script in each call and goes
/// on with the version's path
/// ([`scriptorium_tools::new_version_message`]).
pub fn new_version_result(made: &NewVersion, switch: bool) -> String {
    // No counts for a version made before: its links file's stamps don't
    // give them as they are now.
    let (version, links) = (file_name(&made.version), file_name(&made.links));
    let first = match &made.made {
        Made::Already => scriptorium_tools::already_made(&version, &links, &[]),
        Made::Refound(msgs) => scriptorium_tools::already_made(&version, &links, msgs),
        Made::New | Made::Adopted => made.summary().lines().next().unwrap_or_default().to_string(),
    };
    scriptorium_tools::new_version_message(&first, &made.version.display().to_string(), switch)
}

fn note(notes: &mut Vec<String>, n: String) {
    if !notes.contains(&n) {
        notes.push(n);
    }
}

fn caps(o: &Opened) -> Box<dyn Fn(&ElementKind) -> bool> {
    let set = o.fdx.caps_types().clone();
    Box::new(move |k| set.contains(k.name()))
}

/// The script opened from its files for one tool call; holds the session
/// lock (when opened for writing) until dropped. Writing saves the review
/// file, then the session file.
pub struct FileStore<'a> {
    o: Opened,
    caps: Box<dyn Fn(&ElementKind) -> bool>,
    writes: bool,
    script: &'a str,
    notes: &'a mut Vec<String>,
    _lock: Option<SessionLock>,
}

impl FileStore<'_> {
    fn writable(&self) -> Result<(), ReviewError> {
        if self.writes { Ok(()) } else { Err(ReviewError::Failed("opened for reading only".into())) }
    }

    fn reopen(&mut self, extra: &[&str]) -> Result<(), ReviewError> {
        self.writable()?;
        let mut more = vec![];
        self.o = Opened::open(self.script, extra, &mut more).map_err(ReviewError::Failed)?;
        more.into_iter().for_each(|n| note(self.notes, n));
        self.caps = caps(&self.o);
        Ok(())
    }
}

impl ReviewStore for FileStore<'_> {
    fn dir(&self) -> &Path {
        &self.o.dir
    }
    fn script_name(&self) -> &str {
        &self.o.script_name
    }
    fn doc(&self) -> &Document {
        &self.o.doc
    }
    fn session(&self) -> &Session {
        &self.o.session
    }
    fn is_caps(&self) -> &dyn Fn(&ElementKind) -> bool {
        &*self.caps
    }
    fn session_mut(&mut self) -> (&mut Session, &Document, &dyn Fn(&ElementKind) -> bool) {
        (&mut self.o.session, &self.o.doc, &*self.caps)
    }

    fn row_resolver(&self) -> Option<Box<RowResolver<'static>>> {
        self.o.row_resolver()
    }

    fn pair_view(&self) -> Option<Result<PairView<'_>, String>> {
        self.o.pair_view()
    }

    fn reload(&mut self) -> Result<(), ReviewError> {
        self.reopen(&[])
    }

    fn new_version(&mut self, language: &str, original_language: &str, switch: bool) -> Result<String, ReviewError> {
        if !self.writes {
            return Err(ReviewError::Failed("opened for reading only".into()));
        }
        let mut more = vec![];
        let made = scriptorium_fsio::versions::version_new(
            Path::new(self.script),
            language,
            Some(original_language),
            None,
            &mut more,
        );
        more.into_iter().for_each(|n| note(self.notes, n));
        Ok(new_version_result(&made.map_err(ReviewError::Failed)?, switch))
    }

    fn may_be_version(&self) -> bool {
        self.o.pair().maybe_made()
    }

    fn attach(&mut self, path: &Path) -> Result<usize, ReviewError> {
        let at = path.to_str().ok_or_else(|| ReviewError::Failed(format!("{}: not a UTF-8 path", path.display())))?;
        self.reopen(&[at])?;
        Ok(self.o.given[0])
    }

    fn create(&mut self, path: &Path, text: &str) -> Result<usize, ReviewError> {
        if !self.writes {
            return Err(ReviewError::Failed("opened for reading only".into()));
        }
        create_new(path, text)?;
        self.attach(path)
    }

    fn write(&mut self, r: usize, text: String) -> Result<(), ReviewError> {
        if !self.writes {
            return Err(ReviewError::Failed("opened for reading only".into()));
        }
        let path = self.o.review_path(r);
        self.o.write_review(&path, &text).map_err(ReviewError::Failed)?;
        self.o.save().map_err(ReviewError::Failed)
    }

    fn attach_reviews(&mut self, files: &[PathBuf]) -> Result<String, ReviewError> {
        let files: Vec<&str> = files
            .iter()
            .map(|p| p.to_str().ok_or_else(|| ReviewError::Failed(format!("{}: not a UTF-8 path", p.display()))))
            .collect::<Result<_, _>>()?;
        self.reopen(&files)?;
        ops::attach(&mut self.o, self.script).map_err(ReviewError::Failed)
    }

    fn detach(&mut self, files: &[String]) -> Result<String, ReviewError> {
        self.writable()?;
        let files: Vec<&str> = files.iter().map(String::as_str).collect();
        ops::detach(self.script, &files).map_err(ReviewError::Failed)
    }

    /// The review picked as the CLI's `decide` picks it for an agent
    /// ([`Opened::open_one`], never from the current folder), the script
    /// opened again with it.
    fn decide(&mut self, review: Option<&str>, n: SuggestionRef, choice: Choice) -> Result<String, ReviewError> {
        self.writable()?;
        let mut more = vec![];
        let (o, r) = Opened::open_one(self.script, None, review, false, &mut more).map_err(ReviewError::Failed)?;
        more.into_iter().for_each(|n| note(self.notes, n));
        self.o = o;
        self.caps = caps(&self.o);
        ops::decide(&mut self.o, r, n, choice).map_err(ReviewError::Failed)
    }

    fn edit(&mut self, para: u64, text: &str) -> Result<String, ReviewError> {
        self.writable()?;
        ops::edit(&mut self.o, para, text).map_err(ReviewError::Failed)
    }

    fn undo_redo(&mut self, redo: bool) -> Result<String, ReviewError> {
        self.writable()?;
        ops::undo_redo(&mut self.o, redo).map_err(ReviewError::Failed)
    }

    /// Only where an agent may write ([`ops::agent_output`]), then
    /// [`ops::write`].
    fn write_script(&mut self, output: Option<&str>) -> Result<String, ReviewError> {
        self.writable()?;
        let out = ops::agent_output(&self.o, self.script, output).map_err(ReviewError::Failed)?;
        ops::write(&mut self.o, self.script, false, Some(&out)).map_err(ReviewError::Failed)
    }
}
