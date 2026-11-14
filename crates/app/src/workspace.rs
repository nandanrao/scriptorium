//! The main window: a sidebar that picks the view (the plain script, or one of
//! the reviews) and lists that view's changes, the script in the middle, and
//! the review card on the right.
//!
//! A review is a view of the script. Decisions go through the editor's
//! `Document` (see `scriptorium_review::Session`), so they change the script,
//! every view shows them, and typing and reviewing share one undo history.
//!
//! Focus decides what keys do. With the card focused (the default in a review),
//! single keys work as in script-review: A accept, R keep mine, E rewrite,
//! J/K next/previous… Clicking the script focuses the editor for typing; Esc
//! comes back to the card.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, App, Context, Div, Entity, FocusHandle, Focusable, FontWeight, HighlightStyle, MouseButton,
    ScrollStrategy, SharedString, Stateful, StyledText, Subscription, Task, UniformListScrollHandle, Window, actions,
    div, prelude::*, px, uniform_list,
};
use scriptorium_core::diff::{WordOp, word_diff};
use scriptorium_core::outline::{ParaContext, annotate};
use scriptorium_core::{Document, ParaId};
use scriptorium_fdx::FdxFile;
use scriptorium_review::sessionfile::{self, Resumed, SessionFile};
use scriptorium_review::{Decision, Review, Session, Status};

use crate::editor::{Decor, EditorEvent, Mark, ScriptEditor};
use crate::layout;
use crate::theme::*;

actions!(
    review,
    [
        Accept,
        Accept1,
        Accept2,
        Accept3,
        KeepMine,
        Rewrite,
        NextChange,
        PrevChange,
        NextOption,
        PrevOption,
        UndoDecision,
        RedoDecision,
        ApplyToFdx,
        SaveAndQuit,
        ShowScript,
        ShowReview1,
        ShowReview2,
        ShowReview3,
        NextView,
        PrevView,
    ]
);

const SCRIPT_FONT: &str = "Courier Prime";

/// Everything read from disk when the app starts.
pub struct Opened {
    pub script_path: PathBuf,
    raw: String,
    fdx: FdxFile,
    session: Session,
    doc: Document,
    session_path: PathBuf,
    notice: String,
}

