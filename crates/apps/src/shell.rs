//! Reading and writing apps' folders (feature `shell`): the list of apps,
//! their pages, their data, and the stamps that tell the server an app's
//! files changed.
//!
//! Every function takes `dir`, the open script's folder, and follows one
//! rule (AGENTS.md rule 6, the plan's "data path safety"): each part below
//! `dir` is `lstat`ed, `apps` itself, `apps/<name>` and `apps/<name>/data`
//! included, and **any symlink anywhere is refused**. The file's resolved
//! path must then still lie under the right folder (`apps/<name>/data/` for
//! data, `apps/<name>/` outside `data/` for pages). Checked, then used: a
//! part swapped for a link in between needs someone who can already write
//! the folder, so that race isn't guarded against (as rule 6 says).
//! Hard links inside `data/` can't be detected: writes replace by rename,
//! so they never write through one, but a read would show the linked file,
//! to someone who can already write the folder.
//!
//! [`scan`] is cheap enough to call whenever the Apps menu opens or the
//! window gains focus: one `read_dir` of `apps/`, then per app (at most
//! [`APPS_MAX`]) two `lstat`s and one small read of `app.json`. It never
//! walks an app's files. [`stamp`] does walk them (outside `data/`), so it
//! is for the server's once-a-second thread while a page listens.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, Metadata};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::DATA_MAX;
use crate::list::Found;
use crate::manifest::{self, Problem};
use crate::paths::{self, Part};

/// The most apps listed (sorted by name, the rest left out).
pub const APPS_MAX: usize = 50;
/// The most of `app.json` read; a manifest is a few lines.
pub const MANIFEST_MAX: u64 = 64 * 1024;
/// The most files listed in an app's `data/`, or stamped across all apps.
pub const FILES_MAX: usize = 10_000;
/// The deepest folder walked below an app or its `data/`.
pub const DEPTH_MAX: usize = 16;

/// Why a file wasn't read, written or removed.
#[derive(Debug)]
pub enum FileError {
    /// Nothing there (or not a file, or a bad app name). The server answers
    /// 404.
    NotFound,
    /// A link somewhere on the way, or a path that resolves outside its
    /// folder. The server answers 404, as for a missing file, and may log
    /// the reason.
    Refused(String),
    /// Over [`DATA_MAX`] (20 MB). The server answers 413.
    TooLarge,
    /// The disk said no.
    Io(io::Error),
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileError::NotFound => write!(f, "not found"),
            FileError::Refused(why) => write!(f, "refused: {why}"),
            FileError::TooLarge => write!(f, "larger than {} MB", DATA_MAX / (1024 * 1024)),
            FileError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for FileError {}

impl From<io::Error> for FileError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::NotFound { FileError::NotFound } else { FileError::Io(e) }
    }
}

/// One file in an app's `data/`: its path inside `data/` (with `/`) and
/// its size in bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataFile {
    pub path: String,
    pub size: u64,
}

/// The apps in `dir/apps/`, sorted by name, at most [`APPS_MAX`]. Each
/// folder that isn't hidden and isn't a link is an app, listed with every
/// problem that stops it opening (a bad name, no or a broken `app.json`,
/// no `index.html`). `apps` missing, not a folder, or a link: no apps.
pub fn scan(dir: &Path) -> Vec<Found> {
    app_folders(dir)
        .into_iter()
        .map(|(name, folder)| {
            let mut problems = Vec::new();
            if !manifest::name_ok(&name) {
                problems.push(Problem::BadName(name.clone()));
            }
            let manifest = match read_manifest(&folder.join("app.json")) {
                None => {
                    problems.push(Problem::NoManifest);
                    None
                }
                Some(text) => match manifest::parse(&text) {
                    Ok(m) => Some(m),
                    Err(p) => {
                        problems.extend(p);
                        None
                    }
                },
            };
            if !regular_file(&folder.join("index.html")) {
                problems.push(Problem::NoIndex);
            }
            Found { name, folder, manifest, problems }
        })
        .collect()
}

/// Every app's stamp (see [`crate::events::stamp`]): its files' paths,
/// sizes and modification times, leaving out `data/` (any case), hidden
/// parts and links, for [`crate::events::Quiet::step`]. The same folders as
/// [`scan`].
pub fn stamp(dir: &Path) -> BTreeMap<String, u64> {
    let mut left = FILES_MAX;
    app_folders(dir)
        .into_iter()
        .map(|(name, folder)| {
            let mut rows = Vec::new();
            walk_files(&folder, "", true, 0, left, &mut rows);
            left -= rows.len();
            let stamp = crate::events::stamp(rows.iter().map(|(p, m)| (p.as_str(), m.len(), mtime(m))));
            (name, stamp)
        })
        .collect()
}

/// A page's file from `apps/<name>/`: `parts` are the request's parts after
/// the name (none means `index.html`); `data/…` is refused.
pub fn read_static(dir: &Path, name: &str, parts: &[Part]) -> Result<Vec<u8>, FileError> {
    parts_ok(parts)?;
    let parts = paths::static_path(parts).map_err(|r| FileError::Refused(r.to_string()))?;
    let (root, top) = app_root(dir, name, false)?;
    read_checked(&root, &top, &parts)
}

