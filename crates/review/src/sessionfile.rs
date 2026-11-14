//! The saved review session: the document's event log, plus a fingerprint of
//! the script it applies to.
//!
//! The app and the CLI both read and write it (as `<script>.scriptorium.json`
//! next to the script), so a person in the app and an agent on the command
//! line work on the same decisions. Replaying the log over the script (with the
//! review files' saved decisions applied) gives back every edit and decision,
//! with full undo history.
//!
//! Pure: strings in, strings out.

use scriptorium_core::doc::Event;
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFile {
    pub version: u32,
    /// [`hash`] of the script file the events apply to.
    pub script_hash: String,
    pub events: Vec<Event>,
}

impl SessionFile {
    pub fn new(script_contents: &str, events: &[Event]) -> Self {
        Self { version: VERSION, script_hash: hash(script_contents), events: events.to_vec() }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("session serializes")
    }

    pub fn from_json(json: &str) -> Result<Self, String> {
        let f: Self = serde_json::from_str(json).map_err(|e| format!("unreadable session file: {e}"))?;
        if f.version != VERSION {
            return Err(format!("session file version {} is not supported", f.version));
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

/// Open the document for `script`: the review files' saved decisions, then
/// the saved session's events (`saved`, the session file's contents) if it was
/// made on this exact script.
pub fn resume(
    session: &crate::Session,
    script: scriptorium_core::Script,
    script_contents: &str,
    saved: Option<&str>,
) -> (scriptorium_core::Document, Resumed) {
    let base = session.open_document(script);
    let Some(saved) = saved else { return (base, Resumed::Fresh) };
    let file = match SessionFile::from_json(saved) {
        Ok(f) => f,
        Err(e) => return (base, Resumed::Discarded(e)),
    };
    if !file.is_for(script_contents) {
        return (base, Resumed::Discarded("the script has changed since the session was saved".into()));
    }
    match base.clone().replay_onto(&file.events) {
        Ok(doc) => (doc, Resumed::Resumed { events: file.events.len() }),
        Err(e) => (base, Resumed::Discarded(format!("the saved session doesn't apply: {e}"))),
    }
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
        let f = SessionFile::new("<FinalDraft/>", &[Event::Undo, Event::Redo]);
        let back = SessionFile::from_json(&f.to_json()).unwrap();
        assert_eq!(back, f);
        assert!(back.is_for("<FinalDraft/>"));
        assert!(!back.is_for("<FinalDraft />"));
    }
}
