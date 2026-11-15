//! The accounts file on disk: `$XDG_CONFIG_HOME/scriptorium/accounts.json`
//! (`~/.config/…` without it) on Linux and other Unixes,
//! `~/Library/Application Support/scriptorium/accounts.json` on macOS,
//! `%APPDATA%\scriptorium\accounts.json` on Windows.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ProvidersError;
use crate::account::AccountsFile;

/// The file's name inside the config directory.
pub const FILE_NAME: &str = "accounts.json";

/// Where the accounts file lives, from the environment; `None` if there's
/// no home directory to put it in.
pub fn default_path() -> Option<PathBuf> {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    config_dir(var).map(|d| d.join("scriptorium").join(FILE_NAME))
}

/// The per-user config directory, given a way to read environment variables.
fn config_dir(var: impl Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    if cfg!(windows) {
        return var("APPDATA");
    }
    if cfg!(target_os = "macos") {
        return var("HOME").map(|h| h.join("Library").join("Application Support"));
    }
    var("XDG_CONFIG_HOME").filter(|p| p.is_absolute()).or_else(|| var("HOME").map(|h| h.join(".config")))
}

/// Reads the accounts file; a missing file is an empty one.
pub fn load(path: &Path) -> Result<AccountsFile, ProvidersError> {
    match std::fs::read_to_string(path) {
        Ok(text) => AccountsFile::from_json(&text)
            .map_err(|e| ProvidersError::Config(format!("{}: {}", path.display(), strip_prefix(e)))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AccountsFile::default()),
        Err(e) => Err(ProvidersError::Config(format!("can't read {}: {e}", path.display()))),
    }
}

fn strip_prefix(e: ProvidersError) -> String {
    match e {
        ProvidersError::Config(m) => m,
        other => other.to_string(),
    }
}

/// Checks and writes the accounts file atomically (a temporary file in the
/// same directory, then a rename), creating its directory if needed.
pub fn save(path: &Path, file: &AccountsFile) -> Result<(), ProvidersError> {
    file.check()?;
    let err = |what: &str, e: std::io::Error| ProvidersError::Config(format!("can't {what} {}: {e}", path.display()));
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| err("create the folder for", e))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| FILE_NAME.into());
    // Unique per process and per call, so two saves never share a temp file.
    static SAVES: AtomicU64 = AtomicU64::new(0);
    let n = SAVES.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.{}.{n}.tmp", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(file.to_json().as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        // Make the rename itself durable. (Directories can't be opened for
        // syncing on Windows; there the rename is as durable as it gets.)
        #[cfg(unix)]
        std::fs::File::open(dir)?.sync_all()?;
        Ok(())
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        err("write", e)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Account, Provider};

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("scriptorium-providers-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn missing_is_empty_and_saves_round_trip() {
        let dir = scratch("config");
        let path = dir.join("nested").join(FILE_NAME);
        assert_eq!(load(&path).unwrap(), AccountsFile::default());
        let mut f = AccountsFile::default();
        f.upsert(Account::new("work", "Work", Provider::Anthropic)).unwrap();
        f.set_default("work").unwrap();
        save(&path, &f).unwrap();
        assert_eq!(load(&path).unwrap(), f);
        // No temporary files left behind.
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bad_files_name_the_path() {
        let dir = scratch("config-bad");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, "{\"version\": 99}").unwrap();
        let e = load(&path).unwrap_err().to_string();
        assert!(e.contains(FILE_NAME) && e.contains("99"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn config_dirs() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| PathBuf::from(v))
        };
        if cfg!(all(unix, not(target_os = "macos"))) {
            assert_eq!(config_dir(env(&[("XDG_CONFIG_HOME", "/x"), ("HOME", "/h")])), Some("/x".into()));
            assert_eq!(config_dir(env(&[("XDG_CONFIG_HOME", "rel"), ("HOME", "/h")])), Some("/h/.config".into()));
            assert_eq!(config_dir(env(&[])), None);
        }
    }
}