impl Opened {
    pub fn load(script_path: &Path, review_paths: &[PathBuf]) -> Result<Self, String> {
        let raw = std::fs::read_to_string(script_path).map_err(|e| format!("{}: {e}", script_path.display()))?;
        let fdx = FdxFile::parse(raw.clone()).map_err(|e| format!("{}: {e}", script_path.display()))?;
        let script = fdx.to_script();
        let script_name = file_name(script_path);
        let mut reviews = vec![];
        for p in review_paths {
            let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
            reviews.push(Review::open(review_title(p), &text, &script, |k| fdx.is_caps(k.name()), &script_name));
        }
        let session = Session::new(reviews);
        let session_path = script_path.with_file_name(sessionfile::file_name_for(&script_name));
        let saved = std::fs::read_to_string(&session_path).ok();
        let (doc, how) = sessionfile::resume(&session, script, &raw, saved.as_deref());
        let notice = match how {
            Resumed::Fresh => String::new(),
            Resumed::Resumed { .. } => "Picked up where you left off.".into(),
            Resumed::Discarded(why) => {
                let aside = session_path.with_extension("json.old");
                let _ = std::fs::rename(&session_path, &aside);
                format!("Started fresh: {why}. The old session was kept as {}.", file_name(&aside))
            }
        };
        let mut warnings: Vec<String> =
            session.reviews.iter().flat_map(|r| r.warnings.iter().map(move |w| format!("{}: {w}", r.name))).collect();
        warnings.insert(0, notice);
        warnings.retain(|w| !w.is_empty());
        Ok(Self {
            script_path: script_path.to_path_buf(),
            raw,
            fdx,
            session,
            doc,
            session_path,
            notice: warnings.join("  "),
        })
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

/// "PURPLE LIST.review.txt" → "PURPLE LIST review".
fn review_title(p: &Path) -> String {
    let name = file_name(p);
    let name = name.strip_suffix(".txt").unwrap_or(&name);
    name.replace(".review", " review").replace('.', " ")
}

/// Where Apply writes: "<name> (scriptorium).fdx" next to the script. Never the original.
pub fn output_path(script: &Path) -> PathBuf {
    let stem = script.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    script.with_file_name(format!("{stem} (scriptorium).fdx"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Script,
    Review(usize),
}

struct Rewriting {
    r: usize,
    i: usize,
    /// Undo depth before E: the whole rewrite becomes one undo step, and Esc undoes back to here.
    depth: usize,
}

/// One row of the sidebar's lower list.
#[derive(Clone)]
enum Row {
    Scene(SharedString),
    Item(usize),
    SceneLink(ParaId, SharedString),
}

pub struct Workspace {
    editor: Entity<ScriptEditor>,
    session: Rc<Session>,
    script_path: PathBuf,
    raw: Rc<str>,
    fdx: Rc<FdxFile>,
    session_path: PathBuf,
    originals: Rc<HashMap<ParaId, String>>,
    /// Scene and speaker per paragraph position, as opened.
    ctx: Rc<Vec<ParaContext>>,
    caps: Rc<Vec<bool>>,
    view: View,
    /// Current item per review (None: finished).
    cur: Vec<Option<usize>>,
    /// Highlighted option on the card.
    hi: usize,
    rewriting: Option<Rewriting>,
    focus: FocusHandle,
    rows: Vec<Rc<Vec<Row>>>,
    scene_rows: Rc<Vec<Row>>,
    row_of_item: Vec<Vec<usize>>,
    item_by_para: Vec<HashMap<ParaId, usize>>,
    queue_scroll: UniformListScrollHandle,
    notice: SharedString,
    /// Log length when the .fdx was last written.
    applied_at: Option<usize>,
    autosave: Option<Task<()>>,
    _subscription: Subscription,
}

impl Workspace {
    pub fn new(opened: Opened, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Opened { script_path, raw, fdx, session, doc, session_path, notice } = opened;
        let originals: HashMap<ParaId, String> = fdx.to_script().paragraphs.iter().map(|p| (p.id, p.text())).collect();
        let ctx = annotate(&fdx.to_script());
        let caps: Vec<bool> = fdx.paragraphs().iter().map(|p| fdx.is_caps(&p.kind)).collect();
        let scene_name = |ix: usize| -> SharedString {
            let s = &ctx[ix].scene;
            if s.is_empty() { "Before the first scene".into() } else { s.clone().into() }
        };
        let (mut rows, mut row_of_item, mut item_by_para) = (vec![], vec![], vec![]);
        for review in &session.reviews {
            let (mut rs, mut of) = (vec![], vec![]);
            let mut last_scene: Option<&str> = None;
            for (i, item) in review.items.iter().enumerate() {
                let scene = ctx[item.index].scene.as_str();
                if last_scene != Some(scene) {
                    rs.push(Row::Scene(scene_name(item.index)));
                    last_scene = Some(scene);
                }
                of.push(rs.len());
                rs.push(Row::Item(i));
            }
            rows.push(Rc::new(rs));
            row_of_item.push(of);
            item_by_para.push(review.items.iter().enumerate().map(|(i, it)| (it.para, i)).collect());
        }
        let scene_rows = Rc::new(
            ctx.iter()
                .enumerate()
                .filter(|(_, c)| c.heading)
                .map(|(ix, c)| Row::SceneLink(ParaId(ix as u64), c.scene.clone().into()))
                .collect(),
        );

        let editor = cx.new(|cx| ScriptEditor::new(doc, cx));
        let subscription = cx.subscribe_in(&editor, window, Self::on_editor_event);
        let n = session.reviews.len();
        let mut this = Self {
            editor,
            session: Rc::new(session),
            script_path,
            raw: raw.into(),
            fdx: Rc::new(fdx),
            session_path,
            originals: Rc::new(originals),
            ctx: Rc::new(ctx),
            caps: Rc::new(caps),
            view: View::Script,
            cur: vec![None; n],
            hi: 0,
            rewriting: None,
            focus: cx.focus_handle(),
            rows,
            scene_rows,
            row_of_item,
            item_by_para,
            queue_scroll: UniformListScrollHandle::new(),
            notice: notice.into(),
            applied_at: None,
            autosave: None,
            _subscription: subscription,
        };
        let first = if n > 0 { View::Review(0) } else { View::Script };
        this.set_view(first, window, cx);
        this
    }

    fn doc<'a>(&self, cx: &'a App) -> &'a Document {
        &self.editor.read(cx).doc
    }

    fn review(&self) -> Option<usize> {
        match self.view {
            View::Review(r) => Some(r),
            View::Script => None,
        }
    }

    fn current(&self) -> Option<(usize, usize)> {
        let r = self.review()?;
        Some((r, self.cur[r]?))
    }

    fn text_of(&self, para: ParaId, cx: &App) -> String {
        self.doc(cx).paragraph(para).map(|p| p.text()).unwrap_or_default()
    }

    // ---- view state ----------------------------------------------------------

    /// Push the current line and the margin marks to the editor.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let mut decor = Decor { originals: self.originals.clone(), ..Decor::default() };
        if let Some(r) = self.review() {
            let doc = self.doc(cx);
            for (i, item) in self.session.reviews[r].items.iter().enumerate() {
                decor.marks.insert(item.para, mark_of(&self.session, doc, r, i));
            }
            decor.current = self.cur[r].map(|i| self.session.item(r, i).para);
        }
        self.editor.update(cx, |e, cx| {
            e.decor = decor;
            cx.notify();
        });
        cx.notify();
    }

    fn set_view(&mut self, view: View, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_rewrite_quietly(cx);
        self.view = view;
        self.hi = 0;
        match view {
            View::Review(r) => {
                if self.cur[r].is_none() {
                    let doc = self.doc(cx);
                    self.cur[r] = self.session.next_pending(doc, r, None);
                }
                if let Some(i) = self.cur[r] {
                    self.show_item(r, i, cx);
                }
                window.focus(&self.focus, cx);
            }
            View::Script => {
                let focus = self.editor.read(cx).focus_handle().clone();
                window.focus(&focus, cx);
            }
        }
        self.refresh(cx);
    }

    /// Scroll the script and the queue to item `i`.
    fn show_item(&mut self, r: usize, i: usize, cx: &mut Context<Self>) {
        let para = self.session.item(r, i).para;
        self.editor.update(cx, |e, cx| e.reveal(para, cx));
        self.queue_scroll.scroll_to_item(self.row_of_item[r][i], ScrollStrategy::Center);
    }

    fn go_to(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.review() else { return };
        if self.rewriting.as_ref().is_some_and(|rw| rw.i != i) {
            self.commit_rewrite_quietly(cx);
        }
        self.cur[r] = Some(i);
        self.hi = 0;
        self.show_item(r, i, cx);
        if self.rewriting.is_none() {
            window.focus(&self.focus, cx);
        }
        self.refresh(cx);
    }

    /// After deciding item `i`: on to the next pending one (or finished).
    fn advance(&mut self, r: usize, i: usize, cx: &mut Context<Self>) {
        let next = self.session.next_pending(self.doc(cx), r, Some(i));
        self.cur[r] = next;
        self.hi = 0;
        if let Some(n) = next {
            self.show_item(r, n, cx);
        }
        self.refresh(cx);
    }

    fn decide(&mut self, decision: Decision, window: &mut Window, cx: &mut Context<Self>) {
        let Some((r, i)) = self.current() else { return };
        self.notice = "".into();
        self.rewriting = None;
        let session = self.session.clone();
        self.editor.update(cx, |e, cx| e.run(cx, |doc, sel| session.decide(doc, sel, r, i, decision)));
        window.focus(&self.focus, cx);
        self.advance(r, i, cx);
    }

    fn accept(&mut self, k: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((r, i)) = self.current() else { return };
        if k < self.session.item(r, i).options.len() {
            self.decide(Decision::Accept(k), window, cx);
        }
    }

    // ---- rewriting in place ----------------------------------------------------

    /// E: put the highlighted option into the line and edit it in the script.
    fn start_rewrite(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((r, i)) = self.current() else { return };
        let item = self.session.item(r, i).clone();
        let original = self.text_of(item.para, cx);
        let start = if self.session.status(self.doc(cx), r, i) == Status::Rewritten {
            original.clone()
        } else {
            item.options.get(self.hi).cloned().unwrap_or_else(|| original.clone())
        };
        let para = item.para;
        let depth = self.doc(cx).undo_depth();
        self.editor.update(cx, |e, cx| {
            e.run(cx, |doc, sel| doc.replace_text(sel, para, &start, &[]));
            e.cursor_to_end(para, cx);
        });
        self.rewriting = Some(Rewriting { r, i, depth });
        self.notice = "".into();
        let focus = self.editor.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        self.refresh(cx);
    }

    /// Record what the line now says (an option, the original, or a rewrite).
    fn record_hand_edit(&mut self, r: usize, i: usize, cx: &mut Context<Self>) {
        let session = self.session.clone();
        self.editor.update(cx, |e, cx| e.run(cx, |doc, sel| session.decide_from_text(doc, sel, r, i)));
    }

    /// Record the rewrite's result, as one undo step with everything since E.
    fn finish_rewrite(&mut self, rw: &Rewriting, cx: &mut Context<Self>) {
        self.record_hand_edit(rw.r, rw.i, cx);
        let depth = rw.depth;
        self.editor.update(cx, |e, _| e.doc.merge_undo_since(depth));
        self.schedule_autosave(cx);
    }

    fn commit_rewrite(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rw) = self.rewriting.take() else { return };
        self.finish_rewrite(&rw, cx);
        window.focus(&self.focus, cx);
        self.advance(rw.r, rw.i, cx);
    }

