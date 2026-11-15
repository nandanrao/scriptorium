//! The per-thread lock: `<id>.lock` beside the thread file, held for the
//! length of a run, so the CLI and the app never run one thread at once.
//!
//! The lock covers the thread, not just a write: take it, **re-read the
//! thread file** (the other program may have added turns), then push the
//! user's message and run ([`crate::store::ThreadStore::lock_and_read`] does
//! the first two in that order). If the thread is already running, the second
//! program is refused with [`StoreError::Busy`]; it never waits for a whole
//! run. The app tries once (`Duration::ZERO`); the CLI waits [`CLI_WAIT`] so
//! that the app's momentary probe isn't mistaken for a run.
//!
//! The OS lock (`fs4` through [`SessionLock`]) belongs to an open file, so a
//! second attempt from the same process fails against the first. A program
//! that can run several threads (the app) keeps an [`OwnRuns`] and goes
//! through it, so it knows its own runs without probing them.
//!
//! Where the file system has no locks, a lock is still granted, without the
//! OS lock: runs go ahead, and [`ThreadLock::warning`] says what to log. Two
//! programs on one thread then risk last-writer-wins on the thread file, the
//! same exposure the session file has in that mode.
//!
//! Lock files are never deleted, not even with their thread: deleting one
//! would let two programs lock different files.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use scriptorium_fsio::{LockError, NO_LOCKS, SessionLock};

use crate::clock::IdSource;
use crate::store::{StoreError, ThreadStore};

/// How long the CLI waits for a thread's lock before refusing.
pub const CLI_WAIT: Duration = Duration::from_secs(2);

/// A thread locked for a run (or, without file locks, marked as running).
/// Released when dropped.
#[derive(Debug)]
pub struct ThreadLock {
    id: String,
    /// `None` when the file system has no locks.
    held: Option<SessionLock>,
}

impl ThreadLock {
    /// The thread this lock is for.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Whether the OS lock is really held (false where there are no file locks).
    pub fn is_held(&self) -> bool {
        self.held.is_some()
    }

    /// What to log when running without the OS lock: [`NO_LOCKS`].
    pub fn warning(&self) -> Option<&'static str> {
        self.held.is_none().then_some(NO_LOCKS)
    }

    /// Locks `path` for thread `id`, trying until `wait` has passed.
    pub(crate) fn acquire(path: &Path, id: &str, wait: Duration) -> Result<Self, StoreError> {
        match SessionLock::acquire(path, wait) {
            Ok(l) => Ok(Self { id: id.to_string(), held: Some(l) }),
            Err(LockError::Busy) => Err(StoreError::Busy),
            Err(LockError::NoLocks) => Ok(Self { id: id.to_string(), held: None }),
            Err(LockError::Io(e)) => Err(StoreError::Io(e)),
        }
    }
}

/// The runs this process has going, for a program that can run more than one
/// thread (the app). Cheap to clone; clones share the set.
#[derive(Debug, Clone, Default)]
pub struct OwnRuns {
    running: Arc<Mutex<HashSet<PathBuf>>>,
}

/// A run this process started: the thread's lock, and its place in
/// [`OwnRuns`]. Dropping it ends both.
#[derive(Debug)]
pub struct Run {
    /// Always `Some` until dropped: `Drop` takes it to release the lock
    /// before the run leaves the set, so nothing in this process can see
    /// the thread "free" while it is still locked.
    lock: Option<ThreadLock>,
    key: PathBuf,
    running: Arc<Mutex<HashSet<PathBuf>>>,
}

impl Run {
    pub fn lock(&self) -> &ThreadLock {
        self.lock.as_ref().expect("a run holds its lock until dropped")
    }
    pub fn id(&self) -> &str {
        self.lock().id()
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        // Fields drop after this body, so release the lock here first.
        drop(self.lock.take());
        self.running.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.key);
    }
}

impl OwnRuns {
    /// Whether this process is running thread `id` of `store`.
    pub fn is_running(&self, store: &ThreadStore, id: &str) -> bool {
        store.lock_path(id).is_ok_and(|p| self.set().contains(&key(&p)))
    }