/// The files in `apps/<name>/data/`, sorted by path; links and hidden
/// parts are left out. No `data/` yet: none. `data/` a link: refused.
pub fn data_list(dir: &Path, name: &str) -> Result<Vec<DataFile>, FileError> {
    let (root, top) = app_root(dir, name, true)?;
    let data = top.join("data");
    match chain(&root, &top, &[] as &[Part]) {
        Ok(_) => {}
        Err(FileError::NotFound) => return Ok(Vec::new()),
        Err(e) => return Err(e),
    }
    let mut rows = Vec::new();
    walk_files(&data, "", false, 0, FILES_MAX, &mut rows);
    let mut out: Vec<DataFile> = rows.into_iter().map(|(path, m)| DataFile { path, size: m.len() }).collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// A file from `apps/<name>/data/`; `parts` are those after `data`.
pub fn data_read(dir: &Path, name: &str, parts: &[Part]) -> Result<Vec<u8>, FileError> {
    let (root, top) = app_root(dir, name, true)?;
    read_checked(&root, &top, parts)
}

/// Replaces (or creates) a file in `apps/<name>/data/`, making `data/` and
/// any folders on the way; atomic. Over [`DATA_MAX`]: refused before
/// anything is made. The app's folder must exist.
pub fn data_write(dir: &Path, name: &str, parts: &[Part], bytes: &[u8]) -> Result<(), FileError> {
    if bytes.len() as u64 > DATA_MAX {
        return Err(FileError::TooLarge);
    }
    parts_ok(parts)?;
    if parts.is_empty() {
        return Err(FileError::NotFound);
    }
    let (root, top) = app_root(dir, name, true)?;
    let data = top.join("data");
    let path = parts.iter().fold(data.clone(), |p, part| p.join(part));
    // Every part that exists, `data` included, must not be a link.
    let missing = match chain(&root, &top, parts) {
        Ok(m) if m.is_dir() => return Err(FileError::NotFound),
        Ok(_) => false,
        Err(FileError::NotFound) => true,
        Err(e) => return Err(e),
    };
    if missing {
        // Make the missing folders one at a time, each checked as made.
        let mut folder = data.clone();
        for part in std::iter::once(None).chain(parts[..parts.len() - 1].iter().map(Some)) {
            if let Some(part) = part {
                folder.push(part);
            }
            match fs::symlink_metadata(&folder) {
                Ok(m) if m.file_type().is_symlink() => return Err(refused_link(&folder)),
                Ok(m) if m.is_dir() => {}
                Ok(_) => return Err(FileError::NotFound),
                Err(e) if e.kind() == io::ErrorKind::NotFound => match fs::create_dir(&folder) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                        if fs::symlink_metadata(&folder)?.file_type().is_symlink() {
                            return Err(refused_link(&folder));
                        }
                    }
                    Err(e) => return Err(e.into()),
                },
                Err(e) => return Err(e.into()),
            }
        }
    }
    let parent = path.parent().unwrap_or(&data);
    under(parent, &data, true)?;
    scriptorium_fsio::write_atomic_no_link(&path, bytes)
        .map_err(|e| if e.kind() == io::ErrorKind::InvalidInput { FileError::Refused(e.to_string()) } else { e.into() })
}

/// Removes a file from `apps/<name>/data/` (never a folder, never a link).
pub fn data_remove(dir: &Path, name: &str, parts: &[Part]) -> Result<(), FileError> {
    parts_ok(parts)?;
    if parts.is_empty() {
        return Err(FileError::NotFound);
    }
    let (root, top) = app_root(dir, name, true)?;
    let meta = chain(&root, &top, parts)?;
    if !meta.is_file() {
        return Err(FileError::NotFound);
    }
    let path = parts.iter().fold(top.join("data"), |p, part| p.join(part));
    under(&path, &top.join("data"), false)?;
    fs::remove_file(&path)?;
    Ok(())
}

/// `apps/` checked and listed: (name, folder) for each folder in it that
/// isn't hidden or a link, sorted, at most [`APPS_MAX`].
fn app_folders(dir: &Path) -> Vec<(String, PathBuf)> {
    let apps = dir.join("apps");
    match fs::symlink_metadata(&apps) {
        Ok(m) if m.is_dir() => {}
        _ => return Vec::new(),
    }
    let Ok(entries) = fs::read_dir(&apps) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            // `file_type` doesn't follow links: a linked folder isn't a dir.
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            (is_dir && !name.starts_with('.')).then(|| (name, e.path()))
        })
        .collect();
    out.sort();
    out.truncate(APPS_MAX);
    out
}