    /// Finish a rewrite without moving on (switching view or item).
    fn commit_rewrite_quietly(&mut self, cx: &mut Context<Self>) {
        if let Some(rw) = self.rewriting.take() {
            self.finish_rewrite(&rw, cx);
        }
    }

    fn cancel_rewrite(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rw) = self.rewriting.take() else { return };
        self.editor.update(cx, |e, cx| {
            e.run(cx, |doc, sel| {
                let mut sel = sel;
                while doc.undo_depth() > rw.depth {
                    match doc.undo() {
                        Some(s) => sel = s,
                        None => break,
                    }
                }
                Ok(sel)
            })
        });
        window.focus(&self.focus, cx);
        self.refresh(cx);
    }

    // ---- editor events -------------------------------------------------------

    fn on_editor_event(
        &mut self,
        _: &Entity<ScriptEditor>,
        event: &EditorEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            EditorEvent::Edited => {
                self.schedule_autosave(cx);
                self.refresh(cx);
            }
            EditorEvent::Clicked(para) => {
                if let Some(r) = self.review()
                    && let Some(&i) = self.item_by_para[r].get(para)
                {
                    if self.rewriting.as_ref().is_some_and(|rw| rw.i != i) {
                        self.commit_rewrite_quietly(cx);
                    }
                    self.cur[r] = Some(i);
                    self.hi = 0;
                    self.queue_scroll.scroll_to_item(self.row_of_item[r][i], ScrollStrategy::Center);
                    self.refresh(cx);
                }
            }
            EditorEvent::Escape => {
                if self.rewriting.is_some() {
                    self.cancel_rewrite(window, cx);
                } else if self.review().is_some() {
                    window.focus(&self.focus, cx);
                    cx.notify();
                }
            }
            EditorEvent::Commit => {
                if self.rewriting.is_some() {
                    self.commit_rewrite(window, cx);
                } else if let Some((r, i)) = self.current()
                    && self.editor.read(cx).sel.head.para == self.session.item(r, i).para
                {
                    // Edited the line under review by hand, then Enter: that's a decision.
                    self.record_hand_edit(r, i, cx);
                    window.focus(&self.focus, cx);
                    self.advance(r, i, cx);
                } else {
                    self.notice = "Enter doesn't add paragraphs yet. Esc goes back to the review.".into();
                    cx.notify();
                }
            }
            EditorEvent::Notice(text) => {
                self.notice = text.clone().into();
                cx.notify();
            }
        }
    }

    // ---- saving ----------------------------------------------------------------

    fn schedule_autosave(&mut self, cx: &mut Context<Self>) {
        self.autosave = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(600)).await;
            this.update(cx, |w, cx| w.save_session(cx)).ok();
        }));
    }

    /// Write the event log next to the script, so the session can be resumed.
    fn save_session(&mut self, cx: &mut Context<Self>) {
        let json = SessionFile::new(&self.raw, self.doc(cx).log()).to_json();
        if let Err(e) = std::fs::write(&self.session_path, json) {
            self.notice = format!("Couldn't save the session: {e}").into();
            cx.notify();
        }
    }

    fn apply_to_fdx(&mut self, _: &ApplyToFdx, _: &mut Window, cx: &mut Context<Self>) {
        self.commit_rewrite_quietly(cx);
        let out = output_path(&self.script_path);
        let doc = self.doc(cx);
        let len = doc.log().len();
        self.notice = match self.fdx.write_script(doc.script()) {
            Ok(raw) => match std::fs::write(&out, raw) {
                Ok(()) => {
                    self.applied_at = Some(len);
                    format!("Wrote {}. The original is untouched.", file_name(&out)).into()
                }
                Err(e) => format!("Couldn't write {}: {e}", out.display()).into(),
            },
            Err(e) => format!("Not written: {e}").into(),
        };
        self.save_session(cx);
        cx.notify();
    }

    fn save_and_quit(&mut self, _: &SaveAndQuit, _: &mut Window, cx: &mut Context<Self>) {
        self.commit_rewrite_quietly(cx);
        self.save_session(cx);
        cx.quit();
    }

    // ---- actions -------------------------------------------------------------

    fn on_accept(&mut self, _: &Accept, window: &mut Window, cx: &mut Context<Self>) {
        self.accept(self.hi, window, cx);
    }
    fn on_accept1(&mut self, _: &Accept1, window: &mut Window, cx: &mut Context<Self>) {
        self.accept(0, window, cx);
    }
    fn on_accept2(&mut self, _: &Accept2, window: &mut Window, cx: &mut Context<Self>) {
        self.accept(1, window, cx);
    }
    fn on_accept3(&mut self, _: &Accept3, window: &mut Window, cx: &mut Context<Self>) {
        self.accept(2, window, cx);
    }
    fn on_keep(&mut self, _: &KeepMine, window: &mut Window, cx: &mut Context<Self>) {
        self.decide(Decision::KeepMine, window, cx);
    }
    fn on_rewrite(&mut self, _: &Rewrite, window: &mut Window, cx: &mut Context<Self>) {
        self.start_rewrite(window, cx);
    }
    fn step(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.review() else { return };
        let n = self.session.reviews[r].items.len();
        if n == 0 {
            return;
        }
        self.notice = "".into();
        let i = match (self.cur[r], forward) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(i), true) => (i + 1).min(n - 1),
            (Some(i), false) => i.saturating_sub(1),
        };
        self.go_to(i, window, cx);
    }
    fn on_next(&mut self, _: &NextChange, window: &mut Window, cx: &mut Context<Self>) {
        self.step(true, window, cx);
    }
    fn on_prev(&mut self, _: &PrevChange, window: &mut Window, cx: &mut Context<Self>) {
        self.step(false, window, cx);
    }
    fn cycle_option(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some((r, i)) = self.current() else { return };
        let n = self.session.item(r, i).options.len();
        self.hi = if forward { (self.hi + 1) % n } else { (self.hi + n - 1) % n };
        cx.notify();
    }
    fn on_next_option(&mut self, _: &NextOption, _: &mut Window, cx: &mut Context<Self>) {
        self.cycle_option(true, cx);
    }
    fn on_prev_option(&mut self, _: &PrevOption, _: &mut Window, cx: &mut Context<Self>) {
        self.cycle_option(false, cx);
    }
    fn undo_redo(&mut self, redo: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.rewriting = None;
        self.notice = "".into();
        // The suggestion the undo will touch: by its decision, else by its paragraph.
        let target = self.review().and_then(|r| {
            let doc = self.doc(cx);
            let keys = doc.next_undo_meta_keys(redo);
            let by_key =
                (0..self.session.reviews[r].items.len()).find(|&i| keys.contains(&self.session.key(r, i).as_str()));
            by_key.or_else(|| doc.next_undo_paras(redo).iter().find_map(|p| self.item_by_para[r].get(p).copied()))
        });
        let mut did = false;
        self.editor.update(cx, |e, cx| {
            e.run(cx, |doc, sel| {
                let r = if redo { doc.redo() } else { doc.undo() };
                did = r.is_some();
                Ok(r.unwrap_or(sel))
            })
        });
        if !did {
            self.notice = if redo { "Nothing to redo." } else { "Nothing to undo." }.into();
        }
        // Go to the suggestion the undo touched, as script-review does.
        if let (Some(r), Some(i)) = (self.review(), target) {
            self.cur[r] = Some(i);
            self.hi = 0;
            self.show_item(r, i, cx);
        }
        window.focus(&self.focus, cx);
        self.refresh(cx);
    }
    fn on_undo(&mut self, _: &UndoDecision, window: &mut Window, cx: &mut Context<Self>) {
        self.undo_redo(false, window, cx);
    }
    fn on_redo(&mut self, _: &RedoDecision, window: &mut Window, cx: &mut Context<Self>) {
        self.undo_redo(true, window, cx);
    }

    fn views(&self) -> Vec<View> {
        std::iter::once(View::Script).chain((0..self.session.reviews.len()).map(View::Review)).collect()
    }
    fn show_nth(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(v) = self.views().get(n).copied() {
            self.set_view(v, window, cx);
        }
    }
    fn cycle_view(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let views = self.views();
        let at = views.iter().position(|v| *v == self.view).unwrap_or(0);
        let n = views.len();
        self.set_view(views[if forward { (at + 1) % n } else { (at + n - 1) % n }], window, cx);
    }
    fn on_show_script(&mut self, _: &ShowScript, window: &mut Window, cx: &mut Context<Self>) {
        self.show_nth(0, window, cx);
    }
    fn on_show_review1(&mut self, _: &ShowReview1, window: &mut Window, cx: &mut Context<Self>) {
        self.show_nth(1, window, cx);
    }
    fn on_show_review2(&mut self, _: &ShowReview2, window: &mut Window, cx: &mut Context<Self>) {
        self.show_nth(2, window, cx);
    }
    fn on_show_review3(&mut self, _: &ShowReview3, window: &mut Window, cx: &mut Context<Self>) {
        self.show_nth(3, window, cx);
    }
    fn on_next_view(&mut self, _: &NextView, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_view(true, window, cx);
    }
    fn on_prev_view(&mut self, _: &PrevView, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_view(false, window, cx);
    }

    fn click_row(&mut self, row: Row, window: &mut Window, cx: &mut Context<Self>) {
        match row {
            Row::Item(i) => self.go_to(i, window, cx),
            Row::SceneLink(para, _) => self.editor.update(cx, |e, cx| {
                e.cursor_to_end(para, cx);
                e.reveal(para, cx);
            }),
            Row::Scene(_) => {}
        }
    }

    // ---- rendering ----------------------------------------------------------

    fn changed_lines(&self, cx: &App) -> usize {
        self.doc(cx).paragraphs().iter().filter(|p| self.originals.get(&p.id).is_some_and(|o| *o != p.text())).count()
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> Div {
        let changed = self.changed_lines(cx);
        let written = self.applied_at == Some(self.doc(cx).log().len());
        let state = match (changed, written) {
            (0, _) => "Same as the original".to_string(),
            (n, true) => format!("{n} lines changed · written to {}", file_name(&output_path(&self.script_path))),
            (n, false) => format!("{n} lines changed · not yet written"),
        };
        div()
            .flex_none()
            .h(px(38.))
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .bg(CHROME)
            .border_b_1()
            .border_color(RULE)
            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(INK).child(file_name(&self.script_path)))
            .child(div().flex_1())
            .child(div().text_color(INK_2).child(state))
            .child(
                button("apply", "Write .fdx", Some("Ctrl+S"), true)
                    .on_click(cx.listener(|this, _, window, cx| this.apply_to_fdx(&ApplyToFdx, window, cx))),
            )
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let doc = self.doc(cx);
        let changed = self.changed_lines(cx);
        let mut views = vec![section_label("Script")];
        views.push(
            view_row(
                "view-script",
                "Script",
                if changed > 0 { format!("{changed} changed") } else { String::new() },
                self.view == View::Script,
            )
            .on_click(cx.listener(|this, _, window, cx| this.set_view(View::Script, window, cx)))
            .into_any_element(),
        );
        views.push(div().h(px(8.)).into_any_element());
        views.push(section_label("Reviews"));
        for (r, review) in self.session.reviews.iter().enumerate() {
            let p = self.session.progress(doc, r);
            let count = if p.total == 0 {
                "nothing to review".into()
            } else if p.pending == 0 {
                "done".into()
            } else {
                format!("{} left", p.pending)
            };
            views.push(
                view_row(("view-review", r), &review.name, count, self.view == View::Review(r))
                    .on_click(cx.listener(move |this, _, window, cx| this.set_view(View::Review(r), window, cx)))
                    .into_any_element(),
            );
        }
        if self.session.reviews.is_empty() {
            views.push(
                div()
                    .px_2()
                    .text_color(INK_3)
                    .text_size(px(12.))
                    .child("None open. Pass review files after the script: scriptorium-app SCRIPT.fdx REVIEW.txt")
                    .into_any_element(),
            );
        }

        // The lower list: the selected review's changes, or the script's scenes.
        let (title, rows): (String, Rc<Vec<Row>>) = match self.view {
            View::Review(r) => (format!("Changes · {}", self.session.reviews[r].name), self.rows[r].clone()),
            View::Script => ("Scenes".into(), self.scene_rows.clone()),
        };
        let marks: Rc<Vec<(Mark, SharedString)>> = Rc::new(match self.view {
            View::Review(r) => self.session.reviews[r]
                .items
                .iter()
                .enumerate()
                .map(|(i, it)| (mark_of(&self.session, doc, r, i), self.text_of(it.para, cx).into()))
                .collect(),
            View::Script => vec![],
        });
        let current = self.current().map(|(_, i)| i);
        let ws = cx.entity().downgrade();
        let list = uniform_list("queue", rows.len(), move |range, _, _| {
            range
                .map(|ix| {
                    let row = rows[ix].clone();
                    let base = div().id(("row", ix)).h(px(24.)).px_2().flex().items_center().gap_2().rounded_sm();
                    let el = match &row {
                        Row::Scene(name) => base
                            .pt_1()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(INK_3)
                            .child(div().truncate().child(name.clone())),
                        Row::Item(i) => {
                            let (mark, text) = &marks[*i];
                            base.cursor_pointer()
                                .when(current == Some(*i), |d| d.bg(PAPER).border_1().border_color(ACCENT))
                                .hover(|s| s.bg(ACCENT_SOFT))
                                .child(mark_dot(*mark))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(SCRIPT_FONT)
                                        .text_size(px(12.))
                                        .text_color(if *mark == Mark::Pending || *mark == Mark::Outdated {
                                            INK
                                        } else {
                                            INK_3
                                        })
                                        .child(text.clone()),
                                )
                        }
                        Row::SceneLink(_, name) => base
                            .cursor_pointer()
                            .hover(|s| s.bg(ACCENT_SOFT))
                            .font_family(SCRIPT_FONT)
                            .text_size(px(12.))
                            .text_color(INK)
                            .child(div().truncate().child(name.clone())),
                    };
                    let ws = ws.clone();
                    el.on_click(move |_, window, cx| {
                        let row = row.clone();
                        ws.update(cx, |w, cx| w.click_row(row, window, cx)).ok();
                    })
                })
                .collect()
        })
        .track_scroll(&self.queue_scroll)
        .flex_1();

        div()
            .flex_none()
            .w(px(270.))
            .h_full()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .bg(SIDE)
            .border_r_1()
            .border_color(RULE)
            .children(views)
            .child(div().h(px(10.)))
            .child(section_label(&title))
            .child(list)
    }

    fn render_card(&self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let focused = self.focus.is_focused(window);
        let mut body: Vec<AnyElement> = vec![];
        let shell = div()
            .id("card")
            .track_focus(&self.focus)
            .key_context("Review")
            .flex_none()
            .w(px(380.))
            .h_full()
            .overflow_y_scroll()
            .bg(PAPER)
            .border_l_1()
            .border_color(if focused { ACCENT } else { RULE })
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus, cx);
                    cx.notify();
                }),
            );
        let Some(r) = self.review() else { return shell };
        let Some(i) = self.cur[r] else {
            let p = self.session.progress(self.doc(cx), r);
            body.push(label("Review finished"));
            body.push(
                div()
                    .text_size(px(16.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if p.total == 0 {
                        "Nothing to review here".to_string()
                    } else {
                        format!("All {} suggestions decided", p.total)
                    })
                    .into_any_element(),
            );
            body.push(
                small(format!(
                    "{} accepted · {} kept · {} rewritten. Ctrl+S writes the script.",
                    p.accepted, p.rejected, p.rewritten
                ))
                .into_any_element(),
            );
            body.push(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button("finish-apply", "Write .fdx", Some("Ctrl+S"), true)
                            .on_click(cx.listener(|this, _, window, cx| this.apply_to_fdx(&ApplyToFdx, window, cx))),
                    )
                    .child(
                        button("finish-undo", "Undo last", Some("U"), false)
                            .on_click(cx.listener(|this, _, window, cx| this.undo_redo(false, window, cx))),
                    )
                    .into_any_element(),
            );
            return shell.children(body);
        };

        let doc = self.doc(cx);
        let item = self.session.item(r, i);
        let ctx = &self.ctx[item.index];
        let caps = self.caps.get(item.index).copied().unwrap_or(false);
        let kind = doc.paragraph(item.para).map(|p| p.kind.name().to_lowercase()).unwrap_or_default();
        let who = if ctx.heading {
            "Scene heading".to_string()
        } else if ctx.speaker.is_empty() {
            capitalize(&kind)
        } else {
            format!("{} · {kind}", ctx.speaker)
        };
        let status = self.session.status(doc, r, i);
        let now = self.text_of(item.para, cx);
        let total = self.session.reviews[r].items.len();

        body.push(
            div()
                .flex()
                .flex_col()
                .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(INK).child(who))
                .child(small(format!(
                    "{} · change {} of {total}",
                    if ctx.scene.is_empty() { "Before the first scene" } else { &ctx.scene },
                    i + 1
                )))
                .into_any_element(),
        );
        if item.fuzzy {
            body.push(warn_box("Matched approximately: check this is the right line.", None));
        }
        let rewriting_here = self.rewriting.as_ref().is_some_and(|rw| rw.r == r && rw.i == i);
        if self.session.is_outdated(doc, r, i) && !rewriting_here {
            body.push(warn_box("This line has changed since the suggestion was written.", Some(item.before.clone())));
        }
        if rewriting_here {
            body.push(label("Rewriting in the script"));
            body.push(
                small("Edit the highlighted line. Enter saves and moves on; Esc puts it back.".to_string())
                    .into_any_element(),
            );
            return shell.children(body);
        }

        let chip = match status {
            Status::Pending => None,
            Status::Accepted(k) => {
                Some(if item.options.len() > 1 { format!("accepted option {}", k + 1) } else { "accepted".into() })
            }
            Status::Rejected => Some("kept yours".into()),
            Status::Rewritten => Some("rewritten".into()),
        };
        body.push(label_with("Yours now", chip));
        let hi = self.hi.min(item.options.len() - 1);
        let mine = if status == Status::Pending {
            diff_text(&now, &item.options[hi], WordOp::Del, caps)
        } else {
            plain_text(&now, caps)
        };
        body.push(
            div().border_l_2().border_color(RULE).pl_2().child(script_text(mine).text_color(INK_2)).into_any_element(),
        );

        let multi = item.options.len() > 1;
        body.push(label(if multi { format!("{} options", item.options.len()) } else { "Suggested".into() }));
        for (k, o) in item.options.iter().enumerate() {
            let is_hi = k == hi;
            body.push(
                div()
                    .id(("option", k))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(if is_hi { ACCENT } else { RULE })
                    .when(is_hi, |d| d.bg(ACCENT_SOFT))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.hi = k;
                        window.focus(&this.focus, cx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(multi, |d| d.child(small(format!("{}", k + 1)).font_weight(FontWeight::BOLD)))
                            .when(status == Status::Accepted(k), |d| d.child(chip_el("chosen", OK, OK_SOFT)))
                            .child(div().flex_1())
                            .child(
                                button(
                                    ("accept", k),
                                    "Accept",
                                    Some(if multi { ["1", "2", "3"].get(k).copied().unwrap_or("") } else { "A" }),
                                    is_hi,
                                )
                                .on_click(cx.listener(move |this, _, window, cx| this.accept(k, window, cx))),
                            ),
                    )
                    .child(script_text(diff_text(&now, o, WordOp::Ins, caps)).text_color(INK))
                    .into_any_element(),
            );
        }
        if !item.why.is_empty() {
            body.push(
                div()
                    .text_size(px(12.5))
                    .text_color(INK_2)
                    .child(StyledText::new(format!("Why: {}", item.why)).with_highlights([(
                        0..4,
                        HighlightStyle {
                            font_weight: Some(FontWeight::SEMIBOLD),
                            color: Some(INK.into()),
                            ..Default::default()
                        },
                    )]))
                    .into_any_element(),
            );
        }
        body.push(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(
                    button("b-accept", "Accept", Some("A"), true)
                        .on_click(cx.listener(|this, _, w, cx| this.accept(this.hi, w, cx))),
                )
                .child(
                    button("b-keep", "Keep mine", Some("R"), false)
                        .on_click(cx.listener(|this, _, w, cx| this.decide(Decision::KeepMine, w, cx))),
                )
                .child(
                    button("b-rewrite", "Rewrite", Some("E"), false)
                        .on_click(cx.listener(|this, _, w, cx| this.start_rewrite(w, cx))),
                )
                .child(
                    button("b-skip", "Skip", Some("J"), false)
                        .on_click(cx.listener(|this, _, w, cx| this.step(true, w, cx))),
                )
                .into_any_element(),
        );
        if status != Status::Pending {
            body.push(
                button("b-undecide", "Back to undecided", None, false)
                    .on_click(cx.listener(|this, _, w, cx| {
                        let Some((r, i)) = this.current() else { return };
                        let session = this.session.clone();
                        this.editor.update(cx, |e, cx| {
                            e.run(cx, |doc, sel| session.decide(doc, sel, r, i, Decision::Undecided))
                        });
                        w.focus(&this.focus, cx);
                        this.refresh(cx);
                    }))
                    .into_any_element(),
            );
        }
        if !focused {
            body.push(
                small("Typing goes to the script. Esc or click here for review keys.".to_string()).into_any_element(),
            );
        }
        shell.children(body)
    }

    fn render_status(&self, cx: &mut Context<Self>) -> Div {
        let mut left = div().flex().items_center().gap_3();
        let hints = match self.view {
            View::Review(r) => {
                let p = self.session.progress(self.doc(cx), r);
                let pct = if p.total == 0 { 1.0 } else { p.done() as f32 / p.total as f32 };
                left = left
                    .child(format!("{} · {} of {} decided", self.session.reviews[r].name, p.done(), p.total))
                    .child(
                        div()
                            .w(px(120.))
                            .h(px(4.))
                            .rounded_sm()
                            .bg(RULE)
                            .child(div().h_full().rounded_sm().bg(ACCENT).w(px(120. * pct))),
                    );
                "A accept · 1 2 3 option · Tab next option · R keep mine · E rewrite · J/K next/prev · U undo"
            }
            View::Script => {
                left = left.child(format!("Script · {} paragraphs", self.doc(cx).paragraphs().len()));
                "Edit text inside lines · Ctrl+Z undo · Ctrl+S write .fdx"
            }
        };
        let right: SharedString = if self.notice.is_empty() { hints.into() } else { self.notice.clone() };
        div()
            .flex_none()
            .h(px(28.))
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .bg(CHROME)
            .border_t_1()
            .border_color(RULE)
            .text_size(px(12.))
            .text_color(INK_2)
            .child(left)
            .child(div().flex_1())
            .child(
                div()
                    .when(!self.notice.is_empty(), |d| d.text_color(INK).font_weight(FontWeight::SEMIBOLD))
                    .child(right),
            )
    }
}