    /// Starts a run of thread `id`: [`StoreError::RunningHere`] if this
    /// process is already running it (without touching the lock), else the
    /// lock as [`ThreadStore::lock`] takes it. Re-read the thread file after.
    pub fn start(&self, store: &ThreadStore, id: &str, wait: Duration) -> Result<Run, StoreError> {
        let key = self.reserve(store, id)?;
        match store.lock(id, wait) {
            Ok(lock) => Ok(Run { lock: Some(lock), key, running: self.running.clone() }),
            Err(e) => {
                self.set().remove(&key);
                Err(e)
            }
        }
    }

    /// Starts a run of a new thread, as [`ThreadStore::create`].
    pub fn create(&self, store: &ThreadStore, ids: &mut dyn IdSource) -> Result<Run, StoreError> {
        let lock = store.create(ids)?;
        let key = key(&store.lock_path(lock.id())?);
        self.set().insert(key.clone());
        Ok(Run { lock: Some(lock), key, running: self.running.clone() })
    }

    /// Whether thread `id` could be run now: [`StoreError::RunningHere`]
    /// for this process's own runs (never probed: the probe would fail
    /// against this process's own lock), else [`ThreadStore::probe`].
    ///
    /// The key is reserved in the set for the length of the probe, so a
    /// [`Self::start`] racing it gets [`StoreError::RunningHere`] (this
    /// process is busy with the thread) rather than taking the probe's OS
    /// lock for another program's run.
    pub fn probe(&self, store: &ThreadStore, id: &str) -> Result<(), StoreError> {
        let key = self.reserve(store, id)?;
        let result = store.probe(id);
        self.set().remove(&key);
        result
    }

    fn reserve(&self, store: &ThreadStore, id: &str) -> Result<PathBuf, StoreError> {
        let key = key(&store.lock_path(id)?);
        if !self.set().insert(key.clone()) {
            return Err(StoreError::RunningHere);
        }
        Ok(key)
    }

