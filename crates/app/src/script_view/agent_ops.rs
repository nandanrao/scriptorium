//! The host operations an outside agent runs on the open script (plan
//! "agent connect", C2): decide a suggestion, edit a line, undo or redo,
//! write the script, attach and detach reviews. The app's `LiveStore` calls
//! these inside the one update a tool call runs in, holding the session
//! lease; none takes a `Window` or moves the writer's focus.
//!
//! Each gives the text `scriptorium_fsio::ops` gives on the files (the same
//! cores: `decide_in`, `edit_in`, `undo_redo_in`, `write_doc`,
//! `attach_text`), so every host answers alike. The session is saved by the
//! caller before the agent is answered (`ScriptView::save_for_agent`).
//!
//! While the writer is rewriting a line, nothing here changes the script:
//! the session can't be saved until the rewrite ends (a save then waits),
//! and the agent must never be told of a change that isn't on disk. Each is
//! busy then ([`LiveError::Busy`], tried again), but `edit_paragraph` on the
//! line being rewritten itself ([`EDITING_LINE`]). The agent link answers
//! busy before such a call is taken; these are for any other caller.
//!
//! A call that fails changes nothing, as on the files (where nothing is
//! saved): a review `decide_suggestion` or `attach_reviews` attached is
//! taken off again, and the view isn't marked changed.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use scriptorium_chat::exec::EDITING_LINE;
use scriptorium_core::ParaId;
use scriptorium_fsio::Folders;
use scriptorium_fsio::opened::{agent_review, attached_names, find_attached};
use scriptorium_fsio::ops::{self, Choice, SuggestionRef};
use scriptorium_review::Status;
use scriptorium_review::sessionfile;

use super::{LiveError, ScriptView, View, anchor_para, file_name};
use gpui::{App, Context};
use scriptorium_tools::context::{self, Card, PairPlace, Showing, ViewContext};

/// An outside agent's call that was still busy because the writer is
/// rewriting a line (any line but the one being rewritten: that one is
/// [`EDITING_LINE`]), when the agent link gives up trying it again.
pub(crate) const WRITER_REWRITING: &str = "The writer is rewriting a line in Scriptorium; try again shortly.";

fn failed(m: impl Into<String>) -> LiveError {
    LiveError::Failed(m.into())
}

/// The view as it was before a call that may attach reviews, to put back if
/// the call fails.
struct Before {
    session: Rc<scriptorium_review::Session>,
    cur: Vec<Option<usize>>,
    since: Vec<PathBuf>,
    view: View,
    hi: usize,
    doc: scriptorium_core::Document,
}

impl ScriptView {
    /// A review an agent's call added and has just written (a new
    /// `review_file`): kept attached by a reload before the session is saved,
    /// as a review File › Add review attached is.
    pub(crate) fn keep_attached(&mut self, at: PathBuf) {
        let at = at.canonicalize().unwrap_or(at);
        if !self.attached_since_sync.contains(&at) {
            self.attached_since_sync.push(at);
        }
    }

    fn busy_while_rewriting(&self) -> Result<(), LiveError> {
        match self.rewriting {
            Some(_) => Err(LiveError::Busy),
            None => Ok(()),
        }
    }

    fn before(&self, cx: &gpui::App) -> Before {
        Before {
            session: self.session.clone(),
            cur: self.cur.clone(),
            since: self.attached_since_sync.clone(),
            view: self.view,
            hi: self.hi,
            doc: self.doc(cx).clone(),
        }
    }

    /// Put the view back as it was `before` (a failed call's attaches,
    /// and the decisions saved in their files that attaching applied).
    fn put_back(&mut self, before: Before, cx: &mut Context<Self>) {
        if self.doc(cx).log().len() != before.doc.log().len() {
            let doc = before.doc;
            self.editor.update(cx, |e, cx| {
                e.run(cx, |d, sel| {
                    *d = doc;
                    Ok(d.clamp_selection(sel))
                })
            });
        }
        self.session = before.session;
        self.cur = before.cur;
        self.attached_since_sync = before.since;
        self.view = before.view;
        self.hi = before.hi;
        self.build_rows(cx);
        cx.notify();
    }

