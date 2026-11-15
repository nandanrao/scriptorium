//! A thread as stored on disk, with what must stay fixed for it.
//!
//! The shell reads and writes the file (atomically) and supplies the clock;
//! this only converts to and from JSON.

use serde::{Deserialize, Serialize};

use crate::AgentError;
use crate::protocol::{ToolSpec, fnv1a};
use crate::thread::Thread;

/// The format version this code writes.
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadFile {
    pub version: u32,
    pub id: String,
    pub title: String,
    /// When the thread was started, as the shell formats it (e.g. RFC 3339).
    pub created: String,
    /// When the thread last changed, formatted like `created`.
    #[serde(default)]
    pub updated: String,
    /// The account it runs on: an id meaningful to the shell's settings.
    pub account: String,
    pub model: String,
    /// The review the agent's suggestions go into, once it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_id: Option<String>,
    /// The system prompt the thread was started with. Sent unchanged on every
    /// turn: system and tools must stay byte-identical within a thread (see
    /// [`crate::protocol::build_request`]), even after the built-in prompt changes.
    pub system: String,
    /// [`tools_hash`] of the tools the thread was started with ([`check_tools`] compares). Like the
    /// system prompt, the tools must stay identical within a thread. When the
    /// tools available now hash differently, the shell should warn the writer
    /// that the model's earlier reasoning won't be replayed (or start a new
    /// thread).
    #[serde(default)]
    pub tools_hash: String,
    pub thread: Thread,
}

/// A stable hash of the tool specs as JSON (16 hex digits), for
/// [`ThreadFile::tools_hash`]. Changes whenever a name, description or
/// schema does, or the order of the tools.
pub fn tools_hash(tools: &[ToolSpec]) -> String {
    let json = serde_json::to_string(tools).expect("tool specs always serialize");
    format!("{:016x}", fnv1a(json.as_bytes()))
}

impl ThreadFile {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a thread always serializes")
    }

    /// Reads a file written by this or an earlier version. A newer version is
    /// refused rather than half-read (and then overwritten without its new data).
    pub fn from_json(s: &str) -> Result<Self, AgentError> {
        #[derive(Deserialize)]
        struct Header {
            version: u32,
        }
        let h: Header = serde_json::from_str(s).map_err(|e| AgentError::File(e.to_string()))?;
        if h.version == 0 || h.version > VERSION {
            return Err(AgentError::File(format!(
                "version {} is not supported (this version of Scriptorium reads up to {VERSION})",
                h.version
            )));
        }
        serde_json::from_str(s).map_err(|e| AgentError::File(e.to_string()))
    }
}

/// Whether the tools available now are the ones a thread was started with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolsCheck {
    /// The same tools (or a file from before tools were recorded: nothing to compare).
    Same,
    /// Different tools: warn the writer that the model's earlier reasoning
    /// won't be replayed, or start a new thread.
    Changed,
}

/// Compares `tools` with the [`ThreadFile::tools_hash`] the thread was
/// started with, so every shell makes the same decision.
pub fn check_tools(file: &ThreadFile, tools: &[ToolSpec]) -> ToolsCheck {
    if file.tools_hash.is_empty() || file.tools_hash == tools_hash(tools) {
        ToolsCheck::Same
    } else {
        ToolsCheck::Changed
    }
}