    fn set(&self) -> std::sync::MutexGuard<'_, HashSet<PathBuf>> {
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One key per lock file however the script's path was spelled: the folder
/// resolved when it exists.
fn key(lock_path: &Path) -> PathBuf {
    match (lock_path.parent().and_then(|d| std::fs::canonicalize(d).ok()), lock_path.file_name()) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => lock_path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::SequentialIds;
    use crate::store::tests::{scratch, thread};
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    use std::time::Instant;

    const HOLD: &str = "SCRIPTORIUM_CHAT_TEST_HOLD";

    /// A store with thread t0001 written and unlocked.
    fn one_thread(name: &str) -> (PathBuf, ThreadStore) {
        let dir = scratch(name);
        let store = ThreadStore::new(dir.clone());
        let lock = store.create(&mut SequentialIds::default()).unwrap();
        store.write(&lock, &thread("t0001", "Hi")).unwrap();
        (dir, store)
    }

    #[test]
    fn a_second_handle_is_refused_until_the_first_is_dropped() {
        let (dir, store) = one_thread("handles");
        let held = store.lock("t0001", Duration::ZERO).unwrap();
        assert!(held.is_held() && held.warning().is_none());
        assert_eq!(store.lock("t0001", Duration::ZERO).err(), Some(StoreError::Busy));
        assert_eq!(store.probe("t0001"), Err(StoreError::Busy));
        assert_eq!(store.lock_and_read("t0001", Duration::from_millis(50)).err(), Some(StoreError::Busy));
        drop(held);
        store.probe("t0001").unwrap();
        // The probe released it again.
        let (_l, f) = store.lock_and_read("t0001", Duration::ZERO).unwrap();
        assert_eq!(f.id, "t0001");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_cli_wait_outlasts_a_brief_hold() {
        let (dir, store) = one_thread("wait");
        let probe = store.lock("t0001", Duration::ZERO).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            drop(probe);
        });
        store.lock("t0001", CLI_WAIT).unwrap();
        release.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_creating_program_holds_the_new_threads_lock() {
        let dir = scratch("create-lock");
        let store = ThreadStore::new(dir.clone());
        let lock = store.create(&mut SequentialIds::default()).unwrap();
        store.write(&lock, &thread("t0001", "Hi")).unwrap();
        assert_eq!(store.probe("t0001"), Err(StoreError::Busy));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn own_runs_are_known_without_probing() {
        let (dir, store) = one_thread("own");
        let runs = OwnRuns::default();
        assert!(!runs.is_running(&store, "t0001"));
        let run = runs.start(&store, "t0001", Duration::ZERO).unwrap();
        assert_eq!(run.id(), "t0001");
        assert!(runs.clone().is_running(&store, "t0001"), "clones share the set");
        assert_eq!(runs.probe(&store, "t0001"), Err(StoreError::RunningHere));
        assert_eq!(runs.start(&store, "t0001", Duration::ZERO).err(), Some(StoreError::RunningHere));
        // The same folder spelled another way is the same thread.
        let other = ThreadStore::new(dir.join(".").join("..").join(dir.file_name().unwrap()));
        assert!(runs.is_running(&other, "t0001"));
        drop(run);
        assert!(!runs.is_running(&store, "t0001"));
        runs.probe(&store, "t0001").unwrap();

        // Held by "another program" (another handle): busy, and not left in the set.
        let elsewhere = store.lock("t0001", Duration::ZERO).unwrap();
        assert_eq!(runs.probe(&store, "t0001"), Err(StoreError::Busy));
        assert_eq!(runs.start(&store, "t0001", Duration::ZERO).err(), Some(StoreError::Busy));
        assert!(!runs.is_running(&store, "t0001"));
        drop(elsewhere);

        let new = runs.create(&store, &mut SequentialIds::default()).unwrap();
        assert_eq!(new.id(), "t0002");
        assert!(runs.is_running(&store, "t0002"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_probe_racing_a_start_never_reads_as_another_program() {
        // Catches both races: a probe holding the OS lock unreserved (the
        // start sees Busy), and a finished start leaving the set before its
        // lock is released (the probe sees Busy). Each window is a few
        // syscalls wide, so it takes many rounds to hit.
        let (dir, store) = one_thread("race");
        let runs = OwnRuns::default();
        for _ in 0..2000 {
            let barrier = std::sync::Barrier::new(2);
            let (probed, started) = std::thread::scope(|s| {
                let p = s.spawn(|| {
                    barrier.wait();
                    runs.probe(&store, "t0001")
                });
                let st = s.spawn(|| {
                    barrier.wait();
                    runs.start(&store, "t0001", Duration::ZERO).map(drop)
                });
                (p.join().unwrap(), st.join().unwrap())
            });
            // Whichever wins, the loser hears "running here", never Busy.
            for r in [&probed, &started] {
                assert!(matches!(r, Ok(()) | Err(StoreError::RunningHere)), "{r:?}");
            }
            assert!(!runs.is_running(&store, "t0001"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Not a test on its own: the other process for `another_process_is_refused_then_waited_for`.
    #[test]
    fn child_holds_the_lock() {
        let Ok(dir) = std::env::var(HOLD) else { return };
        let lock = ThreadStore::new(dir.into()).lock("t0001", Duration::ZERO).unwrap();
        // On its own line: the harness has already printed "test … ... ".
        println!("\nchild-has-the-lock");
        std::io::stdout().flush().unwrap();
        // Hold until the parent closes our stdin.
        let _ = std::io::stdin().read_line(&mut String::new());
        drop(lock);
    }

    #[test]
    fn another_process_is_refused_then_waited_for() {
        let (dir, store) = one_thread("process");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "lock::tests::child_holds_the_lock", "--nocapture", "--test-threads=1"])
            .env(HOLD, &dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        while line.trim() != "child-has-the-lock" {
            line.clear();
            assert_ne!(out.read_line(&mut line).unwrap(), 0, "the child exited without locking");
        }
        assert_eq!(store.lock("t0001", Duration::ZERO).err(), Some(StoreError::Busy));
        assert_eq!(OwnRuns::default().probe(&store, "t0001"), Err(StoreError::Busy));
        let started = Instant::now();
        assert_eq!(store.lock("t0001", Duration::from_millis(100)).err(), Some(StoreError::Busy));
        assert!(started.elapsed() >= Duration::from_millis(100), "it waited before refusing");
        // The child lets go while we wait: the wait gets the lock.
        let stdin = child.stdin.take().unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(stdin);
        });
        store.lock("t0001", CLI_WAIT).unwrap();
        release.join().unwrap();
        assert!(child.wait().unwrap().success());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
