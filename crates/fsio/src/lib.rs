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

/// How many temporary names [`write_atomic`] tries before giving up (each
/// is random, so a clash means something else is making names there).
const TMP_TRIES: usize = 16;

/// Write `bytes` to `path` atomically: into `.<name>.tmp-<random>` in the
/// same folder, fsync, then rename over `path` (and fsync the folder, best
/// effort). The temporary file is created new (`create_new`, which never
/// follows a link) under a name nobody can predict, so it can't be planted
/// beforehand or pointed elsewhere. An existing file keeps its permissions;
/// a new one gets the usual ones (`0666` less the umask on Unix). A symlink
/// is written through to its target rather than replaced. On failure the
/// temporary file is removed and `path` is untouched.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_mode(path, bytes, 0o666)
}

/// [`write_atomic`], but a file that doesn't exist yet is created readable
/// by its owner only (`0600` on Unix; elsewhere as [`write_atomic`]): for
/// files holding a user's conversations or settings. An existing file keeps
/// its permissions.
///
/// Unlike [`write_atomic`], an existing symlink at `path` is refused rather
/// than written through: these files are never links by design, so one is
/// someone pointing the write elsewhere.
pub fn write_atomic_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    refuse_link(path)?;
    write_atomic_mode(path, bytes, 0o600)
}

/// [`write_atomic`], but an existing symlink at `path` is refused rather
/// than written through, as [`write_atomic_private`] does; a new file gets
/// the usual permissions. For the session file, which is never a link by
/// design (one is someone pointing the write elsewhere).
pub fn write_atomic_no_link(path: &Path, bytes: &[u8]) -> io::Result<()> {
    refuse_link(path)?;
    write_atomic_mode(path, bytes, 0o666)
}

fn refuse_link(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is a link; not writing through it", path.display()),
        ));
    }
    Ok(())
}

