//! What the agent panel gets from the open script: the values ScriptView's
//! `agent_*` methods hand out (see `script_view.rs`, "the agent panel's
//! hooks"). The live store (U5, `live.rs`) builds `chat::exec::ReviewStore`
//! on these; nothing else in the panel reaches into ScriptView.

// Used by the live store and the driver (U4, U5).
#![allow(dead_code)]

use std::fmt;
use std::rc::Rc;

use scriptorium_core::{Document, ElementKind, ParaId};
use scriptorium_fdx::FdxFile;
use scriptorium_review::Session;
use scriptorium_tools::Ctx;

/// What one tool call reads, taken from the live script at the moment of
/// the call (`ScriptView::agent_ctx`). Cheap to take: the session and the
/// `.fdx` are shared, the document is a copy.
pub(crate) struct AgentSnapshot {
    pub doc: Document,
    pub session: Rc<Session>,
    /// The script's file name, e.g. `film.fdx`.
    pub script_name: String,
    pub fdx: Rc<FdxFile>,
    /// The attached review with the id the caller asked for (the thread's
    /// own review), as an index into `session.reviews`.
    pub review: Option<usize>,
    /// The line the writer is rewriting right now, if any: proposals on it
    /// are refused ("The writer is editing this line right now").
    pub rewriting: Option<ParaId>,
}

impl AgentSnapshot {
    /// Whether the script shows element `kind` in capitals.
    pub fn is_caps(&self, kind: &ElementKind) -> bool {
        self.fdx.is_caps(kind.name())
    }

    /// Run `f` with the tools' context over this snapshot, `review` as the
    /// default review (never "the only attached review": see the plan).
    pub fn with_ctx<R>(&self, f: impl FnOnce(&Ctx) -> R) -> R {
        let is_caps = |k: &ElementKind| self.is_caps(k);
        f(&Ctx {
            doc: &self.doc,
            session: &self.session,
            script_name: &self.script_name,
            is_caps: &is_caps,
            default_review: self.review,
        })
    }
}

/// The session lock, held from `ScriptView::agent_review` to
/// `ScriptView::agent_write_review` (or until dropped), and the review it is
/// for. While it is held, the app's own watcher and autosave wait.
pub(crate) struct AgentLease {
    /// The review's index in the live session.
    pub r: usize,
    /// The review's id, checked again when writing.
    pub id: String,
    /// None when the OS can't lock (logged; the app goes on unlocked, as for
    /// its own saves).
    pub(crate) lock: Option<scriptorium_fsio::SessionLock>,
}

/// Why `ScriptView::agent_review` can't hand out the review now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentStoreError {
    /// Another program holds the session lock: retry shortly from the task
    /// (never wait on the UI thread).
    Busy,
    /// Another program changed the files while the writer is rewriting a
    /// line, so the app can't reload yet: retry like `Busy`.
    Wait,
    /// The review can't be read, created or attached: the reason, for the
    /// tool result.
    File(String),
}

impl fmt::Display for AgentStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy | Self::Wait => f.write_str("The script's files are busy in another program; try again."),
            Self::File(why) => f.write_str(why),
        }
    }
}
