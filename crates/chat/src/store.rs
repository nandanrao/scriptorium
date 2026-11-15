//! A script's chat threads on disk: `<script>.threads/` next to the script
//! (`PURPLE LIST.fdx` → `PURPLE LIST.threads/`), one `<id>.json` per thread
//! ([`ThreadFile`]) and the `<id>.lock` that guards it ([`crate::lock`]).
//!
//! Thread files are written atomically ([`write_atomic`]), so a reader never
//! sees half of one. Writing or deleting a thread needs its [`ThreadLock`].
//! Deleting removes the thread file and leaves the lock file; listing skips
//! lock files (and anything else that isn't a thread file).

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use scriptorium_agent::file::ThreadFile;
use scriptorium_fsio::write_atomic;

use crate::clock::IdSource;
use crate::lock::ThreadLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// Another program is running the thread.
    Busy,
    /// This program is already running the thread ([`crate::lock::OwnRuns`]).
    RunningHere,
    /// No thread has this id.
    NotFound(String),
    /// Not a possible thread id (ids are letters, digits, `-` and `_`).
    BadId(String),
    /// The thread file can't be read: not JSON, or a newer version.
    File {
        id: String,
        message: String,
    },
    /// A thread written under another thread's lock.
    WrongLock {
        lock: String,
        thread: String,
    },
    /// The id source kept giving ids that are taken.
    NoFreeId,
    Io(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Busy => write!(f, "this thread is running in another program; try again when it finishes"),
            StoreError::RunningHere => write!(f, "this thread is already running"),
            StoreError::NotFound(id) => write!(f, "no thread {id}"),
            StoreError::BadId(id) => write!(f, "{id:?} is not a thread id"),
            StoreError::File { id, message } => write!(f, "thread {id}: {message}"),
            StoreError::WrongLock { lock, thread } => {
                write!(f, "thread {thread} can't be written under the lock for thread {lock}")
            }
            StoreError::NoFreeId => write!(f, "couldn't find a free thread id"),
            StoreError::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for StoreError {}

fn io_err(path: &Path, e: io::Error) -> StoreError {
    StoreError::Io(format!("{}: {e}", path.display()))
}

/// One entry of [`ThreadStore::list`]: the thread, or why its file can't be
/// read (the panel lists those greyed, with the reason).
#[derive(Debug, Clone, PartialEq)]
pub struct Listed {
    pub id: String,
    pub file: Result<ThreadFile, StoreError>,
}

/// The threads of one script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadStore {
    dir: PathBuf,
}