    /// The script as the agent is told about it (in hints).
    fn script_arg(&self) -> String {
        self.script_path.display().to_string()
    }

    /// The attached review whose file is `at` (canonical).
    fn attached_at(&self, at: &Path) -> Option<usize> {
        self.session.reviews.iter().position(|r| {
            let p = sessionfile::resolve_path(&self.dir, &r.path);
            p.canonicalize().unwrap_or(p) == at
        })
    }

    /// The review an agent's `decide_suggestion` means, as
    /// `Opened::open_one` picks it for an agent: a review file inside the
    /// script's folder (attached here if it isn't yet), else an attached
    /// review's name or stored path; with none named, the only one attached.
    fn agent_pick(&mut self, pick: Option<&str>, cx: &mut Context<Self>) -> Result<usize, LiveError> {
        let script = self.script_arg();
        let Some(p) = pick else {
            return match self.session.reviews.len() {
                0 => Err(failed(format!("no review files attached to {script}; attach one first"))),
                1 => Ok(0),
                _ => Err(failed(format!(
                    "several reviews are attached; name one with review: {}",
                    attached_names(&self.session)
                ))),
            };
        };
        // A file is attached only if it is a review inside the folder.
        let mut not_file = None;
        match agent_review(&self.dir, p, Folders::MustExist) {
            Ok((at, true)) => {
                let at = at.canonicalize().unwrap_or(at);
                return match self.attached_at(&at) {
                    Some(r) => Ok(r),
                    None => self.attach_review(&at, cx),
                };
            }
            Ok((_, false)) => {}
            Err(e) => not_file = Some(e),
        }
        find_attached(&self.session, p).ok_or_else(|| {
            let names = attached_names(&self.session);
            failed(match &not_file {
                Some(why) if p.contains(['/', '\\', '.']) => {
                    format!("no attached review matches {p:?}, and as a file: {why}. Attached: {names}")
                }
                _ => format!("no attached review matches {p:?}. Attached: {names}"),
            })
        })
    }

    /// `decide_suggestion`: one undo step, like the writer's decision. The
    /// card showing that suggestion moves on, as after a key.
    pub(crate) fn agent_decide(
        &mut self,
        review: Option<&str>,
        n: SuggestionRef,
        choice: Choice,
        cx: &mut Context<Self>,
    ) -> Result<String, LiveError> {
        self.busy_while_rewriting()?;
        let before = self.before(cx);
        let r = match self.agent_pick(review, cx) {
            Ok(r) => r,
            Err(e) => {
                self.put_back(before, cx);
                return Err(e);
            }
        };
        let session = self.session.clone();
        let mut out = Err(String::new());
        self.editor.update(cx, |e, cx| {
            e.run(cx, |doc, sel| {
                out = ops::decide_in(&session, doc, sel, r, n, choice);
                Ok(doc.clamp_selection(sel))
            })
        });
        match out {
            Ok(text) => {
                self.card_after_agent_decision(r, cx);
                self.after_agent_change(cx);
                Ok(text)
            }
            // As on the files, where nothing is saved: a review attached
            // above is taken off again.
            Err(e) => {
                self.put_back(before, cx);
                Err(failed(e))
            }
        }
    }

    /// After an agent decided in review `r`: if the decision was on the
    /// suggestion its card shows, move on to the next pending one.
    fn card_after_agent_decision(&mut self, r: usize, cx: &mut Context<Self>) {
        let Some(i) = self.cur.get(r).copied().flatten() else { return };
        let doc = self.doc(cx);
        let key = self.session.key(r, i);
        let decided = doc.next_undo_meta_keys(false).contains(&key.as_str());
        if !decided || self.session.status(doc, r, i) == Status::Pending {
            return;
        }
        if self.review() == Some(r) {
            self.advance(r, i, cx);
        } else {
            self.cur[r] = self.session.next_pending(doc, r, Some(i));
        }
    }