fn write_atomic_mode(path: &Path, bytes: &[u8], new_mode: u32) -> io::Result<()> {
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
    let (tmp, mut f) = create_temp(&dir, &name.to_string_lossy(), new_mode)?;
    let result = (|| {
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

/// A new temporary file beside `name` in `dir`, created with `create_new`
/// under a random name (mode `mode` on Unix).
fn create_temp(dir: &Path, name: &str, mode: u32) -> io::Result<(PathBuf, File)> {
    let mut last = None;
    for _ in 0..TMP_TRIES {
        let tmp = dir.join(format!(".{name}.tmp-{:016x}", random_u64()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode);
        #[cfg(not(unix))]
        let _ = mode;
        match options.open(&tmp) {
            Ok(f) => return Ok((tmp, f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("no free temporary name")))
}

/// 64 unpredictable bits: std's per-process random hash keys, mixed with the
/// time, the process id and a counter (no extra dependency).
fn random_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(TMP_COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u32(std::process::id());
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    h.write_u128(nanos);
    h.finish()
}

/// Whether new folders may be made for a file an agent names
/// ([`agent_file_in_folder`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Folders {
    /// The file's folder must exist.
    MustExist,
    /// A missing folder is accepted when the nearest folder that exists is
    /// inside the script's folder (resolved through links). Nothing is
    /// created here: the caller makes it (`create_dir_all` of the returned
    /// path's parent) only when it actually writes, so a call that fails
    /// later leaves no empty folder.
    MayCreate,
}

/// Where a path lies relative to the script's folder, resolved through
/// links: the one place that canonicalises, strips the folder and checks
/// for dot parts, shared by [`agent_file_in_folder`] and
/// [`agent_write_place`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum Place {
    /// Inside, with no part starting with `.`: the canonical path.
    Inside(PathBuf),
    /// Inside, but a part of it starts with `.`.
    Hidden,
    /// Not inside the script's folder.
    Outside,
    /// Its folder doesn't exist.
    NoFolder,
}

/// [`Place`] of `path` (absolute) with respect to `dir` (canonical): the
/// file resolved through links where it exists, else its folder.
fn place(dir: &Path, path: &Path) -> Place {
    let at = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => match (path.parent().map(Path::canonicalize), path.file_name()) {
            (Some(Ok(folder)), Some(name)) => folder.join(name),
            _ => return Place::NoFolder,
        },
    };
    match at.strip_prefix(dir) {
        Ok(rest) if hidden(rest) => Place::Hidden,
        Ok(_) => Place::Inside(at),
        Err(_) => Place::Outside,
    }
}

/// A file an agent (a model) names, checked before anything is written:
/// `path` (relative to `dir`, the script's folder, or absolute) as a
/// canonical path inside `dir` whose name ends in `suffix` (any case), or
/// why it can't be one, naming the script's folder. The rule for every path
/// a model picks: no `..`; no part starting with `.` (so never
/// `~/.bashrc.d/` when the script sits in the home folder); the folder must
/// exist (or, with [`Folders::MayCreate`], be creatable inside `dir`); the
/// file (or its folder, when the file doesn't exist yet), resolved through
/// any links, must still be inside `dir` (itself canonicalised, so `/var`
/// and `/private/var` on macOS compare equal); and an existing file must
/// not be a link.
///
/// Checked, then used: a folder swapped for a link in between would need
/// someone who can already write inside the script's folder, so that race
/// isn't guarded against.
pub fn agent_file_in_folder(dir: &Path, path: &str, suffix: &str, folders: Folders) -> Result<PathBuf, String> {
    let dir = dir.canonicalize().map_err(|e| format!("{}: {e}", dir.display()))?;
    let refuse = |why: &str| Err(format!("{path}: {why} (the script's folder is {})", dir.display()));
    let p = Path::new(path);
    let joined = if p.is_absolute() { p.to_path_buf() } else { dir.join(p) };
    let named = |p: &Path| p.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().ends_with(suffix));
    if !named(&joined) {
        return refuse(&format!("the file's name must end in {suffix}"));
    }
    if joined.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return refuse("`..` isn't allowed; name a file inside the script's folder");
    }
    // Lexically below the folder, for the dot check (absolute paths that
    // reach the folder only through a link are checked once resolved).
    if let Ok(rest) = joined.strip_prefix(&dir)
        && hidden(rest)
    {
        return refuse(HIDDEN);
    }
    let parent = joined.parent().ok_or_else(|| format!("{path}: not a file name"))?;
    if fs::symlink_metadata(&joined).is_ok_and(|m| m.file_type().is_symlink()) {
        return refuse("that's a link; name the file itself");
    }
    if folders == Folders::MayCreate && !parent.is_dir() {
        // Only below a folder that is already inside, resolved through links.
        let nearest = parent.ancestors().find(|a| a.is_dir()).map(|a| place(&dir, &a.join("x")));
        return match nearest {
            Some(Place::Inside(_)) if joined.starts_with(&dir) => Ok(joined),
            Some(Place::Hidden) => refuse(HIDDEN),
            _ => refuse("files go inside the script's folder"),
        };
    }
    match place(&dir, &joined) {
        Place::Inside(at) => Ok(at),
        Place::Hidden => refuse(HIDDEN),
        Place::Outside => refuse("files go inside the script's folder"),
        Place::NoFolder => refuse("that folder doesn't exist"),
    }
}

const HIDDEN: &str = "a name or folder starting with `.` isn't allowed";

/// Whether any part of `rest` (a relative path) starts with `.`.
fn hidden(rest: &Path) -> bool {
    rest.components().any(|c| matches!(c, std::path::Component::Normal(n) if n.to_string_lossy().starts_with('.')))
}

/// [`agent_file_in_folder`] for a review file (`*.review.txt`) whose
/// folder exists: where an agent may create or attach a review it names
/// that isn't attached yet. The chat agent's, and the CLI `tool`'s and
/// MCP's `review_file`. An existing file must also read as a review; the
/// caller checks its text.
pub fn review_in_folder(dir: &Path, path: &str) -> Result<PathBuf, String> {
    agent_file_in_folder(dir, path, ".review.txt", Folders::MustExist)
}

/// Whether `name` (a bare file name the host picked, such as a thread's
/// planned review name) is one an agent may create in the script's folder,
/// without touching the disk: a `*.review.txt`, no folder part, not
/// starting with `.`.
pub fn plain_review_name(name: &str) -> bool {
    !name.contains(['/', '\\']) && !name.starts_with('.') && name.to_lowercase().ends_with(".review.txt")
}

/// Whether `name` is one of a script's own files, never a review whatever
/// a session lists: a script (`.fdx`), a session file or a lock.
pub fn is_script_file(name: &str) -> bool {
    let name = name.to_lowercase();
    name.ends_with(".fdx") || name.ends_with(".scriptorium.json") || name.ends_with(".scriptorium.lock")
}

/// Where an agent may write into an attached review at `path`: inside
/// `dir`, the script's folder, once links are resolved, with no part
/// starting with `.`, and not a script, session or lock file. (The text
/// must also read as a review: `opened::agent_may_write` checks that.) The
/// canonical path, or the writer-facing reason.
pub fn agent_write_place(dir: &Path, path: &Path) -> Result<PathBuf, String> {
    let dir = dir.canonicalize().map_err(|e| format!("{}: {e}", dir.display()))?;
    let why = match place(&dir, path) {
        Place::Inside(at) if at.file_name().is_some_and(|n| is_script_file(&n.to_string_lossy())) => {
            "is a script or one of its own files, not a review"
        }
        Place::Inside(at) => return Ok(at),
        Place::Hidden => "is in a hidden folder or has a hidden name (starting with `.`)",
        Place::Outside => "is outside the script's folder",
        Place::NoFolder => "is in a folder that doesn't exist",
    };
    Err(not_writable(path, why, &dir))
}

/// The refusal for an attached file an agent may not write into: what's
/// wrong, and what the writer can do about it in the app.
pub fn not_writable(path: &Path, why: &str, dir: &Path) -> String {
    format!(
        "{} {why}: agents only add suggestions to review files inside the script's folder ({}). The writer can \
         copy it into that folder and attach the copy with File › Add review",
        path.display(),
        dir.display()
    )
}

/// A script path as given (by a person or a model), before anything is
/// written beside it (the lock, the session): it must be a regular `.fdx`
/// file (through a link, the file it points at). Returns it canonical.
pub fn check_script(script: &Path) -> Result<PathBuf, String> {
    let at = script.canonicalize().map_err(|e| format!("{}: {e}", script.display()))?;
    if !at.is_file() {
        return Err(format!("{}: not a file; give a Final Draft script (.fdx)", script.display()));
    }
    if !at.extension().is_some_and(|e| e.eq_ignore_ascii_case("fdx")) {
        return Err(format!("{}: not a Final Draft script (.fdx)", script.display()));
    }
    Ok(at)
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
    file: File,
}

impl Drop for SessionLock {
    /// Unlocks before the file closes. The OS lock belongs to the open file
    /// description, and a child process spawned from any thread holds a copy
    /// of it until the child execs (or forever, if it never does); closing
    /// only our descriptor would leave the lock held by that copy, and the
    /// next attempt (even from this process) would read as another program.
    /// Unlocking releases it for every copy.
    fn drop(&mut self) {
        // Through the trait, as with `try_lock`. An error leaves the close to
        // release it, which is all that could be done anyway.
        let _ = fs4::FileExt::unlock(&self.file);
    }
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
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).write(true);
        // Never through a link: a planted `.lock` link would otherwise
        // create or open a file elsewhere.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NOFOLLOW);
        #[cfg(not(unix))]
        if fs::symlink_metadata(lock_path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(LockError::Io(format!("{} is a link; not opening it", lock_path.display())));
        }
        let file = options.open(lock_path).map_err(|e| {
            // O_NOFOLLOW on a link is ELOOP: say so plainly.
            #[cfg(unix)]
            if e.raw_os_error() == Some(libc::ELOOP) {
                return LockError::Io(format!("{} is a link; not opening it", lock_path.display()));
            }
            LockError::Io(format!("{}: {e}", lock_path.display()))
        })?;
        let until = Instant::now() + wait;
        loop {
            // Through the trait: newer toolchains have an inherent
            // `File::try_lock`, and rust-version (1.88) doesn't.
            match fs4::FileExt::try_lock(&file) {
                Ok(()) => return Ok(Self { file }),
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

    #[cfg(unix)]
    #[test]
    fn private_writes_refuse_links() {
        let dir = std::env::temp_dir().join(format!("scriptorium-private-link-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("profile");
        fs::write(&target, "keep\n").unwrap();
        std::os::unix::fs::symlink(&target, dir.join("seen.json")).unwrap();
        assert!(write_atomic_private(&dir.join("seen.json"), b"{}").is_err());
        std::os::unix::fs::symlink(&target, dir.join("film.scriptorium.json")).unwrap();
        assert!(write_atomic_no_link(&dir.join("film.scriptorium.json"), b"{}").is_err());
        write_atomic_no_link(&dir.join("new.json"), b"{}").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep\n");
        assert!(fs::symlink_metadata(dir.join("seen.json")).unwrap().file_type().is_symlink());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_paths_stay_in_the_scripts_folder() {
        let base = std::env::temp_dir().join(format!("scriptorium-confine-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("film/reviews")).unwrap();
        fs::create_dir_all(base.join("film/.hidden")).unwrap();
        fs::write(base.join("outside.review.txt"), "SCRIPT REVIEW v1\n").unwrap();
        let dir = base.join("film");
        let canon = dir.canonicalize().unwrap();
        let in_folder = |why: &str| why.contains(&canon.display().to_string());
        // The folder given as is, or through a link to it (as macOS's /var
        // is a link to /private/var): both compare canonically.
        let mut dirs = vec![dir.clone()];
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&dir, base.join("alias")).unwrap();
            dirs.push(base.join("alias"));
        }
        for d in &dirs {
            assert_eq!(review_in_folder(d, "a.review.txt"), Ok(canon.join("a.review.txt")));
            let abs = canon.join("d.review.txt").to_string_lossy().into_owned();
            assert_eq!(review_in_folder(d, &abs), Ok(canon.join("d.review.txt")));
        }
        assert_eq!(review_in_folder(&dir, "reviews/b.REVIEW.txt"), Ok(canon.join("reviews/b.REVIEW.txt")));
        let outside = base.join("outside.review.txt").to_string_lossy().into_owned();
        for (bad, says) in [
            (outside.as_str(), "inside the script's folder"),
            ("../outside.review.txt", "`..`"),
            ("reviews/../a.review.txt", "`..`"),
            ("notes.txt", "end in .review.txt"),
            ("missing/x.review.txt", "doesn't exist"),
            ("/etc/x.review.txt", "inside the script's folder"),
            (".hidden/x.review.txt", "starting with `.`"),
            (".x.review.txt", "starting with `.`"),
            (".bashrc.d/x.review.txt", "starting with `.`"),
        ] {
            let e = review_in_folder(&dir, bad).unwrap_err();
            assert!(e.contains(says) && in_folder(&e), "{bad}: {e}");
        }
        // New folders: accepted only inside, never a dot-folder or through
        // a link, and never made here (the caller makes them when it writes).
        assert_eq!(
            agent_file_in_folder(&dir, "new/sub/c.review.txt", ".review.txt", Folders::MayCreate),
            Ok(canon.join("new/sub/c.review.txt"))
        );
        assert!(!canon.join("new").exists());
        let e = agent_file_in_folder(&dir, ".bashrc.d/x.review.txt", ".review.txt", Folders::MayCreate).unwrap_err();
        assert!(e.contains("starting with `.`"), "{e}");
        assert!(agent_file_in_folder(&dir, "out.fdx", ".fdx", Folders::MustExist).is_ok());
        assert!(agent_file_in_folder(&dir, "out.txt", ".fdx", Folders::MustExist).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(base.join("outside.review.txt"), dir.join("link.review.txt")).unwrap();
            std::os::unix::fs::symlink(&base, dir.join("up")).unwrap();
            std::os::unix::fs::symlink(base.join("film/notes"), dir.join("dangling.review.txt")).unwrap();
            assert!(review_in_folder(&dir, "link.review.txt").unwrap_err().contains("link"));
            assert!(review_in_folder(&dir, "up/new.review.txt").is_err());
            let e = agent_file_in_folder(&dir, "up/made/new.review.txt", ".review.txt", Folders::MayCreate);
            assert!(e.unwrap_err().contains("inside the script's folder"));
            // A dangling link is a link too: refused before anything writes.
            assert!(review_in_folder(&dir, "dangling.review.txt").unwrap_err().contains("link"));
        }
        // Where an agent may write into an attached file, and why not.
        assert!(agent_write_place(&dir, &canon.join("reviews/b.review.txt")).is_ok());
        for (path, says) in [
            (base.join("outside.review.txt"), "is outside the script's folder"),
            (canon.join(".hidden/x.review.txt"), "hidden"),
            (canon.join("missing/x.review.txt"), "folder that doesn't exist"),
            (canon.join("film.fdx"), "a script or one of its own files"),
            (canon.join("film.scriptorium.json"), "a script or one of its own files"),
            (canon.join("film.scriptorium.lock"), "a script or one of its own files"),
        ] {
            let e = agent_write_place(&dir, &path).unwrap_err();
            assert!(e.contains(says) && e.contains("File › Add review") && !e.contains("name a new"), "{e}");
        }
        #[cfg(unix)]
        {
            // An attached review in the folder that is a link out.
            let e = agent_write_place(&dir, &dir.join("link.review.txt")).unwrap_err();
            assert!(e.contains("outside"), "{e}");
        }
        assert!(plain_review_name("Dialogue pass.review.txt"));
        for bad in ["Makefile", ".zshenv", ".x.review.txt", "a/b.review.txt", "a\\b.review.txt", "run.sh"] {
            assert!(!plain_review_name(bad), "{bad}");
        }
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_script_is_a_regular_fdx_file() {
        let dir = std::env::temp_dir().join(format!("scriptorium-check-script-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("folder.fdx")).unwrap();
        fs::write(dir.join("film.FDX"), "x").unwrap();
        fs::write(dir.join(".bashrc"), "x").unwrap();
        assert!(check_script(&dir.join("film.FDX")).is_ok());
        for bad in [".bashrc", "folder.fdx", "missing.fdx"] {
            assert!(check_script(&dir.join(bad)).is_err(), "{bad}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_lock_never_follows_a_link() {
        let dir = std::env::temp_dir().join(format!("scriptorium-lock-link-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(dir.join("elsewhere"), dir.join("film.scriptorium.lock")).unwrap();
        assert!(matches!(
            SessionLock::acquire(&dir.join("film.scriptorium.lock"), Duration::ZERO),
            Err(LockError::Io(_))
        ));
        assert!(!dir.join("elsewhere").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn private_writes_create_owner_only_files_and_keep_existing_modes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("scriptorium-private-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let new = dir.join("t0001.json");
        write_atomic_private(&new, b"{}").unwrap();
        assert_eq!(mode(&new), 0o600);
        // A user's existing file keeps its mode, whichever write is used.
        let existing = dir.join("film.scriptorium.json");
        fs::write(&existing, "old").unwrap();
        fs::set_permissions(&existing, fs::Permissions::from_mode(0o644)).unwrap();
        write_atomic_private(&existing, b"new").unwrap();
        assert_eq!((mode(&existing), fs::read_to_string(&existing).unwrap().as_str()), (0o644, "new"));
        write_atomic(&existing, b"newer").unwrap();
        assert_eq!(mode(&existing), 0o644);
        // Nothing left behind, and every temporary name differs.
        let names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert_ne!(random_u64(), random_u64());
        let _ = fs::remove_dir_all(&dir);
    }

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
    fn dropping_releases_the_lock_even_while_a_copy_of_the_file_is_open() {
        // A child spawned while the lock is held keeps a copy of the open
        // file until it execs; `try_clone` makes the same kind of copy.
        let dir = std::env::temp_dir().join(format!("scriptorium-lock-copy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let at = dir.join("film.scriptorium.lock");
        let held = SessionLock::acquire(&at, Duration::ZERO).unwrap();
        let copy = held.file.try_clone().unwrap();
        drop(held);
        SessionLock::acquire(&at, Duration::ZERO).expect("released despite the copy");
        drop(copy);
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
