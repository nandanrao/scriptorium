//! Writing files so a reader (the app, another CLI run, an agent) never sees
//! half of one, and a crash mid-write leaves the old file in place; and
//! [`SessionLock`], which keeps two programs from writing a script's session
//! at once. With the `opened` feature, [`opened`]: a script opened with its
//! saved session, the way the CLI and the chat host open it.

#[cfg(feature = "opened")]
pub mod opened;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Makes each temporary name unique within the process, so two threads
/// writing the same file never share one.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to `path` atomically: into `.<name>.tmp-<pid>-<n>` in the same
/// folder, fsync, then rename over `path` (and fsync the folder, best effort).
/// An existing file keeps its permissions; a symlink is written through to
/// its target rather than replaced. On failure the temporary file is removed
/// and `path` is untouched.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    // Through a symlink to the file it points at, so the link survives.
    let path = match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("{}: not a file name", path.display())))?;
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{}.tmp-{}-{n}", name.to_string_lossy(), std::process::id()));
    let result = (|| {
        let mut f = File::create(&tmp)?;
        if let Ok(m) = fs::metadata(&path) {
            f.set_permissions(m.permissions())?;
        }
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, &path)?;
        // The rename itself is durable once the folder is synced; not every
        // platform lets a folder be opened for that, so it's best effort.
        if let Ok(d) = File::open(&dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// How long a program waits for another to finish with a script's session.
pub const LOCK_WAIT: Duration = Duration::from_secs(5);

/// What a program that couldn't get the lock in time is told.
pub const BUSY: &str = "the script is busy in another program; try again";

/// What a program is told when the file system has no file locks.
pub const NO_LOCKS: &str = "this folder doesn't support file locks; avoid running the app and Claude at the same time";

/// Why [`SessionLock::acquire`] didn't lock. `Display` gives the message a
/// person is shown: [`BUSY`], [`NO_LOCKS`], or what the failure was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockError {
    /// Another program held the lock for the whole wait.
    Busy,
    /// The file system has no file locks (some network and FUSE mounts):
    /// callers go on without the lock and log [`NO_LOCKS`].
    NoLocks,
    /// Anything else (the lock file couldn't be created, ...): the path and
    /// the OS error.
    Io(String),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Busy => f.write_str(BUSY),
            LockError::NoLocks => f.write_str(NO_LOCKS),
            LockError::Io(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for LockError {}

/// Whether a failed lock means "this file system has no locks". The OS codes
/// differ: ENOLCK is 37 on Linux but 77 on macOS and the BSDs (hence libc's
/// constant, not a number), and Windows says ERROR_INVALID_FUNCTION (1) or
/// ERROR_NOT_SUPPORTED (50) for a share that can't lock.
fn is_no_locks(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    let codes: &[i32] = &[libc::ENOLCK];
    #[cfg(windows)]
    let codes: &[i32] = &[1, 50];
    #[cfg(not(any(unix, windows)))]
    let codes: &[i32] = &[];
    e.raw_os_error().is_some_and(|c| codes.contains(&c))
}

/// An OS advisory lock (flock on Unix, LockFileEx on Windows) on
/// `<script>.scriptorium.lock` next to the script, held while a program reads,
/// changes and writes the script's session (`<script>.scriptorium.json`) and
/// its review files, so two writers never each write the session without the
/// other's changes. Every writer takes it: the CLI and the MCP server around
/// a whole command, the app from checking the files on disk through writing.
/// Released when dropped (or when the program exits, however it exits).
///
/// The lock file is never deleted (deleting it would let two programs lock
/// different files); it's empty, and can be deleted when nothing is running.
/// Within one process, don't take a second lock on the same script while
/// holding one: it waits for the first.
#[derive(Debug)]
pub struct SessionLock {
    _file: File,
}

impl SessionLock {
    /// The lock file for the script at `script` (a path to the script file).
    pub fn path_for(script: &Path) -> PathBuf {
        let name = script.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let stem = name.rsplit_once('.').map_or(name.as_str(), |(stem, _)| stem);
        script.with_file_name(format!("{stem}.scriptorium.lock"))
    }

    /// Lock the session of the script at `script`, waiting up to
    /// [`LOCK_WAIT`] for another program to finish; [`LockError::Busy`] if
    /// it doesn't.
    pub fn for_script(script: &Path) -> Result<Self, LockError> {
        Self::acquire(&Self::path_for(script), LOCK_WAIT)
    }

    /// Lock `lock_path` (created if missing), trying until `wait` has passed
    /// (`Duration::ZERO`: try once). Still held elsewhere: [`LockError::Busy`];
    /// no locks on this file system: [`LockError::NoLocks`]; anything else:
    /// [`LockError::Io`] saying what it was.
    pub fn acquire(lock_path: &Path, wait: Duration) -> Result<Self, LockError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock_path)
            .map_err(|e| LockError::Io(format!("{}: {e}", lock_path.display())))?;
        let until = Instant::now() + wait;
        loop {
            // Through the trait: newer toolchains have an inherent
            // `File::try_lock`, and rust-version (1.88) doesn't.
            match fs4::FileExt::try_lock(&file) {
                Ok(()) => return Ok(Self { _file: file }),
                Err(fs4::TryLockError::WouldBlock) if Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(fs4::TryLockError::WouldBlock) => return Err(LockError::Busy),
                Err(fs4::TryLockError::Error(e)) if is_no_locks(&e) => return Err(LockError::NoLocks),
                Err(fs4::TryLockError::Error(e)) => {
                    return Err(LockError::Io(format!("{}: {e}", lock_path.display())));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_is_exclusive_and_released_on_drop() {
        let dir = std::env::temp_dir().join(format!("scriptorium-lock-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("film.fdx");
        let at = SessionLock::path_for(&script);
        assert_eq!(at.file_name().unwrap(), "film.scriptorium.lock");
        let held = SessionLock::acquire(&at, Duration::ZERO).unwrap();
        let busy = SessionLock::acquire(&at, Duration::from_millis(60)).unwrap_err();
        assert_eq!(busy, LockError::Busy);
        assert_eq!(busy.to_string(), BUSY);
        // Released by another thread while this one waits: the wait gets it.
        let waiter = std::thread::spawn({
            let at = at.clone();
            move || SessionLock::acquire(&at, Duration::from_secs(5)).map(drop)
        });
        std::thread::sleep(Duration::from_millis(50));
        drop(held);
        waiter.join().unwrap().unwrap();
        assert!(at.exists(), "the lock file stays");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_locks_is_recognised_by_this_platforms_codes() {
        assert!(is_no_locks(&io::Error::from(io::ErrorKind::Unsupported)));
        #[cfg(unix)]
        assert!(is_no_locks(&io::Error::from_raw_os_error(libc::ENOLCK)));
        #[cfg(windows)]
        assert!(is_no_locks(&io::Error::from_raw_os_error(50)) && is_no_locks(&io::Error::from_raw_os_error(1)));
        assert!(!is_no_locks(&io::Error::from(io::ErrorKind::PermissionDenied)));
        assert_eq!(LockError::NoLocks.to_string(), NO_LOCKS);
    }

    #[test]
    fn writes_replaces_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("scriptorium-fsio-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.txt");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "two");
        let names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["a.txt"]);
        // A missing folder fails without leaving anything behind.
        assert!(write_atomic(&dir.join("no/such/b.txt"), b"x").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_writers_in_one_process_do_not_collide() {
        let dir = std::env::temp_dir().join(format!("scriptorium-fsio-threads-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.txt");
        let texts: Vec<String> = (0..8).map(|i| format!("writer {i} ").repeat(2000)).collect();
        std::thread::scope(|s| {
            for t in &texts {
                let p = &p;
                s.spawn(move || {
                    for _ in 0..20 {
                        write_atomic(p, t.as_bytes()).unwrap();
                    }
                });
            }
        });
        // Whole text of one writer, never a mix; no temporary file left.
        assert!(texts.contains(&fs::read_to_string(&p).unwrap()));
        let names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["a.txt"]);
        let _ = fs::remove_dir_all(&dir);
    }
}
