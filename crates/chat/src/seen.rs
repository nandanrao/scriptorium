//! First-use disclosures already shown, per user: `seen.json` beside the
//! accounts file (`$XDG_CONFIG_HOME/scriptorium/`, macOS Application
//! Support), shared by the CLI and the app. Keys are
//! [`disclosure_key`](crate::model::disclosure_key)s: one per provider address (scheme, host, port)
//! and script, so a new host or a new script shows the disclosure again.
//!
//! Losing an entry (two programs saving at once, a damaged file) only means
//! the disclosure is shown again, so the file takes no lock, and a file that
//! can't be read counts as empty.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use scriptorium_fsio::write_atomic_private;
use serde_json::{Value, json};

/// The file's name, in the same folder as the accounts file.
pub const FILE_NAME: &str = "seen.json";

/// Where the file lives, from the environment; `None` without a home folder.
pub fn default_path() -> Option<PathBuf> {
    scriptorium_providers::shell::config::default_path().map(|p| p.with_file_name(FILE_NAME))
}

/// The disclosures shown so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    path: PathBuf,
    keys: BTreeSet<String>,
}

impl Seen {
    /// The file at `path`; missing or unreadable, none seen yet.
    pub fn load(path: &Path) -> Self {
        Seen { path: path.to_path_buf(), keys: read(path) }
    }

    pub fn contains(&self, key: &str) -> bool {
        self.keys.contains(key)
    }

    /// Records `key` as shown and saves the file (re-read first, so keys
    /// another program saved meanwhile are kept).
    pub fn insert(&mut self, key: &str) -> Result<(), String> {
        self.keys.extend(read(&self.path));
        self.keys.insert(key.to_string());
        // Keys from before 0.4.0's release named the host alone; they match
        // nothing now (keys start with `scheme://`), so they're dropped.
        self.keys.retain(|k| k.contains("://"));
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(&json!({ "version": 1, "disclosures": self.keys }))
            .expect("strings serialize");
        write_atomic_private(&self.path, text.as_bytes()).map_err(|e| format!("{}: {e}", self.path.display()))
    }
}

fn read(path: &Path) -> BTreeSet<String> {
    let Ok(text) = std::fs::read_to_string(path) else { return BTreeSet::new() };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return BTreeSet::new() };
    v.get("disclosures")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_and_merges() {
        let dir = std::env::temp_dir().join(format!("scriptorium-seen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("sub").join(FILE_NAME);
        let (k1, k2) = ("https://a.example:443\u{1f}/s/a.fdx", "https://b.example:443\u{1f}/s/a.fdx");
        let mut a = Seen::load(&path);
        assert!(!a.contains(k1));
        let mut b = Seen::load(&path);
        a.insert(k1).unwrap();
        b.insert(k2).unwrap();
        let c = Seen::load(&path);
        assert!(c.contains(k1) && c.contains(k2));
        // Old host-only keys are dropped on the next save.
        std::fs::write(&path, r#"{"version": 1, "disclosures": ["api.anthropic.com\u001f/s/a.fdx"]}"#).unwrap();
        assert!(Seen::load(&path).contains("api.anthropic.com\u{1f}/s/a.fdx"));
        Seen::load(&path).insert(k1).unwrap();
        let d = Seen::load(&path);
        assert!(d.contains(k1) && !d.contains("api.anthropic.com\u{1f}/s/a.fdx"));
        std::fs::write(&path, "not json").unwrap();
        assert!(!Seen::load(&path).contains(k1));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