/// The folder the request's parts start from, and the app's folder, with
/// `apps` and `apps/<name>` checked: `data` is `<app>/data` for data, else
/// the app's folder. The name must be an app's name.
fn app_root(dir: &Path, name: &str, data: bool) -> Result<(PathBuf, PathBuf), FileError> {
    if !manifest::name_ok(name) {
        return Err(FileError::NotFound);
    }
    let apps = dir.join("apps");
    let top = apps.join(name);
    for p in [&apps, &top] {
        let m = fs::symlink_metadata(p)?;
        if m.file_type().is_symlink() {
            return Err(refused_link(p));
        }
        if !m.is_dir() {
            return Err(FileError::NotFound);
        }
    }
    Ok((if data { top.join("data") } else { top.clone() }, top))
}

/// `lstat`s `root` (when it isn't `top`) and then each part below it; any
/// link is refused, a missing part is `NotFound`, a part on the way that
/// isn't a folder is `NotFound`. The last one's metadata.
fn chain(root: &Path, top: &Path, parts: &[Part]) -> Result<Metadata, FileError> {
    let mut path = root.to_path_buf();
    let mut meta = fs::symlink_metadata(&path)?;
    if root != top && meta.file_type().is_symlink() {
        return Err(refused_link(&path));
    }
    for part in parts {
        if !meta.is_dir() {
            return Err(FileError::NotFound);
        }
        part_ok(part)?;
        path.push(part);
        meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            return Err(refused_link(&path));
        }
    }
    Ok(meta)
}

/// A file read after [`chain`] and the resolved-path check: under `root`,
/// and for a page (`root == top`) not under `data/`.
fn read_checked(root: &Path, top: &Path, parts: &[Part]) -> Result<Vec<u8>, FileError> {
    parts_ok(parts)?;
    let meta = chain(root, top, parts)?;
    if !meta.is_file() {
        return Err(FileError::NotFound);
    }
    if meta.len() > DATA_MAX {
        return Err(FileError::TooLarge);
    }
    let path = parts.iter().fold(root.to_path_buf(), |p, part| p.join(part));
    let real = under(&path, root, false)?;
    if root == top {
        let data = fs::canonicalize(top)?.join("data");
        if real.starts_with(&data) {
            return Err(FileError::Refused("an app's data/ isn't served as a page".into()));
        }
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    fs::File::open(&real)?.take(DATA_MAX + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > DATA_MAX {
        return Err(FileError::TooLarge);
    }
    Ok(bytes)
}

/// `path` resolved, and checked to lie under `folder` resolved (or to be
/// it, with `may_be_folder`). The resolved path.
fn under(path: &Path, folder: &Path, may_be_folder: bool) -> Result<PathBuf, FileError> {
    let real = fs::canonicalize(path)?;
    let base = fs::canonicalize(folder)?;
    if real.starts_with(&base) && (may_be_folder || real != base) {
        Ok(real)
    } else {
        Err(FileError::Refused(format!("{} isn't inside {}", path.display(), folder.display())))
    }
}

/// [`part_ok`] for every part, before anything on disk is looked at or
/// made.
fn parts_ok(parts: &[Part]) -> Result<(), FileError> {
    parts.iter().try_for_each(|p| part_ok(p))
}

/// The shell's own check of one part, whatever the caller did: not empty,
/// not hidden (`.`, `..`, `.git`), no `/`, `\`, `:` or NUL.
fn part_ok(part: &str) -> Result<(), FileError> {
    if part.is_empty() || part.starts_with('.') || part.contains(['/', '\\', ':', '\0']) {
        return Err(FileError::Refused(format!("the path part \"{part}\" isn't allowed")));
    }
    Ok(())
}

fn refused_link(path: &Path) -> FileError {
    FileError::Refused(format!("{} is a link", path.display()))
}

/// `app.json`'s text, if it is a regular file (not a link), read up to
/// [`MANIFEST_MAX`].
fn read_manifest(path: &Path) -> Option<String> {
    if !regular_file(path) {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(path).ok()?.take(MANIFEST_MAX).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// A regular file, not a link.
fn regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
}

/// Every regular file under `folder` (recursively, links and hidden parts
/// left out, at most [`DEPTH_MAX`] folders deep, until `out` holds `max`)
/// as (path with `/`, metadata); `skip_data` leaves out the top-level
/// `data` (any case).
fn walk_files(
    folder: &Path,
    prefix: &str,
    skip_data: bool,
    depth: usize,
    max: usize,
    out: &mut Vec<(String, Metadata)>,
) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        if out.len() >= max {
            return;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') || (skip_data && prefix.is_empty() && name.eq_ignore_ascii_case("data")) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let rel = if prefix.is_empty() { name.to_string() } else { format!("{prefix}/{name}") };
        if meta.file_type().is_symlink() {
            continue;
        } else if meta.is_dir() {
            if depth < DEPTH_MAX {
                walk_files(&entry.path(), &rel, false, depth + 1, max, out);
            }
        } else if meta.is_file() {
            out.push((rel, meta));
        }
    }
}

/// Modification time in nanoseconds since 1970 (0 when unknown).
fn mtime(meta: &Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests;