/// Ids name files, so they're kept to characters safe in any file name.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl ThreadStore {
    /// The threads of the script at `script` (a path to the script file).
    pub fn for_script(script: &Path) -> Self {
        let name = script.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let stem = name.rsplit_once('.').map_or(name.as_str(), |(stem, _)| stem);
        Self { dir: script.with_file_name(format!("{stem}.threads")) }
    }

    /// The threads in the folder `dir`.
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// `<script>.threads/`. It's created with the first thread.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `<id>.json`.
    pub fn path(&self, id: &str) -> Result<PathBuf, StoreError> {
        Ok(self.dir.join(format!("{}.json", checked(id)?)))
    }

    /// `<id>.lock`.
    pub fn lock_path(&self, id: &str) -> Result<PathBuf, StoreError> {
        Ok(self.dir.join(format!("{}.lock", checked(id)?)))
    }

    /// Every thread file, by id. No folder yet: no threads. Lock files,
    /// temporary files from a write in progress, and anything else that
    /// isn't `<id>.json` are skipped.
    pub fn list(&self) -> Result<Vec<Listed>, StoreError> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_err(&self.dir, e)),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| io_err(&self.dir, e))?;
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".json")) else { continue };
            if !is_valid_id(id) || !entry.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            out.push(Listed { id: id.to_string(), file: self.read(id) });
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// Reads thread `id`, without its lock: what's on disk now, which a run
    /// elsewhere may change. To run the thread, use [`Self::lock_and_read`].
    pub fn read(&self, id: &str) -> Result<ThreadFile, StoreError> {
        let path = self.path(id)?;
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(StoreError::NotFound(id.to_string())),
            Err(e) => return Err(io_err(&path, e)),
        };
        let file = ThreadFile::from_json(&text)
            .map_err(|e| StoreError::File { id: id.to_string(), message: e.to_string() })?;
        if file.id != id {
            return Err(StoreError::File {
                id: id.to_string(),
                message: format!("the file says it is thread {}", file.id),
            });
        }
        Ok(file)
    }

    /// Locks thread `id` for a run, trying until `wait` has passed (the app:
    /// `Duration::ZERO`; the CLI: [`crate::lock::CLI_WAIT`]). Re-read the
    /// thread after this, before changing it: [`Self::lock_and_read`].
    ///
    /// Within one process, check [`crate::lock::OwnRuns`] first: this fails
    /// against the process's own lock.
    pub fn lock(&self, id: &str, wait: Duration) -> Result<ThreadLock, StoreError> {
        let lock_path = self.lock_path(id)?;
        if !self.path(id)?.is_file() {
            return Err(StoreError::NotFound(id.to_string()));
        }
        ThreadLock::acquire(&lock_path, id, wait)
    }

    /// Locks thread `id` and then reads it, in that order, so the thread
    /// being run is the latest one on disk.
    pub fn lock_and_read(&self, id: &str, wait: Duration) -> Result<(ThreadLock, ThreadFile), StoreError> {
        let lock = self.lock(id, wait)?;
        let file = self.read(id)?;
        Ok((lock, file))
    }

    /// Whether thread `id` is free to run now: locks once and releases.
    /// [`StoreError::Busy`] if another program has it.
    pub fn probe(&self, id: &str) -> Result<(), StoreError> {
        self.lock(id, Duration::ZERO).map(drop)
    }

    /// A new thread's id, locked: the first candidate from `ids` with neither
    /// a thread file nor a lock file (a deleted thread's id is never reused,
    /// so its review id stays unique). The id is claimed by creating its lock
    /// file, so two programs creating threads at once get different ids.
    /// Write the thread file under the returned lock.
    pub fn create(&self, ids: &mut dyn IdSource) -> Result<ThreadLock, StoreError> {
        fs::create_dir_all(&self.dir).map_err(|e| io_err(&self.dir, e))?;
        for _ in 0..10_000 {
            let id = ids.next_id();
            let lock_path = self.lock_path(&id)?;
            if self.path(&id)?.exists() {
                continue;
            }
            match OpenOptions::new().write(true).create_new(true).open(&lock_path) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io_err(&lock_path, e)),
            }
            match ThreadLock::acquire(&lock_path, &id, Duration::ZERO) {
                Err(StoreError::Busy) => continue,
                r => return r,
            }
        }
        Err(StoreError::NoFreeId)
    }

    /// Writes `file` (atomically) under its thread's lock.
    pub fn write(&self, lock: &ThreadLock, file: &ThreadFile) -> Result<(), StoreError> {
        if lock.id() != file.id {
            return Err(StoreError::WrongLock { lock: lock.id().to_string(), thread: file.id.clone() });
        }
        let path = self.path(&file.id)?;
        write_atomic(&path, file.to_json().as_bytes()).map_err(|e| io_err(&path, e))
    }

    /// Deletes the thread file under its lock. The lock file stays (lock
    /// files are never deleted); the thread's review file is not this
    /// store's business.
    pub fn delete(&self, lock: &ThreadLock) -> Result<(), StoreError> {
        let path = self.path(lock.id())?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(StoreError::NotFound(lock.id().to_string())),
            Err(e) => Err(io_err(&path, e)),
        }
    }
}

