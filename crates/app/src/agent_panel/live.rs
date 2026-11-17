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
//! - The host operations an outside agent has (decide, edit, undo/redo,
//!   write the script, attach, detach) run through the view
//!   (`script_view/agent_ops.rs`), with the files' words; the agent link
//!   saves the session before it answers.
//! - A review an outside agent's `review_file` makes ([`ReviewStore::add_new`])
//!   is kept attached through a reload once written, like one the view
//!   attached.
//!
//! Agent proposals are not undo steps (appended review lines, like CLI
//! suggestions); the writer's decisions on them are, as for any review.

use std::path::{Path, PathBuf};

use gpui::Context;
use scriptorium_chat::exec::{ReviewError, ReviewStore, Script, create_new, new_version_result};
use scriptorium_core::{Document, ElementKind, ParaId};
use scriptorium_fsio::ops::{Choice, SuggestionRef};
use scriptorium_review::{Review, RowResolver, Session};
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
        Ok(Box::new(LiveStore {
            view: &mut *self.view,
            cx: &mut *self.cx,
            _lease: lease,
            caps,
            name,
            dir,
            added: vec![],
        }))
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
    /// Stored paths of reviews this opening added before writing them
    /// ([`ReviewStore::add_new`]): once written, the view keeps them attached
    /// through a reload until the session is saved.
    added: Vec<String>,
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
        if self.added.contains(&self.view.session().reviews[r].path) {
            self.view.keep_attached(path);
        }
        self.view.after_agent_change(self.cx);
        Ok(())
    }

    /// Added to the view's session; once written ([`Self::write`]) it is
    /// kept attached through a reload, like a review the view attached. (Not
    /// before: one taken out again unwritten has no file to keep.)
    fn add_new(&mut self, review: Review) -> Result<usize, ReviewError> {
        let path = review.path.clone();
        let r = self.session_mut().0.add(review).map_err(ReviewError::Failed)?;
        self.added.push(path);
        Ok(r)
    }

    // The host operations, through the view (`script_view/agent_ops.rs`).

    fn attach_reviews(&mut self, files: &[PathBuf]) -> Result<String, ReviewError> {
        self.writable()?;
        self.view.agent_attach(files, self.cx).map_err(live_err)
    }

    /// `files` already resolved against [`Self::dir`] (`Script::detach`'s
    /// default): the view's session, never the session file under it (its
    /// next save would write the review back).
    fn detach(&mut self, files: &[String]) -> Result<String, ReviewError> {
        self.writable()?;
        self.view.agent_detach(files, self.cx).map_err(live_err)
    }

    fn decide(&mut self, review: Option<&str>, n: SuggestionRef, choice: Choice) -> Result<String, ReviewError> {
        self.writable()?;
        self.view.agent_decide(review, n, choice, self.cx).map_err(live_err)
    }

    fn edit(&mut self, para: u64, text: &str) -> Result<String, ReviewError> {
        self.writable()?;
        self.view.agent_edit(para, text, self.cx).map_err(live_err)
    }

    fn undo_redo(&mut self, redo: bool) -> Result<String, ReviewError> {
        self.writable()?;
        self.view.agent_undo_redo(redo, self.cx).map_err(live_err)
    }

    fn write_script(&mut self, output: Option<&str>) -> Result<String, ReviewError> {
        self.writable()?;
        self.view.agent_write(output, self.cx).map_err(live_err)
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

    /// An outside agent's call on the live view, as the agent link runs it.
    fn agent(v: &mut ScriptView, cx: &mut Context<ScriptView>, name: &str, input: serde_json::Value) -> (String, bool) {
        let done = run_call(&mut LiveScript::new(v, cx), Caller::Agent, &call(name, input));
        (done.result.content, done.result.is_error)
    }

    fn ok(got: (String, bool)) -> String {
        assert!(!got.1, "{}", got.0);
        got.0
    }

    fn err(got: (String, bool)) -> String {
        assert!(got.1, "not refused: {}", got.0);
        got.0
    }

    fn line(v: &ScriptView, cx: &gpui::App, para: u64) -> String {
        v.live_doc(cx).paragraph(ParaId(para)).map(|p| p.text()).unwrap_or_default()
    }

    const GOING: &str = "I am going to town tomorrow.";

    /// Each host operation on the live view, with the files' words; an
    /// agent's decision is an undo step; its `.fdx` is Write .fdx's, byte
    /// for byte.
    #[gpui::test]
    fn the_host_operations_run_on_the_live_view(cx: &mut TestAppContext) {
        let f = Folder::new("ops");
        let w = open(cx, &f.0.join("film.fdx"));
        w.update(cx, |v, window, cx| {
            let suggest =
                json!({"paragraph_id": 2, "options": ["I go to town tomorrow."], "review_file": "notes.review.txt"});
            ok(agent(v, cx, "suggest_change", suggest));
            // Decide, and the card moves on (none left).
            let out = ok(agent(v, cx, "decide_suggestion", json!({"n": 1, "decision": "accept"})));
            assert!(out.starts_with("#1 ") && out.contains("I go to town tomorrow."), "{out}");
            assert_eq!(line(v, cx, 2), "I go to town tomorrow.");
            // Undo takes the agent's decision back; redo puts it back.
            assert_eq!(ok(agent(v, cx, "undo", json!({}))), format!("¶2: {GOING}"));
            assert_eq!(line(v, cx, 2), GOING);
            ok(agent(v, cx, "redo", json!({})));
            assert_eq!(line(v, cx, 2), "I go to town tomorrow.");
            let out = err(agent(v, cx, "decide_suggestion", json!({"n": 1, "decision": "accept", "review": "nope"})));
            assert!(out.contains("no attached review matches \"nope\""), "{out}");
            // Edit a line.
            assert_eq!(
                ok(agent(v, cx, "edit_paragraph", json!({"paragraph_id": 0, "text": "INT. BARN - DAY"}))),
                "¶0: INT. BARN - DAY"
            );
            assert_eq!(line(v, cx, 0), "INT. BARN - DAY");
            err(agent(v, cx, "edit_paragraph", json!({"paragraph_id": 99, "text": "x"})));
            // Write: the default output, as Write .fdx writes it.
            let out = ok(agent(v, cx, "write_script", json!({})));
            assert!(out.contains("film (scriptorium).fdx") && out.contains("2 paragraphs changed"), "{out}");
            let by_agent = std::fs::read(f.0.join("film (scriptorium).fdx")).unwrap();
            std::fs::remove_file(f.0.join("film (scriptorium).fdx")).unwrap();
            v.apply_to_fdx(&crate::script_view::ApplyToFdx, window, cx);
            assert_eq!(std::fs::read(f.0.join("film (scriptorium).fdx")).unwrap(), by_agent);
            ok(agent(v, cx, "write_script", json!({"output": "copy.fdx"})));
            assert_eq!(std::fs::read(f.0.join("copy.fdx")).unwrap(), by_agent);
            let out = err(agent(v, cx, "write_script", json!({"output": "film.fdx"})));
            assert!(out.contains("already exists"), "{out}");
            err(agent(v, cx, "write_script", json!({"output": "../out.fdx"})));
            // Detach, then attach again: the text keeps the decision.
            let out = ok(agent(v, cx, "detach_reviews", json!({"files": ["notes.review.txt"]})));
            // Named as the files route names it: resolved in the script's folder.
            let at = f.0.join("notes.review.txt");
            assert_eq!(out, format!("removed {} (the script's text keeps its decisions)", at.display()));
            assert!(v.session().reviews.is_empty());
            assert_eq!(line(v, cx, 2), "I go to town tomorrow.");
            let out = err(agent(v, cx, "detach_reviews", json!({"files": ["notes.review.txt"]})));
            assert!(out.contains("isn't attached"), "{out}");
            let out = ok(agent(v, cx, "attach_reviews", json!({"files": ["notes.review.txt"]})));
            assert_eq!(out, "attached notes.review.txt (1 suggestions)");
            assert_eq!(v.session().reviews.len(), 1);
            // Attaching it again changes nothing.
            ok(agent(v, cx, "attach_reviews", json!({"files": ["notes.review.txt"]})));
            assert_eq!(v.session().reviews.len(), 1);
            std::fs::write(f.0.join("run.sh"), "echo hi").unwrap();
            err(agent(v, cx, "attach_reviews", json!({"files": ["run.sh"]})));
        })
        .unwrap();
    }

    /// Whether an outside agent's call was busy (to be tried again).
    fn busy(v: &mut ScriptView, cx: &mut Context<ScriptView>, name: &str, input: serde_json::Value) -> bool {
        run_call(&mut LiveScript::new(v, cx), Caller::Agent, &call(name, input)).busy
    }

    /// While the writer rewrites a line, an agent changes nothing in the
    /// session (it couldn't be saved before the reply): busy, to be tried
    /// again. Suggestions into an attached review go on.
    #[gpui::test]
    fn nothing_changes_while_the_writer_rewrites(cx: &mut TestAppContext) {
        let f = Folder::new("rewriting");
        let w = open(cx, &f.0.join("film.fdx"));
        w.update(cx, |v, window, cx| {
            let suggest =
                json!({"paragraph_id": 2, "options": ["I go to town tomorrow."], "review_file": "notes.review.txt"});
            ok(agent(v, cx, "suggest_change", suggest));
            let es = json!({"paragraph_id": 0, "options": ["EXT. GRANJA - NOCHE"], "review_file": "es.review.txt"});
            ok(agent(v, cx, "suggest_change", es));
            let id = v.session().reviews[0].id.clone();
            v.rewrite_for_test(&id, 1, window, cx);
            assert!(v.is_rewriting());
            let typed = line(v, cx, 2);
            v.take_autosave_for_test();
            assert!(busy(v, cx, "undo", json!({})));
            assert!(busy(v, cx, "redo", json!({})));
            assert!(busy(v, cx, "write_script", json!({})));
            assert!(!f.0.join("film (scriptorium).fdx").exists());
            assert!(busy(v, cx, "decide_suggestion", json!({"n": 1, "decision": "keep"})));
            let edit = |p: u64| json!({"paragraph_id": p, "text": "Hello."});
            assert_eq!(err(agent(v, cx, "edit_paragraph", edit(2))), scriptorium_chat::exec::EDITING_LINE);
            assert!(busy(v, cx, "edit_paragraph", edit(0)));
            assert!(busy(v, cx, "detach_reviews", json!({"files": ["notes.review.txt"]})));
            assert!(busy(v, cx, "attach_reviews", json!({"files": ["notes.review.txt"]})));
            assert!(!v.take_autosave_for_test(), "a refused call marks nothing changed");
            // The rewrite is untouched.
            assert!(v.is_rewriting());
            assert_eq!(line(v, cx, 2), typed);
            assert_eq!(v.session().reviews.len(), 2);
            // A translation into an attached review goes on; the line
            // being rewritten is refused.
            let batch = json!({"suggestions": [
                {"paragraph_id": 1, "options": ["CÉSAR"]},
                {"paragraph_id": 2, "options": ["Voy al pueblo mañana."]},
            ], "review_file": "es.review.txt"});
            let out = ok(agent(v, cx, "suggest_changes", batch));
            assert!(out.contains(scriptorium_chat::exec::EDITING_LINE), "{out}");
            assert_eq!(v.session().reviews[1].items.len(), 2);
            assert!(std::fs::read_to_string(f.0.join("es.review.txt")).unwrap().contains("CÉSAR"));
            assert!(v.is_rewriting());
        })
        .unwrap();
    }

    /// A call that fails changes nothing, as on the files (which save
    /// nothing then): a review it attached is taken off again, all of
    /// `attach_reviews` or none, and the view isn't marked changed.
    #[gpui::test]
    fn a_failed_call_leaves_the_view_as_it_was(cx: &mut TestAppContext) {
        let f = Folder::new("failed");
        let w = open(cx, &f.0.join("film.fdx"));
        w.update(cx, |v, _, cx| {
            let suggest =
                json!({"paragraph_id": 2, "options": ["I go to town tomorrow."], "review_file": "notes.review.txt"});
            ok(agent(v, cx, "suggest_change", suggest));
            // The same review under another name: attaching both fails.
            std::fs::copy(f.0.join("notes.review.txt"), f.0.join("copy.review.txt")).unwrap();
            ok(agent(v, cx, "detach_reviews", json!({"files": ["notes.review.txt"]})));
            assert!(v.session().reviews.is_empty());
            v.take_autosave_for_test();
            let both = json!({"files": ["notes.review.txt", "copy.review.txt"]});
            let out = err(agent(v, cx, "attach_reviews", both));
            assert!(out.contains("same file"), "{out}");
            assert!(v.session().reviews.is_empty(), "all or none");
            // A decision that fails keeps no review it attached.
            let decide = json!({"review": "notes.review.txt", "n": 9, "decision": "accept"});
            let out = err(agent(v, cx, "decide_suggestion", decide));
            assert!(out.contains("no suggestion 9"), "{out}");
            assert!(v.session().reviews.is_empty());
            err(agent(v, cx, "edit_paragraph", json!({"paragraph_id": 99, "text": "x"})));
            assert!(!v.take_autosave_for_test(), "a failed call marks nothing changed");
            // And when it works, it is kept.
            let decide = json!({"review": "notes.review.txt", "n": 1, "decision": "accept"});
            ok(agent(v, cx, "decide_suggestion", decide));
            assert_eq!(v.session().reviews.len(), 1);
            assert_eq!(line(v, cx, 2), "I go to town tomorrow.");
            assert!(v.take_autosave_for_test());
        })
        .unwrap();
    }

    /// The version Scriptorium made is written in place by the writer
    /// (Write .fdx); an agent writes it only to an `output`.
    #[gpui::test]
    fn the_made_version_is_written_only_to_an_output(cx: &mut TestAppContext) {
        let f = Folder::new("made");
        let w = open(cx, &f.0.join("film.fdx"));
        w.update(cx, |v, _, cx| {
            ok(agent(v, cx, "new_version", json!({"language": "es", "original_language": "en"})));
        })
        .unwrap();
        let w = open(cx, &f.0.join("film.es.fdx"));
        w.update(cx, |v, _, cx| {
            let out = err(agent(v, cx, "write_script", json!({})));
            assert!(out.contains("Write .fdx") && out.contains("give `output`"), "{out}");
            ok(agent(v, cx, "write_script", json!({"output": "film.es copy.fdx"})));
        })
        .unwrap();
        assert!(f.0.join("film.es copy.fdx").exists());
        assert!(!f.0.join("film.es (scriptorium).fdx").exists());
    }

    /// A review an agent's call made (a new `review_file`) stays attached
    /// when another program writes the session before the app saves it.
    #[gpui::test]
    fn a_new_agent_review_survives_a_reload_before_the_save(cx: &mut TestAppContext) {
        let f = Folder::new("kept");
        let script = f.0.join("film.fdx");
        let w = open(cx, &script);
        w.update(cx, |v, _, cx| {
            let suggest = json!({"paragraph_id": 2, "options": ["I go to town."], "review_file": "notes.review.txt"});
            ok(agent(v, cx, "suggest_change", suggest));
        })
        .unwrap();
        // Another program: its own review, into the session file.
        let mut files = scriptorium_chat::exec::FileScript::new(&script).unwrap();
        let other = json!({"paragraph_id": 1, "options": ["CESAR (V.O.)"], "review_file": "other.review.txt"});
        let done = run_call(&mut files, Caller::Agent, &call("suggest_change", other));
        assert!(!done.result.is_error, "{}", done.result.content);
        w.update(cx, |v, _, cx| {
            v.reload_or_busy(cx).unwrap();
            let mut names: Vec<&str> = v.session().reviews.iter().map(|r| r.path.as_str()).collect();
            names.sort();
            assert_eq!(names, ["notes.review.txt", "other.review.txt"]);
        })
        .unwrap();
    }

    /// An agent decides the suggestion the card shows: the card moves on,
    /// as after a key.
    #[gpui::test]
    fn the_card_moves_on_after_an_agent_decides_it(cx: &mut TestAppContext) {
        let f = Folder::new("card");
        let w = open(cx, &f.0.join("film.fdx"));
        w.update(cx, |v, window, cx| {
            let suggest = json!({"suggestions": [
                {"paragraph_id": 1, "options": ["CESAR (V.O.)"]},
                {"paragraph_id": 2, "options": ["I go to town."]},
            ], "review_file": "notes.review.txt"});
            ok(agent(v, cx, "suggest_changes", suggest));
            let id = v.session().reviews[0].id.clone();
            v.show_suggestion(&id, 1, window, cx);
            assert_eq!(v.card_for_test(), Some((0, 0)));
            ok(agent(v, cx, "decide_suggestion", json!({"n": 1, "decision": "keep"})));
            assert_eq!(v.card_for_test(), Some((0, 1)));
            // A decision on another suggestion leaves the card where it is.
            ok(agent(v, cx, "decide_suggestion", json!({"n": 1, "decision": "undecide"})));
            assert_eq!(v.card_for_test(), Some((0, 1)));
        })
        .unwrap();
    }

    /// The review shown is finished; an agent adds a suggestion to it: the
    /// card goes to the new one, as after a reload (the sidebar says "1
    /// left", the card must not say "Review finished").
    #[gpui::test]
    fn a_finished_review_shows_an_agents_new_suggestion(cx: &mut TestAppContext) {
        let f = Folder::new("finished");
        let w = open(cx, &f.0.join("film.fdx"));
        w.update(cx, |v, window, cx| {
            let suggest = json!({"suggestions": [
                {"paragraph_id": 1, "options": ["CESAR (V.O.)"]},
                {"paragraph_id": 2, "options": ["I go to town."]},
            ], "review_file": "notes.review.txt"});
            ok(agent(v, cx, "suggest_changes", suggest));
            let id = v.session().reviews[0].id.clone();
            v.show_suggestion(&id, 1, window, cx);
            ok(agent(v, cx, "decide_suggestion", json!({"n": 1, "decision": "keep"})));
            ok(agent(v, cx, "decide_suggestion", json!({"n": 2, "decision": "keep"})));
            assert_eq!(v.card_for_test(), None, "finished");
            let more = json!({"paragraph_id": 0, "options": ["INT. BARN - DAY"], "review_file": "notes.review.txt"});
            ok(agent(v, cx, "suggest_change", more));
            assert_eq!(v.card_for_test(), Some((0, 2)));
        })
        .unwrap();
    }
}
