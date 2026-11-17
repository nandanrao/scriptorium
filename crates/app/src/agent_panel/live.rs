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
use scriptorium_chat::exec::{ReviewError, ReviewStore, Script, create_new, new_version_result};
use scriptorium_core::{Document, ElementKind, ParaId};
use scriptorium_review::{RowResolver, Session};
use scriptorium_versions::PairView;

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
    fn pair_view(&self) -> Option<Result<PairView<'_>, String>> {
        self.view.pair_view(self.cx)
    }
    fn may_be_version(&self) -> bool {
        self.view.may_be_version()
    }

    /// Made under the session lock this store holds (the script is the
    /// original); the view then loads its pair again on its next look at
    /// the files.
    fn new_version(&mut self, language: &str, original_language: &str, switch: bool) -> Result<String, ReviewError> {
        self.writable()?;
        if let Some(why) = self.view.unwritten_changes(self.cx) {
            return Err(ReviewError::Failed(why));
        }
        let mut notes = vec![];
        let made = scriptorium_fsio::versions::version_new(
            self.view.script_path(),
            language,
            Some(original_language),
            None,
            &mut notes,
        );
        self.view.after_new_version(notes, self.cx);
        Ok(new_version_result(&made.map_err(ReviewError::Failed)?, switch))
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui::{TestAppContext, WindowHandle};
    use scriptorium_agent::ToolCall;
    use scriptorium_agent::file::ThreadFile;
    use scriptorium_chat::exec::{Caller, run_call};
    use serde_json::json;

    use super::*;

    const SCRIPT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Scene Heading">
      <Text>EXT. FARM HOUSE - NIGHT</Text>
    </Paragraph>
    <Paragraph Type="Character">
      <Text>CESAR</Text>
    </Paragraph>
    <Paragraph Type="Dialogue">
      <Text>I am going to town tomorrow.</Text>
    </Paragraph>
  </Content>
</FinalDraft>
"#;

    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("scriptorium-live-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let dir = dir.canonicalize().unwrap();
            std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
            Self(dir)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn open(cx: &mut TestAppContext, script: &Path) -> WindowHandle<ScriptView> {
        cx.update(gpui_base::init);
        let opened = crate::script_view::Opened::load(script, &[]).unwrap();
        cx.add_window(|window, cx| ScriptView::new(opened, window, cx))
    }

    fn thread() -> ThreadFile {
        ThreadFile {
            version: 1,
            id: "t0001".into(),
            title: "Translate".into(),
            created: String::new(),
            updated: String::new(),
            account: "work".into(),
            model: "m".into(),
            review_id: None,
            review_path: None,
            whole_script: None,
            system: String::new(),
            tools_hash: String::new(),
            thread: Default::default(),
            extra: Default::default(),
        }
    }

    fn call(name: &str, input: serde_json::Value) -> ToolCall {
        ToolCall { id: "c1".into(), name: name.into(), input, input_error: None, raw_input: None, extra: None }
    }

    /// The panel's agent makes a version on the original, and on the version
    /// sees the pair: `pair_view` and `read_rows` go through the live view.
    #[gpui::test]
    fn the_live_store_makes_a_version_and_reads_its_rows(cx: &mut TestAppContext) {
        let f = Folder::new("pair");
        let w = open(cx, &f.0.join("film.fdx"));
        let made = w
            .update(cx, |v, _, cx| {
                let mut script = LiveScript::new(v, cx);
                {
                    let store = script.open(false).unwrap();
                    assert!(store.pair_view().is_none(), "no pair yet");
                    assert!(!store.may_be_version());
                }
                run_call(
                    &mut script,
                    Caller::Thread(&mut thread()),
                    &call("new_version", json!({"language": "es", "original_language": "en"})),
                )
            })
            .unwrap();
        assert!(!made.result.is_error, "{}", made.result.content);
        assert!(
            made.result.content.contains(&f.0.join("film.es.fdx").display().to_string()),
            "{}",
            made.result.content
        );
        assert!(f.0.join("film.es.links.txt").exists());
        // The original's view loads its new pair on its next look.
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        w.update(cx, |v, _, cx| {
            assert!(v.notice_text().contains("Lined up with film.es.fdx"), "{}", v.notice_text());
            let store = LiveScript::new(v, cx).open(false).map(|s| s.pair_view().map(|p| p.map(|p| p.this)));
            assert_eq!(store.ok().flatten(), Some(Ok(0)));
        })
        .unwrap();

        // On the version: the pair, and `read_rows` lists the rows to do.
        let w = open(cx, &f.0.join("film.es.fdx"));
        let rows = w
            .update(cx, |v, _, cx| {
                let mut script = LiveScript::new(v, cx);
                {
                    let store = script.open(false).unwrap();
                    let view = store.pair_view().expect("a pair").expect("usable");
                    assert_eq!(view.this, 1);
                    assert!(store.may_be_version());
                }
                run_call(&mut script, Caller::Thread(&mut thread()), &call("read_rows", json!({"needs": true})))
            })
            .unwrap();
        assert!(!rows.result.is_error, "{}", rows.result.content);
        let out: serde_json::Value = serde_json::from_str(&rows.result.content).unwrap();
        let rows = out["rows"].as_array().unwrap();
        assert!(rows.iter().any(|r| r["needs"] == json!(true)), "{rows:?}");
    }
}