    /// `edit_paragraph`: one undo step. Busy while the writer rewrites a
    /// line (refused, [`EDITING_LINE`], for that line itself).
    pub(crate) fn agent_edit(&mut self, para: u64, text: &str, cx: &mut Context<Self>) -> Result<String, LiveError> {
        if self.rewriting.is_some() {
            let editing = self.editing_paras(cx).contains(&ParaId(para));
            return Err(if editing { failed(EDITING_LINE) } else { LiveError::Busy });
        }
        let mut out = Err(String::new());
        self.editor.update(cx, |e, cx| {
            e.run(cx, |doc, sel| {
                out = ops::edit_in(doc, sel, para, text);
                Ok(doc.clamp_selection(sel))
            })
        });
        self.changed_if_ok(out, cx)
    }

    /// A host operation's result: the view catches up only if it did
    /// something.
    fn changed_if_ok(&mut self, out: Result<String, String>, cx: &mut Context<Self>) -> Result<String, LiveError> {
        if out.is_ok() {
            self.after_agent_change(cx);
        }
        out.map_err(LiveError::Failed)
    }

    /// `undo` / `redo`: the document's last step, whoever made it (the
    /// writer's typing included, as on the files: decision 10). Busy while
    /// the writer rewrites a line.
    pub(crate) fn agent_undo_redo(&mut self, redo: bool, cx: &mut Context<Self>) -> Result<String, LiveError> {
        self.busy_while_rewriting()?;
        let mut out = Err(String::new());
        self.editor.update(cx, |e, cx| {
            e.run(cx, |doc, sel| {
                out = ops::undo_redo_in(doc, redo);
                Ok(doc.clamp_selection(sel))
            })
        });
        self.changed_if_ok(out, cx)
    }

    /// `write_script`: the live document to `output` (or the default
    /// output), where an agent may write (`ops::agent_output_for`), with
    /// the bytes Write .fdx gives. Commits nothing: busy while the writer
    /// rewrites a line; refused on the version Scriptorium made without an
    /// `output` (the writer saves it in the app with Write .fdx).
    pub(crate) fn agent_write(&mut self, output: Option<&str>, cx: &mut Context<Self>) -> Result<String, LiveError> {
        self.busy_while_rewriting()?;
        let script = self.script_path.to_str().ok_or_else(|| failed("the script's path isn't UTF-8"))?.to_string();
        let pair = self.links.pair();
        let made = pair.is_made() || pair.maybe_made();
        let out = ops::agent_output_for(&script, &file_name(&self.script_path), made, output).map_err(failed)?;
        let doc = self.doc(cx).clone();
        let text = ops::write_doc(&self.fdx, &doc, &self.script_path, &out).map_err(failed)?;
        if out == super::output_path(&self.script_path) {
            self.applied = Some(Rc::new(doc.script().clone()));
        }
        self.say_agent_news(format!("An AI agent wrote {}.", file_name(&out)), cx);
        Ok(text)
    }

    /// `attach_reviews`: each file (checked by the caller: an existing
    /// review inside the script's folder) attached as File › Add review
    /// attaches it, unless it is already; all or none, as on the files.
    /// Busy while the writer rewrites a line.
    pub(crate) fn agent_attach(&mut self, files: &[PathBuf], cx: &mut Context<Self>) -> Result<String, LiveError> {
        self.busy_while_rewriting()?;
        let before = self.before(cx);
        let mut given = vec![];
        let mut result = Ok(());
        for f in files {
            let at = f.canonicalize().unwrap_or_else(|_| f.clone());
            let r = match self.attached_at(&at) {
                Some(r) => r,
                None => match self.attach_review(&at, cx) {
                    Ok(r) => r,
                    Err(e) => {
                        result = Err(e);
                        break;
                    }
                },
            };
            if !given.contains(&r) {
                given.push(r);
            }
        }
        if let Err(e) = result {
            self.put_back(before, cx);
            return Err(e);
        }
        self.after_agent_change(cx);
        Ok(ops::attach_text(&self.session, &given, &self.script_arg()))
    }

