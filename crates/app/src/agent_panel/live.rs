//! The live store: `chat::exec`'s [`Script`] and [`ReviewStore`] over the
//! open [`ScriptView`], so the agent's tool calls read and change the script
//! the writer sees, through the same apply path as `scriptorium chat`.
//!
//! A tool call runs inside *one* `ScriptView` update (the driver's, from its
//! task: never nested in a panel update), as
//! `exec::run_call(&mut LiveScript::new(view, cx), …)`. Nothing here waits:
//!
//! - Opening for writing takes the session lock with zero wait
//!   ([`ScriptView::try_lock_session`]); busy is [`ReviewError::Busy`], which
//!   `chat::host::run` retries off the UI thread. The lease is held until the
//!   store drops, so the files can't change between reading and writing.
//! - It then loads what other programs wrote ([`ScriptView::reload_or_busy`]);
//!   while the writer rewrites a line and the files changed, that is busy too
//!   (the app can't reload under an edit).
//! - [`ReviewStore::write`] writes only the review file, then
//!   [`ScriptView::after_agent_change`] marks the session dirty for the
//!   view's autosave. The agent never writes the session file.
//! - Proposals on the line being rewritten are refused
//!   ([`ScriptView::editing_paras`], via [`ReviewStore::editing_paras`]).
//! - Reading opens no lock and reloads nothing: the tools see the live
//!   document and session as they are.
//!
//! Agent proposals are not undo steps (appended review lines, like CLI
//! suggestions); the writer's decisions on them are, as for any review.

use std::path::{Path, PathBuf};

use gpui::Context;
use scriptorium_chat::exec::{ReviewError, ReviewStore, Script, create_new};
use scriptorium_core::{Document, ElementKind, ParaId};
use scriptorium_review::{RowResolver, Session};

use crate::script_view::{LiveError, ScriptView, SessionLease};

/// Logged and told to the writer once when the OS can't lock the session.
const NO_LOCK: &str = "The script's files can't be locked here; the agent writes without the lock.";

/// The open script view, for one tool call's [`Script::open`].
pub struct LiveScript<'a, 'b> {
    view: &'a mut ScriptView,
    cx: &'a mut Context<'b, ScriptView>,
    notes: Vec<String>,
}

impl<'a, 'b> LiveScript<'a, 'b> {
    pub fn new(view: &'a mut ScriptView, cx: &'a mut Context<'b, ScriptView>) -> Self {
        Self { view, cx, notes: vec![] }
    }
}

fn live_err(e: LiveError) -> ReviewError {
    match e {
        LiveError::Busy => ReviewError::Busy,
        LiveError::Failed(m) => ReviewError::Failed(m),
    }
}

impl Script for LiveScript<'_, '_> {
    fn open(&mut self, writes: bool) -> Result<Box<dyn ReviewStore + '_>, ReviewError> {
        let lease = if writes {
            let lease = self.view.try_lock_session().ok_or(ReviewError::Busy)?;
            if lease.is_none() && !self.notes.iter().any(|n| n == NO_LOCK) {
                crate::log::warn(NO_LOCK);
                self.notes.push(NO_LOCK.into());
            }
            self.view.reload_or_busy(self.cx).map_err(live_err)?;
            Some(lease)
        } else {
            None
        };
        let caps = Box::new(self.view.caps());
        let name = self.view.script_name();
        let dir = self.view.script_dir().to_path_buf();
        Ok(Box::new(LiveStore { view: &mut *self.view, cx: &mut *self.cx, _lease: lease, caps, name, dir }))
    }

    fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }
}

/// One opening of the live script. Holds the session lease (when opened for
/// writing) until dropped.
struct LiveStore<'s, 'b> {
    view: &'s mut ScriptView,
    cx: &'s mut Context<'b, ScriptView>,
    /// `Some` when opened for writing: the session lock, shared with the
    /// view's own saves while it lives.
    _lease: Option<SessionLease>,
    caps: Box<dyn Fn(&ElementKind) -> bool>,
    name: String,
    /// The script's folder, canonical (the view opens the script canonical).
    dir: PathBuf,
}

impl LiveStore<'_, '_> {
    fn writable(&self) -> Result<(), ReviewError> {
        match self._lease {
            Some(_) => Ok(()),
            None => Err(ReviewError::Failed("opened for reading only".into())),
        }
    }
}

impl ReviewStore for LiveStore<'_, '_> {
    fn dir(&self) -> &Path {
        &self.dir
    }
    fn script_name(&self) -> &str {
        &self.name
    }
    fn doc(&self) -> &Document {
        self.view.live_doc(self.cx)
    }
    fn session(&self) -> &Session {
        self.view.session()
    }
    fn is_caps(&self) -> &dyn Fn(&ElementKind) -> bool {
        &*self.caps
    }
    fn session_mut(&mut self) -> (&mut Session, &Document, &dyn Fn(&ElementKind) -> bool) {
        let (session, doc) = self.view.session_mut(self.cx);
        (session, doc, &*self.caps)
    }
    fn editing_paras(&self) -> Vec<ParaId> {
        self.view.editing_paras(self.cx)
    }
    fn row_resolver(&self) -> Option<Box<RowResolver<'static>>> {
        self.view.row_resolver(self.cx)
    }

    fn reload(&mut self) -> Result<(), ReviewError> {
        self.writable()?;
        self.view.reload_or_busy(self.cx).map_err(live_err)
    }

    fn attach(&mut self, path: &Path) -> Result<usize, ReviewError> {
        self.writable()?;
        let r = self.view.attach_review(path, self.cx).map_err(live_err)?;
        self.view.after_agent_change(self.cx);
        Ok(r)
    }

    fn create(&mut self, path: &Path, text: &str) -> Result<usize, ReviewError> {
        self.writable()?;
        create_new(path, text)?;
        self.attach(path)
    }

    fn write(&mut self, r: usize, text: String) -> Result<(), ReviewError> {
        self.writable()?;
        let path = self.view.review_file(r);
        scriptorium_fsio::write_atomic(&path, text.as_bytes())
            .map_err(|e| ReviewError::Failed(format!("{}: {e}", path.display())))?;
        self.view.after_agent_change(self.cx);
        Ok(())
    }
}