fn mark_of(session: &Session, doc: &Document, r: usize, i: usize) -> Mark {
    match session.status(doc, r, i) {
        Status::Pending if session.is_outdated(doc, r, i) => Mark::Outdated,
        Status::Pending => Mark::Pending,
        Status::Accepted(_) => Mark::Accepted,
        Status::Rejected => Mark::Kept,
        Status::Rewritten => Mark::Rewritten,
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// The text of one side of a word diff, with the changed words tinted:
/// `keep = Del` shows `a` with removed words marked, `Ins` shows `b` with added ones.
fn diff_text(a: &str, b: &str, keep: WordOp, caps: bool) -> StyledText {
    let mut text = String::new();
    let mut marks = vec![];
    for c in word_diff(a, b) {
        if c.op == WordOp::Eq {
            text.push_str(&c.text);
        } else if c.op == keep {
            let start = text.len();
            text.push_str(&c.text);
            marks.push(start..text.len());
        }
    }
    let bg = if keep == WordOp::Ins { INS } else { DEL };
    let text = if caps { layout::display_caps(&text) } else { text };
    StyledText::new(text).with_highlights(
        marks.into_iter().map(|r| (r, HighlightStyle { background_color: Some(bg.into()), ..Default::default() })),
    )
}

fn plain_text(s: &str, caps: bool) -> StyledText {
    StyledText::new(if caps { layout::display_caps(s) } else { s.to_string() })
}

fn script_text(t: StyledText) -> Div {
    div().font_family(SCRIPT_FONT).text_size(px(14.)).line_height(px(20.)).child(t)
}

fn label(text: impl Into<SharedString>) -> AnyElement {
    label_with(text, None)
}

fn label_with(text: impl Into<SharedString>, chip: Option<String>) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(INK_3)
        .child(text.into().to_uppercase())
        .when_some(chip, |d, c| d.child(chip_el(c, OK, OK_SOFT)))
        .into_any_element()
}

fn section_label(text: &str) -> AnyElement {
    div()
        .px_2()
        .pb_1()
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(INK_3)
        .child(text.to_uppercase())
        .into_any_element()
}

fn small(text: impl Into<SharedString>) -> Div {
    div().text_size(px(12.)).text_color(INK_2).child(text.into())
}

fn chip_el(text: impl Into<SharedString>, fg: gpui::Rgba, bg: gpui::Rgba) -> Div {
    div()
        .px_1p5()
        .rounded_sm()
        .bg(bg)
        .text_color(fg)
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

fn warn_box(text: &str, was: Option<String>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded_md()
        .bg(WARN_SOFT)
        .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(WARN).child(text.to_string()))
        .when_some(was, |d, was| {
            d.child(small("It was written for:".to_string()))
                .child(script_text(StyledText::new(was)).text_color(INK_2).line_through())
        })
        .into_any_element()
}

fn button(id: impl Into<gpui::ElementId>, text: &str, key: Option<&str>, primary: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_1p5()
        .px_2()
        .py_0p5()
        .rounded_md()
        .border_1()
        .cursor_pointer()
        .text_size(px(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .map(|d| {
            if primary {
                d.bg(OK).border_color(OK).text_color(PAPER)
            } else {
                d.bg(PAPER).border_color(RULE).text_color(INK)
            }
        })
        .hover(|s| s.border_color(INK_3))
        .child(text.to_string())
        .when_some(key.filter(|k| !k.is_empty()), |d, k| {
            d.child(
                div()
                    .px_1()
                    .rounded_sm()
                    .border_1()
                    .border_color(if primary { PAPER } else { RULE })
                    .text_size(px(10.))
                    .child(k.to_string()),
            )
        })
}

fn view_row(id: impl Into<gpui::ElementId>, name: &str, count: String, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .hover(|s| s.bg(ACCENT_SOFT))
        .when(on, |d| d.bg(ACCENT_SOFT).text_color(ACCENT).font_weight(FontWeight::SEMIBOLD))
        .child(div().flex_1().min_w_0().truncate().child(name.to_string()))
        .child(div().flex_none().text_size(px(11.)).text_color(INK_3).child(count))
}

fn mark_dot(mark: Mark) -> Div {
    let (bg, border, round) = match mark {
        Mark::Pending => (ACCENT, ACCENT, true),
        Mark::Outdated => (PAPER, WARN, true),
        Mark::Accepted => (OK, OK, true),
        Mark::Kept => (PAPER, INK_3, true),
        Mark::Rewritten => (OWN, OWN, false),
    };
    div()
        .flex_none()
        .size(px(9.))
        .border_1()
        .border_color(border)
        .bg(bg)
        .map(|d| if round { d.rounded_full() } else { d.rounded_sm() })
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let card = self.review().map(|_| self.render_card(window, cx));
        div()
            .key_context("Workspace")
            .size_full()
            .flex()
            .flex_col()
            .bg(CHROME)
            .text_color(INK)
            .text_size(px(13.))
            .on_action(cx.listener(Self::apply_to_fdx))
            .on_action(cx.listener(Self::save_and_quit))
            .on_action(cx.listener(Self::on_accept))
            .on_action(cx.listener(Self::on_accept1))
            .on_action(cx.listener(Self::on_accept2))
            .on_action(cx.listener(Self::on_accept3))
            .on_action(cx.listener(Self::on_keep))
            .on_action(cx.listener(Self::on_rewrite))
            .on_action(cx.listener(Self::on_next))
            .on_action(cx.listener(Self::on_prev))
            .on_action(cx.listener(Self::on_next_option))
            .on_action(cx.listener(Self::on_prev_option))
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_redo))
            .on_action(cx.listener(Self::on_show_script))
            .on_action(cx.listener(Self::on_show_review1))
            .on_action(cx.listener(Self::on_show_review2))
            .on_action(cx.listener(Self::on_show_review3))
            .on_action(cx.listener(Self::on_next_view))
            .on_action(cx.listener(Self::on_prev_view))
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_sidebar(cx))
                    .child(div().flex_1().min_w_0().h_full().child(self.editor.clone()))
                    .children(card),
            )
            .child(self.render_status(cx))
    }
}