fn checked(id: &str) -> Result<&str, StoreError> {
    if is_valid_id(id) { Ok(id) } else { Err(StoreError::BadId(id.to_string())) }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::clock::{SequentialIds, from_vars};
    use scriptorium_agent::Thread;
    use scriptorium_agent::file::{VERSION, tools_hash};

    /// A fresh scratch folder for one test.
    pub(crate) fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("scriptorium-chat-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(crate) fn thread(id: &str, text: &str) -> ThreadFile {
        let mut thread = Thread::default();
        assert!(thread.push_user(text));
        ThreadFile {
            version: VERSION,
            id: id.into(),
            title: text.into(),
            created: "2026-11-15T09:30:00Z".into(),
            updated: "2026-11-15T09:30:00Z".into(),
            account: "acct".into(),
            model: "claude-opus-5-5".into(),
            review_id: None,
            review_path: Some(format!("{text}.review.txt")),
            whole_script: None,
            system: "sys".into(),
            tools_hash: tools_hash(&[]),
            thread,
            extra: Default::default(),
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        v.sort();
        v
    }

    #[test]
    fn folder_is_named_after_the_script() {
        let s = ThreadStore::for_script(Path::new("/w/PURPLE LIST.fdx"));
        assert_eq!(s.dir(), Path::new("/w/PURPLE LIST.threads"));
        assert_eq!(ThreadStore::for_script(Path::new("/w/film")).dir(), Path::new("/w/film.threads"));
        assert_eq!(s.lock_path("t0001").unwrap(), Path::new("/w/PURPLE LIST.threads/t0001.lock"));
        for bad in ["", "../x", "a/b", "a.b", "x y"] {
            assert_eq!(s.path(bad), Err(StoreError::BadId(bad.into())));
        }
    }

    #[test]
    fn create_write_read_list_delete() {
        let dir = scratch("cycle");
        let store = ThreadStore::for_script(&dir.join("film.fdx"));
        assert_eq!(store.list().unwrap(), [], "no folder yet: no threads");
        let mut c = from_vars(Some("2026-11-15T09:30:00Z"), None).unwrap();
        let lock = store.create(c.ids.as_mut()).unwrap();
        assert_eq!(lock.id(), "t0001");
        let f = thread("t0001", "Tighten act one");
        store.write(&lock, &f).unwrap();
        assert_eq!(store.read("t0001").unwrap(), f, "review_path round-trips");
        // A second thread from a fresh id source (a new process) skips t0001.
        let lock2 = store.create(&mut SequentialIds::default()).unwrap();
        assert_eq!(lock2.id(), "t0002");
        assert_eq!(
            store.write(&lock2, &f),
            Err(StoreError::WrongLock { lock: "t0002".into(), thread: "t0001".into() })
        );
        store.write(&lock2, &thread("t0002", "Second")).unwrap();
        assert_eq!(names(store.dir()), ["t0001.json", "t0001.lock", "t0002.json", "t0002.lock"]);
        let ids: Vec<_> = store.list().unwrap().into_iter().map(|l| (l.id, l.file.is_ok())).collect();
        assert_eq!(ids, [("t0001".into(), true), ("t0002".into(), true)]);

        store.delete(&lock).unwrap();
        drop(lock);
        assert_eq!(names(store.dir()), ["t0001.lock", "t0002.json", "t0002.lock"], "the lock file stays");
        assert_eq!(store.list().unwrap().len(), 1, "the list skips lock files");
        assert_eq!(store.read("t0001"), Err(StoreError::NotFound("t0001".into())));
        assert_eq!(store.lock("t0001", Duration::ZERO).err(), Some(StoreError::NotFound("t0001".into())));
        // The deleted thread's id isn't reused: its lock file still claims it.
        assert_eq!(store.create(&mut SequentialIds::default()).unwrap().id(), "t0003");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_greys_unreadable_files_and_skips_the_rest() {
        let dir = scratch("list");
        let store = ThreadStore::new(dir.clone());
        fs::write(dir.join("bad.json"), "not json").unwrap();
        let mut newer: serde_json::Value = serde_json::from_str(&thread("new", "x").to_json()).unwrap();
        newer["version"] = (VERSION + 1).into();
        fs::write(dir.join("new.json"), newer.to_string()).unwrap();
        fs::write(dir.join("other.json"), thread("t9", "x").to_json()).unwrap();
        fs::write(dir.join("ok.json"), thread("ok", "x").to_json()).unwrap();
        fs::write(dir.join(".ok.json.tmp-1-0"), "half").unwrap();
        fs::write(dir.join("stray.lock"), "").unwrap();
        fs::write(dir.join("notes.txt"), "").unwrap();
        fs::create_dir(dir.join("sub.json")).unwrap();
        let listed = store.list().unwrap();
        let ids: Vec<_> = listed.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(ids, ["bad", "new", "ok", "other"]);
        assert!(matches!(&listed[0].file, Err(StoreError::File { .. })));
        assert!(matches!(&listed[1].file, Err(StoreError::File { message, .. }) if message.contains("version")));
        assert!(listed[2].file.is_ok());
        assert!(matches!(&listed[3].file, Err(StoreError::File { message, .. }) if message.contains("t9")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn writes_are_atomic_and_leave_no_temp() {
        let dir = scratch("atomic");
        let store = ThreadStore::new(dir.clone());
        let lock = store.create(&mut SequentialIds::default()).unwrap();
        let (a, b) = (thread("t0001", &"a".repeat(50_000)), thread("t0001", &"b".repeat(50_000)));
        std::thread::scope(|s| {
            s.spawn(|| {
                for _ in 0..30 {
                    store.write(&lock, &a).unwrap();
                    store.write(&lock, &b).unwrap();
                }
            });
            for _ in 0..200 {
                if let Ok(f) = store.read("t0001") {
                    assert!(f == a || f == b, "a reader sees a whole file");
                }
            }
        });
        assert_eq!(names(&dir), ["t0001.json", "t0001.lock"]);
        let _ = fs::remove_dir_all(&dir);
    }
}