    /// `detach_reviews`: `files` as attached (stored paths) or absolute,
    /// matched as `ops::detach` matches them; all or none. The script's
    /// text keeps their decisions. Busy while the writer rewrites a line
    /// (its card may be one of them).
    pub(crate) fn agent_detach(&mut self, files: &[String], cx: &mut Context<Self>) -> Result<String, LiveError> {
        self.busy_while_rewriting()?;
        let script = self.script_arg();
        let mut gone: Vec<usize> = vec![];
        let mut out = String::new();
        for f in files {
            let at = Path::new(f).canonicalize().unwrap_or_else(|_| self.dir.join(f));
            let found: Vec<usize> = (0..self.session.reviews.len())
                .filter(|&r| !gone.contains(&r))
                .filter(|&r| {
                    let a = &self.session.reviews[r].path;
                    let p = sessionfile::resolve_path(&self.dir, a);
                    a == f || p.canonicalize().unwrap_or(p) == at
                })
                .collect();
            if found.is_empty() {
                return Err(failed(format!("{f} isn't attached to {script} (list_reviews shows what is)")));
            }
            gone.extend(found);
            out.push_str(&format!("removed {f} (the script's text keeps its decisions)\n"));
        }
        gone.sort_unstable();
        let shown = self.review();
        for &r in gone.iter().rev() {
            let review = Rc::make_mut(&mut self.session).reviews.remove(r);
            let p = sessionfile::resolve_path(&self.dir, &review.path);
            let p = p.canonicalize().unwrap_or(p);
            self.attached_since_sync.retain(|a| a.canonicalize().unwrap_or_else(|_| a.clone()) != p);
            if r < self.cur.len() {
                self.cur.remove(r);
            }
        }
        self.view = match shown {
            Some(r) if gone.contains(&r) => View::Script,
            Some(r) => View::Review(r - gone.iter().filter(|&&g| g < r).count()),
            None => View::Script,
        };
        self.hi = 0;
        self.after_agent_change(cx);
        Ok(out)
    }

    /// Tests: the suggestion the card shows (review, item).
    #[cfg(test)]
    pub(crate) fn card_for_test(&self) -> Option<(usize, usize)> {
        self.current()
    }
}

impl ScriptView {
    /// The line the writer looks at, as [`Self::where_is_writer`] places
    /// it (the card's while a review shows, else the cursor's), without
    /// the rest of its work: apps' pages follow it several times a second.
    pub(crate) fn writer_para(&self, cx: &App) -> ParaId {
        let editor = self.editor.read(cx);
        let doc = &editor.doc;
        match self.current() {
            Some((r, i)) => anchor_para(&self.session, doc, r, i),
            None => doc.clamp_selection(editor.sel).head.para,
        }
    }

    /// Where the writer is, for an outside agent's `where_is_writer` (plan
    /// "agent connect", C3): the open script, its pair, what shows, the card,
    /// the selection (or the cursor's line), the scene, the lines on screen,
    /// and whether there are changes not yet written. The agent panel's
    /// conversation is not part of it.
    pub(crate) fn where_is_writer(&self, cx: &App) -> ViewContext {
        let editor = self.editor.read(cx);
        let doc = &editor.doc;
        let pair = self.links.loaded().map(|p| {
            let other = p.other_index();
            PairPlace {
                this: p.links.label(p.this),
                other: p.links.label(other),
                other_file: p.links.members[other].file.clone(),
                this_is_version: p.this_is_version(),
                lined_up: self.links.shows_left() && !self.left_hidden,
            }
        });
        let view = match self.review() {
            Some(r) => Showing::Review(self.session.reviews[r].name.clone()),
            None => Showing::Script,
        };
        let card = self
            .current()
            .map(|(r, i)| Card { number: self.number(doc, r, i), para: anchor_para(&self.session, doc, r, i) });
        let sel = doc.clamp_selection(editor.sel);
        let selection = context::selected(doc, sel);
        // The line the writer looks at: the card's while a review shows,
        // else the cursor's.
        let at = card.as_ref().map(|c| c.para).unwrap_or(sel.head.para);
        let written = self.applied.as_deref() == Some(doc.script());
        ViewContext {
            script: self.script_path.display().to_string(),
            pair,
            view,
            card,
            selection,
            scene: context::scene_of(doc, at),
            visible: editor.visible(),
            unwritten: self.changed_lines(cx) > 0 && !written,
        }
    }
}

#[cfg(test)]
impl ScriptView {
    /// Tests: the writer selects `sel`.
    pub(crate) fn set_selection_for_test(&mut self, sel: scriptorium_core::Selection, cx: &mut Context<Self>) {
        self.editor.update(cx, |e, _| e.sel = sel);
    }
}
