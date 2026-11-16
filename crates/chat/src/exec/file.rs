//! The CLI's store: the script opened from its files the way every CLI
//! command opens it ([`Opened`]), under the session lock when writing.

use std::path::{Path, PathBuf};
use std::time::Duration;

use scriptorium_core::{Document, ElementKind};
use scriptorium_fsio::opened::Opened;
use scriptorium_fsio::{LockError, SessionLock};
use scriptorium_review::Session;

use super::{ReviewError, ReviewStore, Script, create_new};

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
        let lock = if writes {
            match SessionLock::acquire(&SessionLock::path_for(&self.canonical), self.lock_wait) {
                Ok(l) => Some(l),
                Err(LockError::Busy) => return Err(ReviewError::Busy),
                Err(e @ LockError::NoLocks) => {
                    note(&mut self.notes, e.to_string());
                    None
                }
                Err(e) => return Err(ReviewError::Failed(e.to_string())),
            }
        } else {
            None
        };
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
    fn reopen(&mut self, extra: &[&str]) -> Result<(), ReviewError> {
        if !self.writes {
            return Err(ReviewError::Failed("opened for reading only".into()));
        }
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

    fn reload(&mut self) -> Result<(), ReviewError> {
        self.reopen(&[])
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
}
