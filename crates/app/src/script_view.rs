//! One open script: a sidebar that picks the view (the plain script, or one of
//! the reviews) and lists that view's changes, the script in the middle, and
//! the review card on the right. The window around it (toolbar, menus, empty
//! state) is `crate::workspace`.
//!
//! A review is a view of the script. Decisions go through the editor's
//! `Document` (see `scriptorium_review::Session`), so they change the script,
//! every view shows them, and typing and reviewing share one undo history.
//!
//! Focus decides what keys do. With the card focused (the default in a review),
//! single keys work as in script-review: A accept, R keep mine, E rewrite,
//! J/K next/previous… Clicking the script focuses the editor for typing; Esc
//! comes back to the card.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Context, Div, Entity, FocusHandle, Focusable, FontWeight, HighlightStyle, MouseButton,
    ScrollStrategy, SharedString, Stateful, StyledText, Subscription, Task, UniformListScrollHandle, Window, actions,
    div, prelude::*, px, relative, uniform_list,
};
use scriptorium_core::diff::{WordOp, word_diff};
use scriptorium_core::doc::Event;
use scriptorium_core::outline::{ParaContext, annotate};
use scriptorium_core::{Document, ParaId, Paragraph, Script};
use scriptorium_fdx::FdxFile;
use scriptorium_review::sessionfile::{self, Change, Disk, Resumed, Seen, SessionFile};
use scriptorium_review::{Decision, LineChange, Refusal, Review, Session, Status};
use scriptorium_tools::item_numbers;

use crate::blocks::{self, CardLine, Side};
use crate::disk::{self, Plan};
use crate::editor::{Decor, EditorEvent, Mark, ScriptEditor, is_changed};
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
        UndoAny,
        RedoAny,
        ShowScript,
        ShowReview1,
        ShowReview2,
        ShowReview3,
        NextView,
        PrevView,
    ]
);

const SCRIPT_FONT: &str = "Courier Prime";

/// The key that finishes a line rewrite (Enter splits the line).
const COMMIT_KEY: &str = if cfg!(target_os = "macos") { "Cmd+Enter" } else { "Ctrl+Enter" };

/// Everything read from disk to open a script.
pub struct Opened {
    pub script_path: PathBuf,
    raw: String,
    fdx: FdxFile,
    opened: sessionfile::ResumedSession,
    session_path: PathBuf,
    /// The script's folder, canonical: review paths are stored relative to it.
    dir: PathBuf,
    /// The session file as read (to notice another program changing it).
    seen: Seen,
    /// Review files attached now that the session file doesn't list yet.
    unsynced: Vec<PathBuf>,
    notices: Vec<String>,
}

// This mirrors `scriptorium_cli::Opened` rather than using it: the app sets a
// session that can't be used aside and starts fresh (the CLI refuses, so it
// never overwrites one), turns an unreadable review file into a notice (the
// CLI fails), and needs `Seen` for the session file it read.
impl Opened {
    /// Read the script, the review files attached to it last time plus
    /// `review_paths`, and the saved session. Only a script that can't be read
    /// is an error; a review file that can't be read becomes a notice.
    pub fn load(script_path: &Path, review_paths: &[PathBuf]) -> Result<Self, String> {
        let script_path = script_path.canonicalize().map_err(|e| format!("{}: {e}", script_path.display()))?;
        let raw = std::fs::read_to_string(&script_path).map_err(|e| format!("{}: {e}", file_name(&script_path)))?;
        let fdx = FdxFile::parse(raw.clone())
            .map_err(|e| format!("{}: {}", file_name(&script_path), capitalize(&e.to_string())))?;
        let dir = script_path.parent().map(Path::to_path_buf).unwrap_or_default();
        let session_path = dir.join(sessionfile::file_name_for(&file_name(&script_path)));
        let (mut opened, mut seen, mut notices) =
            resume_from_disk(&script_path, &raw, &fdx, &session_path, review_paths);
        let listed: Vec<String> = match &opened.how {
            Resumed::Resumed { .. } => std::fs::read_to_string(&session_path)
                .ok()
                .and_then(|j| SessionFile::from_json(&j).ok())
                .map(|f| f.reviews.into_iter().map(|a| a.path).collect())
                .unwrap_or_default(),
            _ => vec![],
        };
        let unsynced = opened
            .session
            .reviews
            .iter()
            .filter(|r| !listed.contains(&r.path))
            .map(|r| sessionfile::resolve_path(&dir, &r.path))
            .collect();
        match &opened.how {
            Resumed::Fresh => {}
            Resumed::Resumed { .. } => notices.insert(0, "Picked up where you left off.".into()),
            Resumed::Discarded(why) => {
                let aside = disk::set_aside_path(&dir, &file_name(&script_path));
                let kept = match std::fs::rename(&session_path, &aside) {
                    Ok(()) => {
                        seen = Seen::default();
                        format!("The old session was kept as {}.", file_name(&aside))
                    }
                    Err(e) => format!("Couldn't keep the old session as {}: {e}.", aside.display()),
                };
                crate::log::warn(&format!("session {} discarded: {why}. {kept}", session_path.display()));
                notices.insert(0, format!("Started fresh: {why}. {kept}"));
            }
        }
        notices.append(&mut opened.notes);
        Ok(Self { script_path, raw, fdx, opened, session_path, dir, seen, unsynced, notices })
    }
}

/// Resume the session saved next to the script with the review files it
/// attaches plus `review_paths` (as opening does, and again when another
/// program changed the files). Writes nothing. Returns the resumed session
/// (its `notes` not yet shown), the session file as read, and notices about
/// review files.
fn resume_from_disk(
    script_path: &Path,
    raw: &str,
    fdx: &FdxFile,
    session_path: &Path,
    review_paths: &[PathBuf],
) -> (sessionfile::ResumedSession, Seen, Vec<String>) {
    let dir = script_path.parent().map(Path::to_path_buf).unwrap_or_default();
    let script_name = file_name(script_path);
    // The time first: a write between the two shows as a newer time later.
    let mtime = modified(session_path);
    let saved = std::fs::read_to_string(session_path).ok();
    let seen = Seen::read(mtime, saved.as_deref());
    let mut notices = vec![];
    let attached: Vec<String> = saved
        .as_deref()
        .and_then(|j| SessionFile::from_json(j).ok())
        .map(|f| f.reviews.into_iter().map(|a| a.path).collect())
        .unwrap_or_default();
    let mut paths: Vec<PathBuf> = attached
        .iter()
        .map(|stored| {
            let p = sessionfile::resolve_path(&dir, stored);
            p.canonicalize().unwrap_or(p)
        })
        .collect();
    for p in review_paths {
        let p = p.canonicalize().unwrap_or_else(|_| p.clone());
        if !paths.contains(&p) {
            paths.push(p);
        }
    }
    let mut sources = vec![];
    for p in paths {
        match read_review(&p, &dir) {
            Ok(source) => sources.push(source),
            Err(e) => notices.push(format!("{e}. It was taken off this script's reviews.")),
        }
    }
    let caps = |k: &scriptorium_core::ElementKind| fdx.is_caps(k.name());
    // Always checked: reader 1 also misnumbered files without DualDialogue
    // (nested paragraphs in scene summaries and arc beats), and it's cheap.
    let reader_1 = Some(fdx.legacy_script());
    let opened =
        sessionfile::resume(sources, fdx.to_script(), reader_1.as_ref(), raw, saved.as_deref(), &caps, &script_name);
    for r in 0..opened.session.reviews.len() {
        notices.append(&mut review_notes(&opened.session, r));
    }
    (opened, seen, notices)
}

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Read one review file, to open with the script.
fn read_review(path: &Path, dir: &Path) -> Result<sessionfile::ReviewSource, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    Ok(sessionfile::ReviewSource { name: review_title(path), path: sessionfile::stored_path(dir, path), text })
}

/// What to tell the user about a review just opened.
fn review_notes(session: &Session, r: usize) -> Vec<String> {
    let review = &session.reviews[r];
    let mut notes: Vec<String> = review.warnings.iter().map(|w| format!("{}: {w}", review.name)).collect();
    // Blocks whose lines another review cut are named (they can't be
    // placed), and not counted again as not matching.
    let mut n = 0;
    for u in &review.unmatched {
        match &u.why {
            Some(why @ scriptorium_review::Unplaced::RemovedBy { .. }) => {
                notes.push(format!("{}: a suggestion's {}.", review.name, session.unplaced_label(why)));
            }
            _ => n += 1,
        }
    }
    if n > 0 {
        notes.push(format!(
            "{}: {n} suggestion{} didn't match a line (`scriptorium review` lists them).",
            review.name,
            if n == 1 { "" } else { "s" }
        ));
    }
    notes
}

pub fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

pub fn has_extension(p: &Path, ext: &str) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext))
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

/// The sidebar's scene list for the script, given its outline ([`annotate`]).
fn scene_rows(script: &Script, ctx: &[ParaContext]) -> Vec<Row> {
    ctx.iter()
        .zip(&script.paragraphs)
        .filter(|(c, _)| c.heading)
        .map(|(c, p)| Row::SceneLink(p.id, c.scene.clone().into()))
        .collect()
}

/// What only a structural edit changes: the paragraphs' ids and kinds, in
/// order. Typing leaves it alone, so the outline isn't rebuilt per keystroke.
fn structure_key(script: &Script) -> Vec<(ParaId, scriptorium_core::ElementKind)> {
    script.paragraphs.iter().map(|p| (p.id, p.kind.clone())).collect()
}

/// How long after the last keystroke the outline catches up with text edits
/// (a renamed scene heading or speaker).
const OUTLINE_SETTLE: Duration = Duration::from_millis(500);

/// One row of the sidebar's lower list.
#[derive(Clone, PartialEq)]
enum Row {
    Scene(SharedString),
    Item(usize),
    SceneLink(ParaId, SharedString),
}

pub struct ScriptView {
    editor: Entity<ScriptEditor>,
    session: Rc<Session>,
    /// The events that made the document's starting point (kept in the session file).
    base: Vec<Event>,
    script_path: PathBuf,
    dir: PathBuf,
    raw: Rc<str>,
    fdx: Rc<FdxFile>,
    session_path: PathBuf,
    originals: Rc<HashMap<ParaId, Paragraph>>,
    /// Scene and speaker per paragraph position, for the live document as of
    /// the last [`Self::rebuild_outline`]. Look positions up with
    /// `doc.index_of(para)` and `get`, never with an as-opened index.
    ctx: Rc<Vec<ParaContext>>,
    /// [`structure_key`] as of the last outline rebuild.
    structure: Vec<(ParaId, scriptorium_core::ElementKind)>,
    /// A pending outline rebuild after text edits.
    outline_task: Option<Task<()>>,
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
    /// Each review's item numbers (`scriptorium_tools::item_numbers`), as of
    /// the last refresh, so rendering a card doesn't work them out again.
    numbers: Vec<Vec<usize>>,
    item_by_para: Vec<HashMap<ParaId, usize>>,
    queue_scroll: UniformListScrollHandle,
    /// The status bar's message (cleared by the next key the writer presses).
    notice: SharedString,
    /// An error the writer may not have seen yet: shown beside `notice`, and
    /// never replaced by a background notice (a reload), only by the writer's
    /// next action.
    error: SharedString,
    /// The notice for recent reloads, merged while it's on screen, so an agent
    /// writing every second gives one growing notice, not a new one each time.
    reload_note: Option<(Instant, disk::Summary)>,
    /// Notes about review files already shown (a reload doesn't repeat them).
    shown_notes: HashSet<String>,
    /// The script as last written to the .fdx.
    applied: Option<Rc<Script>>,
    autosave: Option<Task<()>>,
    /// The session file as the app last read or wrote it, and the log length
    /// it holds: events after that are the app's own, not yet saved.
    seen_session: Seen,
    synced: usize,
    /// Review files attached here since the session file was last read or
    /// written: a reload keeps these (and only these) attached.
    attached_since_sync: Vec<PathBuf>,
    /// Another program wrote a session the app can't use (why): the app stops
    /// saving over it until the file is usable again (or removed).
    stuck: Option<String>,
    /// Looks at the files every so often (see `crate::disk`).
    _watch: Task<()>,
    _subscription: Subscription,
}

/// Said when a rewrite is undone because its line went away.
const REWRITE_LOST: &str = "The line you were rewriting was removed, so the rewrite was undone.";

/// How often the app looks for changes another program made to its files.
const WATCH_EVERY: Duration = Duration::from_millis(1000);

impl ScriptView {
    pub fn new(opened: Opened, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Opened { script_path, raw, fdx, opened, session_path, dir, seen, unsynced, notices } = opened;
        let sessionfile::ResumedSession { session, doc, base, .. } = opened;
        let synced = doc.log().len();
        let originals: HashMap<ParaId, Paragraph> = fdx.to_script().paragraphs.into_iter().map(|p| (p.id, p)).collect();

        let editor = cx.new(|cx| ScriptEditor::new(doc, cx));
        let subscription = cx.subscribe_in(&editor, window, Self::on_editor_event);
        let watch = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(WATCH_EVERY).await;
                if this.update(cx, |w, cx| w.watch_files(cx)).is_err() {
                    break;
                }
            }
        });
        let n = session.reviews.len();
        let mut this = Self {
            editor,
            session: Rc::new(session),
            base,
            script_path,
            dir,
            raw: raw.into(),
            fdx: Rc::new(fdx),
            session_path,
            originals: Rc::new(originals),
            ctx: Rc::default(),
            structure: vec![],
            outline_task: None,
            view: View::Script,
            cur: vec![None; n],
            hi: 0,
            rewriting: None,
            focus: cx.focus_handle(),
            rows: vec![],
            scene_rows: Rc::default(),
            row_of_item: vec![],
            numbers: vec![],
            item_by_para: vec![],
            queue_scroll: UniformListScrollHandle::new(),
            notice: notices.join("  ").into(),
            error: "".into(),
            reload_note: None,
            shown_notes: notices.iter().cloned().collect(),
            applied: None,
            autosave: None,
            seen_session: seen,
            synced,
            attached_since_sync: unsynced,
            stuck: None,
            _watch: watch,
            _subscription: subscription,
        };
        this.build_rows(cx);
        let first = if n > 0 { View::Review(0) } else { View::Script };
        this.set_view(first, window, cx);
        this
    }

    /// The per-review sidebar rows and lookups, after the reviews change.
    /// Items are listed in script order, the order their numbers follow
    /// (`scriptorium_tools::item_numbers`, as the CLI and tools number them).
    fn build_rows(&mut self, cx: &App) {
        self.rebuild_outline(cx);
        let doc = self.doc(cx);
        let ctx = &self.ctx;
        let scene_of = |para: ParaId| -> &str { para_ctx(ctx, doc, para).map_or("", |c| c.scene.as_str()) };
        let scene_name = |scene: &str| -> SharedString {
            if scene.is_empty() { "Before the first scene".into() } else { scene.to_string().into() }
        };
        let (mut rows, mut row_of_item, mut item_by_para) = (vec![], vec![], vec![]);
        for (r, review) in self.session.reviews.iter().enumerate() {
            let (mut rs, mut of) = (vec![], vec![0; review.items.len()]);
            let mut last_scene: Option<&str> = None;
            for i in in_script_order(doc, &review.items) {
                let scene = scene_of(anchor_para(&self.session, doc, r, i));
                if last_scene != Some(scene) {
                    rs.push(Row::Scene(scene_name(scene)));
                    last_scene = Some(scene);
                }
                of[i] = rs.len();
                rs.push(Row::Item(i));
            }
            rows.push(Rc::new(rs));
            row_of_item.push(of);
            item_by_para.push(items_by_para(&self.session, doc, r));
        }
        self.rows = rows;
        self.row_of_item = row_of_item;
        self.item_by_para = item_by_para;
        self.cur.resize(self.session.reviews.len(), None);
    }

    /// Item `i`'s number in review `r`, from the last refresh (worked out
    /// afresh if the reviews changed since).
    fn number(&self, doc: &Document, r: usize, i: usize) -> usize {
        match self.numbers.get(r).and_then(|n| n.get(i)) {
            Some(&n) if self.numbers[r].len() == self.session.reviews[r].items.len() => n,
            _ => item_numbers(doc, &self.session.reviews[r].items)[i],
        }
    }

    pub fn script_path(&self) -> &Path {
        &self.script_path
    }

    pub fn set_notice(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.say(text);
        cx.notify();
    }

    /// A message in answer to something the writer did: it replaces any
    /// other (the writer has been looking at the screen).
    fn say(&mut self, text: impl Into<SharedString>) {
        self.notice = text.into();
        self.error = "".into();
        self.reload_note = None;
    }

    /// Something went wrong (possibly in the background, like an autosave).
    fn say_error(&mut self, text: impl Into<SharedString>) {
        let text = text.into();
        crate::log::error(&text);
        self.error = text;
    }

    /// What a reload brought in: merged with the previous reload's notice
    /// while that is still recent and showing; never hides an error.
    fn say_reloaded(&mut self, summary: disk::Summary) {
        const MERGE_FOR: Duration = Duration::from_secs(20);
        let summary = match self.reload_note.take() {
            Some((at, mut prev)) if at.elapsed() < MERGE_FOR => {
                prev.merge(summary);
                prev
            }
            _ => summary,
        };
        if let Some(text) = summary.render() {
            self.notice = text.into();
            self.reload_note = Some((Instant::now(), summary));
        }
    }

    /// File › Add review: open each file against the script (by `PARA:` as
    /// it is now, by `BEFORE` as it is in the `.fdx`), attach it, apply the decisions saved in it (one undo step
    /// each), and show the first one added.
    pub fn add_reviews(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        self.commit_rewrite_quietly(cx);
        let script = self.fdx.to_script();
        let script_name = file_name(&self.script_path);
        let (mut notes, mut first) = (vec![], None);
        for p in paths {
            if !has_extension(p, "txt") {
                notes.push(format!("{} isn't a review file (they end in .txt).", file_name(p)));
                continue;
            }
            let p = p.canonicalize().unwrap_or_else(|_| p.clone());
            // Held from reading the file through pinning its id, so a
            // concurrent CLI/MCP append isn't overwritten.
            let lock = self.lock_session(scriptorium_cli::fsio::LOCK_WAIT);
            let source = match read_review(&p, &self.dir) {
                Ok(s) => s,
                Err(e) => {
                    notes.push(e);
                    continue;
                }
            };
            let fdx = &self.fdx;
            let review = Review::open_live(
                source.name,
                &source.text,
                &script,
                self.doc(cx),
                |k| fdx.is_caps(k.name()),
                &script_name,
            )
            .with_path(source.path);
            let restored = review.restored();
            let r = match Rc::make_mut(&mut self.session).add(review) {
                Ok(r) => r,
                Err(e) => {
                    notes.push(e);
                    continue;
                }
            };
            self.attached_since_sync.push(p.clone());
            // Attached: pin the review's id into a file that has no `Id:`
            // header, so later edits to the file don't orphan its decisions.
            // Written now, before the session that names it (host contract).
            if let Some(text) = Rc::make_mut(&mut self.session).reviews[r].pin_id() {
                if lock.is_none() {
                    notes.push(format!(
                        "{} is busy in another program; its Id: wasn't written (it is attached).",
                        file_name(&p)
                    ));
                } else if let Err(e) = scriptorium_cli::write_atomic(&p, text.as_bytes()) {
                    notes.push(format!("Couldn't write the review's Id: into {}: {e}", p.display()));
                }
            }
            drop(lock);
            if restored > 0 {
                let session = self.session.clone();
                self.editor.update(cx, |e, cx| {
                    e.run(cx, |doc, sel| {
                        session.apply_saved(doc, r);
                        Ok(doc.clamp_selection(sel))
                    })
                });
                notes.push(format!(
                    "{}: {restored} decisions saved in the file were applied (one undo step).",
                    self.session.reviews[r].name
                ));
            }
            first.get_or_insert(r);
        }
        // Blocks whose lines another review cut, named once all are added.
        if let Some(r0) = first {
            let doc = self.doc(cx).clone();
            let session = Rc::make_mut(&mut self.session);
            session.note_removed_by(&doc);
            for r in r0..session.reviews.len() {
                notes.append(&mut review_notes(session, r));
            }
        }
        self.build_rows(cx);
        if let Some(r) = first {
            self.set_view(View::Review(r), window, cx);
            self.schedule_autosave(cx);
        }
        self.shown_notes.extend(notes.iter().cloned());
        self.say(notes.join("  "));
        cx.notify();
    }

    /// Before the window closes or switches script: finish a rewrite, save.
    /// Never drops the writer's work silently: if it can't be saved to the
    /// session file (another program left one that can't be used, or the
    /// write failed), it is written beside it as
    /// `X.scriptorium.set-aside-<time>.json`. Returns what to tell the writer.
    pub fn save_now(&mut self, cx: &mut Context<Self>) -> Option<String> {
        self.commit_rewrite_quietly(cx);
        self.autosave = None;
        // Worth waiting a moment for another program to finish writing; if
        // it doesn't, the work is set aside below.
        self.save_session_within(scriptorium_cli::fsio::LOCK_WAIT, cx);
        if self.doc(cx).log().len() <= self.synced {
            return None;
        }
        let doc = self.doc(cx).clone();
        Some(self.set_aside(&doc, "couldn't be saved to the session file", true))
    }

    /// Write the app's session for `doc` aside (see [`Self::save_now`]),
    /// log where, and return a notice saying so.
    /// `loud`: also show it as an error (a reload's own notice already says
    /// where the lost text went, so it passes false).
    fn set_aside(&mut self, doc: &Document, why: &str, loud: bool) -> String {
        let json = SessionFile::new(&self.raw, self.session.attached(), &self.base, doc.log()).to_json();
        let path = disk::set_aside_path(&self.dir, &file_name(&self.script_path));
        let msg = match scriptorium_cli::write_atomic(&path, json.as_bytes()) {
            Ok(()) => format!("Your unsaved work {why}; it was kept in {}.", file_name(&path)),
            Err(e) => format!("Your unsaved work {why}, and keeping it in {} failed too: {e}", path.display()),
        };
        crate::log::warn(&format!("{msg} ({})", path.display()));
        if loud {
            self.error = msg.clone().into();
        }
        msg
    }

    /// The toolbar's summary of what's changed and whether it's been written.
    pub fn save_state(&self, cx: &App) -> String {
        let changed = self.changed_lines(cx);
        let written = self.applied.as_deref() == Some(self.doc(cx).script());
        match (changed, written) {
            (0, _) => "Same as the original".to_string(),
            (1, true) => format!("1 line changed · written to {}", file_name(&output_path(&self.script_path))),
            (1, false) => "1 line changed · not yet written".to_string(),
            (n, true) => format!("{n} lines changed · written to {}", file_name(&output_path(&self.script_path))),
            (n, false) => format!("{n} lines changed · not yet written"),
        }
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

    /// Scenes and speakers for the live document. The scene list is replaced
    /// only if it changed, so the sidebar doesn't move under the writer.
    fn rebuild_outline(&mut self, cx: &App) {
        self.outline_task = None;
        let script = self.doc(cx).script();
        let ctx = annotate(script);
        let rows = scene_rows(script, &ctx);
        self.structure = structure_key(script);
        self.ctx = Rc::new(ctx);
        if *self.scene_rows != rows {
            self.scene_rows = Rc::new(rows);
        }
    }

    /// Catch the outline up with the document: at once after a structural
    /// edit, else (text edits, which can rename a scene or speaker) once
    /// typing pauses.
    fn update_outline(&mut self, cx: &mut Context<Self>) {
        if structure_key(self.doc(cx).script()) != self.structure {
            self.rebuild_outline(cx);
        } else {
            self.outline_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(OUTLINE_SETTLE).await;
                this.update(cx, |w, cx| {
                    w.rebuild_outline(cx);
                    cx.notify();
                })
                .ok();
            }));
        }
    }

    /// Push the current line and the margin marks to the editor.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if structure_key(self.doc(cx).script()) != self.structure {
            self.rebuild_outline(cx);
        }
        let doc = self.doc(cx);
        self.numbers = self.session.reviews.iter().map(|review| item_numbers(doc, &review.items)).collect();
        let protect = self.rewriting.as_ref().map(|rw| self.session.item(rw.r, rw.i).para);
        let mut decor = Decor { originals: self.originals.clone(), ..Decor::default() };
        // Lines move under blocks (accepting one gives its lines new ids), so
        // which item a line belongs to is worked out again each time.
        self.item_by_para = (0..self.session.reviews.len()).map(|r| items_by_para(&self.session, doc, r)).collect();
        if let Some(r) = self.review() {
            let doc = self.doc(cx);
            // One dot per line, of the item the line belongs to (a line a
            // suggestion covers wins over a cut's position on it).
            let marks: Vec<Mark> =
                (0..self.session.reviews[r].items.len()).map(|i| mark_of(&self.session, doc, r, i)).collect();
            decor.marks = self.item_by_para[r].iter().map(|(&para, &i)| (para, marks[i])).collect();
            if let Some(i) = self.cur[r] {
                decor.current = dot_paras(&self.session, doc, r, i).into_iter().collect();
            }
        }
        self.editor.update(cx, |e, cx| {
            e.decor = decor;
            e.protect = protect;
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
        let para = anchor_para(&self.session, self.doc(cx), r, i);
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
        if let Err(e) = self.session.check(self.doc(cx), r, i, &decision) {
            self.say(self.refusal_text(&e, r, i, cx));
            cx.notify();
            return;
        }
        self.say("");
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
        if self.session.item(r, i).block {
            self.edit_block(r, i, window, cx);
            return;
        }
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
        self.say("");
        let focus = self.editor.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        self.refresh(cx);
    }

    /// E on a block: accept the highlighted option (unless it is already what
    /// the lines say, or the writer has rewritten them), then put the cursor
    /// in its first changed line and hand the keys to the editor. There is no
    /// separate block editor: the writer edits with the normal editor, the
    /// status follows from the lines (B3), and Cmd/Ctrl+Enter on one of the
    /// lines records it and moves on.
    fn edit_block(&mut self, r: usize, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let k = self.hi.min(self.session.item(r, i).option_lines.len().saturating_sub(1));
        let status = self.session.status(self.doc(cx), r, i);
        let accept = !matches!(status, Status::Rewritten) && status != Status::Accepted(k);
        // Refused (lines restructured, decided in another review...): still
        // put the cursor on the block's lines, so the writer can edit them
        // there as the message says.
        let mut refused = None;
        if accept {
            match self.session.check(self.doc(cx), r, i, &Decision::Accept(k)) {
                Err(e) => refused = Some(self.refusal_text(&e, r, i, cx)),
                Ok(()) => {
                    self.rewriting = None;
                    let session = self.session.clone();
                    self.editor
                        .update(cx, |e, cx| e.run(cx, |doc, sel| session.decide(doc, sel, r, i, Decision::Accept(k))));
                }
            }
        }
        let doc = self.doc(cx);
        let item = self.session.item(r, i);
        let run = self.session.live_run(doc, r, i);
        let first = if self.session.status(doc, r, i) == Status::Accepted(k) {
            blocks::first_changed(&item.before_lines, &item.option_lines[k]).and_then(|j| run.get(j).copied())
        } else {
            None
        };
        let target = first.or_else(|| run.first().copied()).unwrap_or_else(|| anchor_para(&self.session, doc, r, i));
        self.editor.update(cx, |e, cx| {
            e.cursor_to_end(target, cx);
            e.reveal(target, cx);
        });
        self.say(refused.unwrap_or_else(|| {
            format!("Edit the lines in the script. {COMMIT_KEY} records them and moves on; Esc back to the card.")
        }));
        let focus = self.editor.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        self.schedule_autosave(cx);
        self.refresh(cx);
    }

    /// A refusal as the app words it: lines named by their first words, the
    /// app's own way out, and (lines partly gone) which review removed them.
    fn refusal_text(&self, refusal: &Refusal, r: usize, i: usize, cx: &App) -> String {
        let doc = self.doc(cx);
        let item = self.session.item(r, i);
        let words = |id| line_words(item, doc, id);
        refusal_text(refusal, hint_for(item), words, self.session.removed_by(doc, r, i))
    }

    /// Record what the line now says (an option, the original, or a rewrite).
    fn record_hand_edit(&mut self, r: usize, i: usize, cx: &mut Context<Self>) {
        let session = self.session.clone();
        self.editor.update(cx, |e, cx| e.run(cx, |doc, sel| session.decide_from_text(doc, sel, r, i)));
    }

    /// Record the rewrite's result, as one undo step with everything since E.
    /// If the line under review is gone (removed or merged into another by an
    /// edit the editor couldn't stop, e.g. deleting a selection across it),
    /// there is nothing to record: the rewrite is undone instead, with a
    /// notice. Returns whether it was recorded.
    fn finish_rewrite(&mut self, rw: &Rewriting, cx: &mut Context<Self>) -> bool {
        let para = self.session.item(rw.r, rw.i).para;
        if self.doc(cx).paragraph(para).is_none() {
            self.undo_to(rw.depth, cx);
            self.say(REWRITE_LOST);
            self.refresh(cx);
            return false;
        }
        self.record_hand_edit(rw.r, rw.i, cx);
        let depth = rw.depth;
        self.editor.update(cx, |e, _| e.doc.merge_undo_since(depth));
        self.schedule_autosave(cx);
        true
    }

    /// Undo back to undo depth `depth` (where a rewrite started).
    fn undo_to(&mut self, depth: usize, cx: &mut Context<Self>) {
        self.editor.update(cx, |e, cx| {
            e.run(cx, |doc, sel| {
                let mut sel = sel;
                while doc.undo_depth() > depth {
                    match doc.undo() {
                        Some(s) => sel = s,
                        None => break,
                    }
                }
                Ok(sel)
            })
        });
    }

    fn commit_rewrite(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rw) = self.rewriting.take() else { return };
        let recorded = self.finish_rewrite(&rw, cx);
        window.focus(&self.focus, cx);
        if recorded {
            self.advance(rw.r, rw.i, cx);
        } else {
            self.show_item(rw.r, rw.i, cx);
            self.refresh(cx);
        }
    }

    /// Finish a rewrite without moving on (switching view or item).
    fn commit_rewrite_quietly(&mut self, cx: &mut Context<Self>) {
        if let Some(rw) = self.rewriting.take() {
            self.finish_rewrite(&rw, cx);
        }
    }

    fn cancel_rewrite(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rw) = self.rewriting.take() else { return };
        self.undo_to(rw.depth, cx);
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
                self.update_outline(cx);
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
                    && dot_paras(&self.session, self.doc(cx), r, i).contains(&self.editor.read(cx).sel.head.para)
                {
                    // Edited the line under review by hand, then Cmd/Ctrl+Enter: that's a decision.
                    self.record_hand_edit(r, i, cx);
                    self.say("");
                    window.focus(&self.focus, cx);
                    self.advance(r, i, cx);
                }
            }
            EditorEvent::Split => {
                // Enter used to finish a line rewrite; it now splits the line.
                if self.rewriting.is_some() && crate::settings::first_time(crate::settings::ENTER_SPLITS) {
                    self.say(format!("Enter now starts a new line. Press {COMMIT_KEY} when you're done."));
                    cx.notify();
                }
            }
            EditorEvent::Notice(text) => {
                self.say(text.clone());
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
    /// If another program (the CLI, an agent through the MCP server) changed
    /// the session or a review file since the app last read or wrote it, the
    /// app reloads them instead, keeping its own unsaved changes on top.
    /// While another program holds the session lock, the app doesn't wait
    /// for it: it tries again shortly.
    fn save_session(&mut self, cx: &mut Context<Self>) {
        if !self.save_session_within(Duration::ZERO, cx) {
            self.schedule_autosave(cx);
        }
    }

    /// [`Self::save_session`], waiting up to `wait` for another program
    /// holding the session lock. False: it was still busy; nothing was done.
    fn save_session_within(&mut self, wait: Duration, cx: &mut Context<Self>) -> bool {
        if self.stuck.is_some() {
            return true;
        }
        // Held from looking at the files through writing them, so no other
        // program writes in between (see `scriptorium_cli::SessionLock`).
        let Some(_lock) = self.lock_session(wait) else { return false };
        match self.disk_plan().1 {
            Plan::Save => self.write_session(cx),
            Plan::Reload => self.reload(cx),
            // Finishing the rewrite saves again.
            Plan::Wait => {}
        }
        true
    }

    fn write_session(&mut self, cx: &mut Context<Self>) {
        let doc = self.doc(cx);
        let len = doc.log().len();
        let json = SessionFile::new(&self.raw, self.session.attached(), &self.base, doc.log()).to_json();
        match scriptorium_cli::write_atomic(&self.session_path, json.as_bytes()) {
            Ok(()) => {
                self.seen_session = Seen::wrote(&json);
                self.synced = len;
                self.attached_since_sync.clear();
            }
            Err(e) => {
                self.say_error(format!("Couldn't save the session to {}: {e}", self.session_path.display()));
                cx.notify();
            }
        }
    }

    /// Take the script's session lock, waiting up to `wait`. None: another
    /// program holds it. A lock that can't be taken for another reason (the
    /// folder is read-only, the file system has no locks) is logged and the
    /// app goes on unlocked, as it did before there was a lock.
    fn lock_session(&self, wait: Duration) -> Option<Option<scriptorium_cli::SessionLock>> {
        let at = scriptorium_cli::SessionLock::path_for(&self.script_path);
        match scriptorium_cli::SessionLock::acquire(&at, wait) {
            Ok(lock) => Some(Some(lock)),
            Err(scriptorium_cli::fsio::LockError::Busy) => None,
            Err(e) => {
                crate::log::warn(&format!("couldn't lock the session ({e}); going on without the lock"));
                Some(None)
            }
        }
    }

    /// Compare the session file and each review file with what the app
    /// holds: whether the session file changed, and what to do.
    fn disk_plan(&mut self) -> (bool, Plan) {
        let path = &self.session_path;
        let session = sessionfile::check(&self.seen_session, modified(path), || std::fs::read_to_string(path).ok());
        if let Disk::Touched(seen) = &session {
            self.seen_session = seen.clone();
        }
        let reviews_changed = self.session.reviews.iter().any(|r| {
            let at = sessionfile::resolve_path(&self.dir, &r.path);
            sessionfile::review_changed(r.text(), std::fs::read_to_string(at).ok().as_deref())
        });
        (session == Disk::Changed, disk::plan(&session, reviews_changed, self.rewriting.is_some()))
    }

    /// Every so often: pick up what another program wrote, so a suggestion an
    /// agent adds shows up without waiting for the next edit here. While the
    /// session file can't be used, only a new version of it (or its removal)
    /// is worth another look.
    fn watch_files(&mut self, cx: &mut Context<Self>) {
        // Another program is writing: look again next time.
        let Some(_lock) = self.lock_session(Duration::ZERO) else { return };
        if self.stuck.is_some() && !self.session_path.exists() {
            crate::log::info(&format!("{} was removed; saving again", self.session_path.display()));
            self.stuck = None;
            self.write_session(cx);
            cx.notify();
            return;
        }
        let (session_changed, plan) = self.disk_plan();
        if plan == Plan::Reload && (self.stuck.is_none() || session_changed) {
            self.reload(cx);
        }
    }

    /// Resume again from the files on disk (the script itself is the one
    /// opened: neither the app nor the CLI writes it), put the app's unsaved
    /// events back on top (see `sessionfile::with_unsaved`), keep the view
    /// where it was, and say what changed. The undo history is the session
    /// file's, so undo can take back the other program's steps too, in order,
    /// as the CLI's `undo` does. Work that couldn't be put back is named in
    /// the notice and the log, and the app's whole session as it was is
    /// written aside. Call it holding the session lock (it writes the merge).
    fn reload(&mut self, cx: &mut Context<Self>) {
        let old_doc = self.doc(cx).clone();
        let (opened, seen, notes) = resume_from_disk(&self.script_path, &self.raw, &self.fdx, &self.session_path, &[]);
        if let Resumed::Discarded(why) = &opened.how {
            if self.stuck.is_none() {
                crate::log::warn(&format!(
                    "session {} changed on disk and can't be used: {why}",
                    self.session_path.display()
                ));
            }
            self.stuck = Some(why.clone());
            // Look again only when it changes again.
            self.seen_session = seen;
            cx.notify();
            return;
        }
        if self.stuck.take().is_some() {
            crate::log::info(&format!("{} is usable again", self.session_path.display()));
        }
        // Only what the file still holds of the app's log is synced: a program
        // that read the file before the app's last save and wrote after it
        // dropped the app's newest events, which are then unsaved work again.
        let synced = sessionfile::still_synced(self.synced, old_doc.log(), opened.doc.log());
        if synced < self.synced.min(old_doc.log().len()) {
            crate::log::warn(&format!(
                "another program wrote {} without {} of the app's saved changes; putting them back",
                self.session_path.display(),
                self.synced.min(old_doc.log().len()) - synced
            ));
        }
        let (start, unsaved) = match sessionfile::rebuild(&old_doc, synced) {
            Ok(d) => (d, &old_doc.log()[synced..]),
            Err(e) => {
                // Can't happen for the app's own log; if it does, keep the work aside.
                crate::log::error(&format!("couldn't rebuild the session at event {synced}: {e}"));
                self.set_aside(&old_doc, "couldn't be put back after a reload", true);
                (old_doc.clone(), &[][..])
            }
        };
        // Where the view was, by review id and file entry (both survive a reload).
        let at = |session: &Session, r: usize, i: Option<usize>| {
            (session.reviews[r].id.clone(), i.map(|i| session.item(r, i).entry))
        };
        let was: Vec<(String, Option<usize>)> =
            (0..self.session.reviews.len()).map(|r| at(&self.session, r, self.cur[r])).collect();
        let view_id = self.review().map(|r| self.session.reviews[r].id.clone());
        let was_current = self.current().map(|(r, i)| at(&self.session, r, Some(i)));

        let sessionfile::ResumedSession { mut session, doc: reloaded, base, notes: more, .. } = opened;
        let new_synced = reloaded.log().len();
        let replayed = sessionfile::with_unsaved(start.clone(), unsaved, reloaded.clone());
        let mut notes: Vec<String> = notes.into_iter().chain(more).collect();
        // Reviews attached here since the last save stay attached (their
        // saved decisions are among the unsaved events just replayed); any
        // other review the session file no longer lists was detached there.
        let script = self.fdx.to_script();
        let script_name = file_name(&self.script_path);
        let mut still = vec![];
        for p in std::mem::take(&mut self.attached_since_sync) {
            match read_review(&p, &self.dir) {
                Ok(src) => {
                    let fdx = &self.fdx;
                    let review = Review::open_live(
                        src.name,
                        &src.text,
                        &script,
                        &replayed.doc,
                        |k| fdx.is_caps(k.name()),
                        &script_name,
                    )
                    .with_path(src.path);
                    // Already attached by the other program: nothing to keep.
                    if session.add(review).is_ok() {
                        still.push(p);
                    }
                }
                Err(e) => notes.push(format!("{e}. It was taken off this script's reviews.")),
            }
        }
        self.attached_since_sync = still;
        session.note_removed_by(&replayed.doc);

        let mut summary = disk::summarize(&self.session, &start, &session, &reloaded);
        summary.kept = count_changes(&session, &replayed.kept);
        summary.lost = self.describe_lost(&session, &start, &replayed.doc, &replayed.lost);
        notes.retain(|n| self.shown_notes.insert(n.clone()));
        summary.notes = notes;
        let lost = !replayed.lost.is_empty();
        if lost {
            for c in &replayed.lost {
                match c {
                    Change::Para(id, text) => crate::log::warn(&format!(
                        "reload: your change to paragraph {} clashed with another program's and wasn't applied. Your text: {text}",
                        id.0
                    )),
                    Change::Removed(id, text) => crate::log::warn(&format!(
                        "reload: removing paragraph {} clashed with another program's change and wasn't applied. Its text was: {text}",
                        id.0
                    )),
                    Change::Meta(key) => crate::log::warn(&format!(
                        "reload: your decision {key} clashed with another program's and wasn't applied"
                    )),
                }
            }
            self.set_aside(&old_doc, "clashed with another program's changes", false);
        }

        if !replayed.renumber.is_identity() {
            let moved: Vec<String> = old_doc
                .paragraphs()
                .iter()
                .filter_map(|p| {
                    let to = replayed.renumber.id(p.id);
                    (to != p.id && replayed.doc.paragraph(to).is_some()).then(|| format!("{}\u{2192}{}", p.id.0, to.0))
                })
                .collect();
            if !moved.is_empty() {
                crate::log::info(&format!(
                    "reload: another program added lines too; your new lines were renumbered ({})",
                    moved.join(", ")
                ));
            }
        }
        let renumber = replayed.renumber;
        let hi = self.hi;
        self.session = Rc::new(session);
        self.base = base;
        self.seen_session = seen;
        self.synced = new_synced;
        self.rewriting = None;
        let doc = replayed.doc;
        self.editor.update(cx, |e, cx| {
            e.run(cx, |d, sel| {
                *d = doc;
                Ok(d.clamp_selection(renumber.selection(sel)))
            })
        });
        self.cur = self
            .session
            .reviews
            .iter()
            .map(|r| {
                let (_, entry) = was.iter().find(|(id, _)| *id == r.id)?;
                r.items.iter().position(|it| Some(it.entry) == *entry)
            })
            .collect();
        self.view = view_id
            .and_then(|id| self.session.reviews.iter().position(|r| r.id == id))
            .map_or(View::Script, View::Review);
        self.build_rows(cx);
        if let Some(r) = self.review()
            && self.cur[r].is_none()
        {
            self.cur[r] = self.session.next_pending(self.doc(cx), r, None);
        }
        // The highlighted option stays if the card shows the same suggestion.
        let same = self.current().map(|(r, i)| at(&self.session, r, Some(i))) == was_current && was_current.is_some();
        self.hi = match self.current() {
            Some((r, i)) if same => hi.min(self.session.item(r, i).options.len().saturating_sub(1)),
            _ => 0,
        };

        crate::log::info(&format!(
            "reloaded {}: {}",
            self.session_path.display(),
            summary.render().unwrap_or_default()
        ));
        self.say_reloaded(summary);
        // The kept changes (and reviews attached here) aren't on disk yet.
        if self.doc(cx).log().len() > new_synced || !self.attached_since_sync.is_empty() {
            self.write_session(cx);
        }
        self.refresh(cx);
    }

    /// The writer's changes a reload couldn't apply, for the notice: the text
    /// of each line (quoted), what happened to a line without text, or which
    /// decision for a decision alone. `start` is the document as last synced.
    fn describe_lost(&self, session: &Session, start: &Document, doc: &Document, lost: &[Change]) -> Vec<String> {
        let paras: HashSet<ParaId> = lost
            .iter()
            .filter_map(|c| match c {
                Change::Para(id, _) | Change::Removed(id, _) => Some(*id),
                Change::Meta(_) => None,
            })
            .collect();
        let quoted = |text: &str| format!("\u{201c}{}\u{201d}", disk::excerpt(text, 30));
        lost.iter()
            .filter_map(|c| match c {
                // `id` is as the reloaded document has it, but looking it up
                // in `start` is safe: only ids the host gave out after `start`
                // (at or above its `next_id`) are renumbered, and they move
                // further up, so a renumbered id is never in `start` (the line
                // is new, as the fallback says). Ids below that keep their
                // meaning in both.
                Change::Para(id, text) if text.is_empty() => Some(match start.paragraph(*id) {
                    Some(was) if !was.text().is_empty() => format!("emptying {}", quoted(&was.text())),
                    _ => "a new empty line".to_string(),
                }),
                Change::Para(_, text) => Some(quoted(text)),
                Change::Removed(_, text) if text.is_empty() => Some("removing an empty line".to_string()),
                Change::Removed(_, text) => Some(format!("removing {}", quoted(text))),
                Change::Meta(key) => {
                    let (r, i) = item_with_key(session, key)?;
                    let item = session.item(r, i);
                    (!paras.contains(&item.para)).then(|| {
                        let n = item_numbers(doc, &session.reviews[r].items)[i];
                        format!("your decision on {} #{n}", session.reviews[r].name)
                    })
                }
            })
            .collect()
    }

    pub fn apply_to_fdx(&mut self, _: &ApplyToFdx, _: &mut Window, cx: &mut Context<Self>) {
        self.commit_rewrite_quietly(cx);
        let out = output_path(&self.script_path);
        let script = self.doc(cx).script().clone();
        let said = match self.fdx.write_script(&script) {
            Ok(raw) => match scriptorium_cli::write_atomic(&out, raw.as_bytes()) {
                Ok(()) => {
                    self.applied = Some(Rc::new(script));
                    format!("Wrote {}. The original is untouched.", file_name(&out))
                }
                Err(e) => {
                    crate::log::error(&format!("writing {}: {e}", out.display()));
                    format!("Couldn't write {}: {e}", out.display())
                }
            },
            Err(e) => {
                // E.g. a structural edit the .fdx writer can't make: never hidden.
                self.say("");
                self.say_error(format!("Not written: {e}"));
                self.save_session(cx);
                cx.notify();
                return;
            }
        };
        self.say(said);
        self.save_session(cx);
        cx.notify();
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
        self.say("");
        // By script position (the numbers shown), not file order.
        let order = in_script_order(self.doc(cx), &self.session.reviews[r].items);
        let at = self.cur[r].and_then(|i| order.iter().position(|&o| o == i));
        let k = match (at, forward) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(k), true) => (k + 1).min(n - 1),
            (Some(k), false) => k.saturating_sub(1),
        };
        self.go_to(order[k], window, cx);
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
    /// What the next undo (or redo) would take back, for the notice: the
    /// decisions it changes (whoever made them: undo is shared with the CLI
    /// and agents), else how many lines. None if there's nothing to undo.
    fn describe_step(&self, redo: bool, cx: &App) -> Option<String> {
        let doc = self.doc(cx);
        if !(if redo { doc.can_redo() } else { doc.can_undo() }) {
            return None;
        }
        let verb = if redo { "Redid" } else { "Undid" };
        let keys = doc.next_undo_meta_keys(redo);
        let mut found: Vec<(usize, usize)> = vec![];
        for key in keys {
            if let Some(ri) = item_with_key(&self.session, key)
                && !found.contains(&ri)
            {
                found.push(ri);
            }
        }
        if !found.is_empty() {
            let one_review = found.iter().all(|(r, _)| *r == found[0].0);
            let names: Vec<String> = found
                .iter()
                .map(|&(r, i)| {
                    let n = item_numbers(doc, &self.session.reviews[r].items)[i];
                    if one_review && self.review() == Some(r) {
                        format!("#{n}")
                    } else {
                        format!("{} #{n}", self.session.reviews[r].name)
                    }
                })
                .collect();
            return Some(format!("{verb} the decision on {}.", names.join(", ")));
        }
        let n = doc.next_undo_paras(redo).len();
        Some(format!("{verb} an edit to {n} line{}.", if n == 1 { "" } else { "s" }))
    }

    fn undo_redo(&mut self, redo: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.rewriting = None;
        let said = self.describe_step(redo, cx);
        self.say(said.unwrap_or_default());
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
            self.say(if redo { "Nothing to redo." } else { "Nothing to undo." });
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
    /// Edit › Undo: the review's undo when the card has focus, else the editor's.
    pub fn undo_any(&mut self, redo: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus.is_focused(window) {
            self.undo_redo(redo, window, cx);
        } else {
            self.rewriting = None;
            // Typing undoes quietly; a decision (maybe the CLI's) is named.
            let said = self.describe_step(redo, cx).filter(|s| s.contains("decision"));
            self.say(said.unwrap_or_default());
            self.editor.update(cx, |e, cx| {
                e.run(cx, |doc, sel| Ok(if redo { doc.redo() } else { doc.undo() }.unwrap_or(sel)))
            });
            self.refresh(cx);
        }
    }
    fn on_undo_any(&mut self, _: &UndoAny, window: &mut Window, cx: &mut Context<Self>) {
        self.undo_any(false, window, cx);
    }
    fn on_redo_any(&mut self, _: &RedoAny, window: &mut Window, cx: &mut Context<Self>) {
        self.undo_any(true, window, cx);
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
    pub fn show_nth(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(v) = self.views().get(n).copied() {
            self.set_view(v, window, cx);
        }
    }
    pub fn cycle_view(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Lines changed, added or retyped, plus lines removed, since the file was opened.
    fn changed_lines(&self, cx: &App) -> usize {
        let doc = self.doc(cx);
        let live = doc.paragraphs().iter().filter(|p| is_changed(&self.originals, p)).count();
        let removed = self.originals.keys().filter(|id| doc.paragraph(**id).is_none()).count();
        live + removed
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
                    .child(format!(
                        "None yet. File › Add review… ({}) or drop a review .txt file on the window.",
                        crate::menus::shortcut("shift-o")
                    ))
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
                .map(|(i, _)| (mark_of(&self.session, doc, r, i), item_text(&self.session, doc, r, i).into()))
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
                    "{} accepted · {} kept · {} rewritten. {} writes the script.",
                    p.accepted,
                    p.rejected,
                    p.rewritten,
                    crate::menus::shortcut("s")
                ))
                .into_any_element(),
            );
            body.push(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button("finish-apply", "Write .fdx", Some(&crate::menus::shortcut("s")), true)
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
        if item.block {
            self.block_card(&mut body, r, i, cx);
            let status = self.session.status(self.doc(cx), r, i);
            self.card_footer(&mut body, r, i, status, focused, cx);
            return shell.children(body);
        }
        let fallback = ParaContext::default();
        let ctx = para_ctx(&self.ctx, doc, item.para).unwrap_or(&fallback);
        let caps = doc.paragraph(item.para).is_some_and(|p| self.fdx.is_caps(p.kind.name()));
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
        let number = self.number(doc, r, i);

        body.push(
            div()
                .flex()
                .flex_col()
                .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(INK).child(who))
                .child(small(format!(
                    "{} · change {} of {total}",
                    if ctx.scene.is_empty() { "Before the first scene" } else { &ctx.scene },
                    number
                )))
                .into_any_element(),
        );
        if item.fuzzy {
            body.push(warn_box("Matched approximately: check this is the right line.", None));
        }
        let rewriting_here = self.rewriting.as_ref().is_some_and(|rw| rw.r == r && rw.i == i);
        let change = self.session.line_change(doc, r, i);
        if !rewriting_here && let Some(text) = card_change_notice(change, status, self.session.is_outdated(doc, r, i)) {
            body.push(warn_box(text, Some(item.before.clone())));
        }
        if rewriting_here {
            body.push(label("Rewriting in the script"));
            body.push(
                small(format!("Edit the highlighted line. {COMMIT_KEY} saves and moves on; Esc puts it back."))
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
            Status::Rewritten if change == Some(LineChange::Removed) => Some("line cut".into()),
            Status::Rewritten => Some("rewritten".into()),
            Status::Removed => Some("line removed".into()),
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
        self.card_footer(&mut body, r, i, status, focused, cx);
        shell.children(body)
    }

    /// The card's middle for a block: its heading, why it can't be decided
    /// (if so), the lines it left once decided, and each option as BEFORE
    /// struck in red and AFTER in green, laid out by element kind.
    fn block_card(&self, body: &mut Vec<AnyElement>, r: usize, i: usize, cx: &mut Context<Self>) {
        let doc = self.doc(cx);
        let item = self.session.item(r, i);
        let status = self.session.status(doc, r, i);
        let run = self.session.live_run(doc, r, i);
        let hi = self.hi.min(item.option_lines.len().saturating_sub(1));
        let fallback = ParaContext::default();
        let ctx = para_ctx(&self.ctx, doc, anchor_para(&self.session, doc, r, i)).unwrap_or(&fallback);
        let total = self.session.reviews[r].items.len();
        let number = self.number(doc, r, i);
        let words = |id| line_words(item, doc, id);
        // Once decided, the heading is what was chosen, not what is highlighted.
        let shown = match status {
            Status::Accepted(k) => k,
            _ => hi,
        };
        let headings: Vec<String> =
            (0..item.option_lines.len()).map(|k| blocks::heading(item.shape(k), words)).collect();
        let title = headings.get(shown).cloned().unwrap_or_default();
        body.push(
            div()
                .flex()
                .flex_col()
                .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(INK).child(title))
                .child(small(format!(
                    "{} \u{b7} change {number} of {total}",
                    if ctx.scene.is_empty() { "Before the first scene" } else { &ctx.scene },
                )))
                .into_any_element(),
        );
        if item.fuzzy {
            body.push(warn_box("Matched approximately: check these are the right lines.", None));
        }
        let removed_by = self.session.removed_by(doc, r, i);
        let outdated = self.session.is_outdated(doc, r, i);
        let cut_option = matches!(status, Status::Accepted(k) if item.option_lines[k].is_empty());
        if let Some(text) = block_notice(status, removed_by, outdated, run.is_empty() && !cut_option) {
            body.push(warn_box(&text, None));
        }

        let chip = match status {
            Status::Pending | Status::Removed => None,
            Status::Accepted(k) if item.option_lines.len() > 1 => Some(format!("accepted option {}", k + 1)),
            Status::Accepted(_) => Some("accepted".into()),
            Status::Rejected => Some("kept yours".into()),
            Status::Rewritten if run.is_empty() => Some("lines cut".into()),
            Status::Rewritten => Some("rewritten".into()),
        };
        if chip.is_some() {
            body.push(label_with("Yours now", chip));
            let mine: Vec<CardLine> = self
                .session
                .mine(doc, r, i)
                .iter()
                .map(|l| CardLine {
                    kind: l.kind.clone().unwrap_or(scriptorium_core::ElementKind::Action),
                    text: l.text.clone(),
                    side: Side::Same,
                    retyped_from: None,
                })
                .collect();
            let lines = if mine.is_empty() {
                vec![small("(no lines: cut)".to_string()).into_any_element()]
            } else {
                self.card_lines(&mine, INK_2)
            };
            body.push(div().border_l_2().border_color(RULE).pl_2().children(lines).into_any_element());
        }

        let multi = item.option_lines.len() > 1;
        body.push(label(if multi { format!("{} options", item.option_lines.len()) } else { "Suggested".into() }));
        for (k, o) in item.option_lines.iter().enumerate() {
            let is_hi = k == hi;
            let rows = blocks::card_lines(&item.before_lines, o);
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
                            .when(multi, |d| d.child(small(headings[k].clone())))
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
                    .children(self.card_lines(&rows, INK))
                    .into_any_element(),
            );
        }
    }

    /// Lines of a block on the card, each placed and cased as on the page
    /// (scaled to the card), struck in red or green by [`Side`]; unchanged
    /// lines in `ink`.
    fn card_lines(&self, rows: &[CardLine], ink: gpui::Rgba) -> Vec<AnyElement> {
        rows.iter()
            .enumerate()
            .map(|(n, row)| {
                let (left, width, align) = blocks::card_indent(&row.kind);
                let caps = self.fdx.is_caps(row.kind.name()) || layout::geometry(&row.kind).caps;
                let gap = n > 0 && layout::geometry(&row.kind).space_before >= 1.0;
                let text = if caps { layout::display_caps(&row.text) } else { row.text.clone() };
                let line = div()
                    .w(relative(width))
                    .font_family(SCRIPT_FONT)
                    .text_size(px(13.))
                    .line_height(px(18.))
                    .when(align == layout::Align::Right, |d| d.text_right())
                    .when(align == layout::Align::Center, |d| d.text_center())
                    .map(|d| match row.side {
                        Side::Same => d.text_color(ink),
                        Side::Del => d.text_color(STRUCK).line_through(),
                        // Not by colour alone: a bar on the left as well.
                        Side::Ins => d.text_color(ADDED).bg(INS).border_l_2().border_color(ADDED).pl_1(),
                    })
                    .child(text);
                let tag = row.retyped_from.as_ref().map(|from| {
                    div().text_size(px(10.5)).text_color(INK_2).child(format!(
                        "{} \u{2192} {}",
                        from.name(),
                        row.kind.name()
                    ))
                });
                div()
                    .w_full()
                    .pl(relative(left))
                    .when(gap, |d| d.mt(px(6.)))
                    .child(line)
                    .children(tag)
                    .into_any_element()
            })
            .collect()
    }

    /// The card's lower part, the same for lines and blocks: why, the
    /// buttons, back to undecided, and the focus hint.
    fn card_footer(
        &self,
        body: &mut Vec<AnyElement>,
        r: usize,
        i: usize,
        status: Status,
        focused: bool,
        cx: &mut Context<Self>,
    ) {
        let item = self.session.item(r, i);
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
        if !matches!(status, Status::Pending | Status::Removed) {
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
                    .to_string()
            }
            View::Script => {
                left = left.child(format!("Script · {} paragraphs", self.doc(cx).paragraphs().len()));
                format!(
                    "Enter new line · Tab line type · {} undo · {} write .fdx",
                    crate::menus::shortcut("z"),
                    crate::menus::shortcut("s")
                )
            }
        };
        let said: Vec<&str> =
            [self.error.as_ref(), self.notice.as_ref()].into_iter().filter(|s| !s.is_empty()).collect();
        let right: SharedString = if said.is_empty() { hints.into() } else { said.join("  \u{b7}  ").into() };
        // Not cleared by the writer's keys: it goes when the file is usable again.
        let stuck = self.stuck.as_ref().map(|why| {
            div()
                .flex_none()
                .px_2()
                .rounded_sm()
                .bg(WARN_SOFT)
                .text_color(WARN)
                .font_weight(FontWeight::SEMIBOLD)
                .child(format!(
                    "The session file was changed by another program and can't be read ({why}). Close the other program or move {} aside, then the app resumes saving.",
                    file_name(&self.session_path)
                ))
        });
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
            .children(stuck)
            .child(div().flex_1())
            .child(
                div()
                    .when(!said.is_empty(), |d| d.text_color(INK).font_weight(FontWeight::SEMIBOLD))
                    .when(!self.error.is_empty(), |d| d.text_color(WARN))
                    .child(right),
            )
    }
}

/// Indexes into `items` in script order (by their numbers).
/// Paragraph `para`'s scene and speaker, by its live position. None if it's
/// gone (or the outline hasn't caught up with it yet).
fn para_ctx<'a>(ctx: &'a [ParaContext], doc: &Document, para: ParaId) -> Option<&'a ParaContext> {
    ctx.get(doc.index_of(para)?)
}

fn in_script_order(doc: &Document, items: &[scriptorium_review::Item]) -> Vec<usize> {
    let numbers = item_numbers(doc, items);
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| numbers[i]);
    order
}

/// The review and item whose decision is metadata `key`.
fn item_with_key(session: &Session, key: &str) -> Option<(usize, usize)> {
    session.reviews.iter().enumerate().find_map(|(r, review)| {
        let prefix = format!("review/{}/", review.id);
        let entry: usize = key.strip_prefix(&prefix)?.parse().ok()?;
        review.items.iter().position(|it| it.entry == entry).map(|i| (r, i))
    })
}

/// How many of the writer's changes `changes` are: a suggestion's line and
/// its decision count once; another line counts once.
fn count_changes(session: &Session, changes: &[Change]) -> usize {
    let mut seen: HashSet<String> = HashSet::new();
    for c in changes {
        let id = match c {
            Change::Meta(key) => match item_with_key(session, key) {
                Some((r, i)) => format!("p{}", session.item(r, i).para.0),
                None => format!("k{key}"),
            },
            Change::Para(p, _) | Change::Removed(p, _) => format!("p{}", p.0),
        };
        seen.insert(id);
    }
    seen.len()
}

/// How the app tells the writer to rewrite a line it can't decide
/// (see [`scriptorium_review::Refusal::message`]).
const REWRITE_HINT: &str = "press E to rewrite this line";

/// How the app tells the writer to rewrite a block it can't accept.
/// Ends with [`COMMIT_KEY`].
const BLOCK_HINT: &str = if cfg!(target_os = "macos") {
    "edit the lines in the script, then press Cmd+Enter"
} else {
    "edit the lines in the script, then press Ctrl+Enter"
};

fn hint_for(item: &scriptorium_review::Item) -> &'static str {
    if item.block { BLOCK_HINT } else { REWRITE_HINT }
}

/// How the app names line `id` of `item`: the first words of its live text,
/// else of the text it had when the suggestion was matched.
fn line_words(item: &scriptorium_review::Item, doc: &Document, id: ParaId) -> String {
    let text =
        doc.paragraph(id).map(|p| p.text()).or_else(|| item.before_paras.iter().find(|p| p.id == id).map(|p| p.text()));
    match text {
        Some(t) => blocks::quote(&t),
        None => "a line no longer in the script".into(),
    }
}

/// [`Refusal::message`] in the app's words: lines named by `words` (their
/// first words) rather than paragraph numbers, and lines partly gone named
/// by the review that removed them (`removed_by`).
fn refusal_text(refusal: &Refusal, hint: &str, words: impl Fn(ParaId) -> String, removed_by: Option<&str>) -> String {
    let since = "since the suggestion was written";
    match refusal {
        Refusal::NoOption { .. } => refusal.message(hint),
        Refusal::LineRemoved { para } => format!(
            "Line removed: {} was removed (or joined into the line above) {since}, so this can't be decided",
            words(*para)
        ),
        Refusal::LineChanged { para, how } => {
            let how = if *how == LineChange::Split { "split" } else { "joined with the next line" };
            format!("Line changed: {} was {how} {since}, so its text no longer fits the line; {hint}", words(*para))
        }
        Refusal::Restructured { para } => match removed_by {
            Some(review) => format!(
                "Lines removed by {review}: some of the lines from {} are gone, so this can't be accepted; {hint}",
                words(*para)
            ),
            None => format!(
                "Lines changed: the lines from {} were split, partly removed or moved apart {since}, so this can't \
                 be accepted; {hint}",
                words(*para)
            ),
        },
        Refusal::RemovedBy { para, review } => {
            format!("Line removed by {review}: {} is gone, so this can't be decided", words(*para))
        }
        Refusal::DecidedElsewhere { para, review } => {
            format!("{} is decided in {review}; undecide it there first", words(*para))
        }
    }
}

/// The lines item `i` of review `r` puts its dot on (and the card's
/// highlight): the live lines it covers ([`Session::live_run`]); for a
/// block with none (a cut, or lines removed), the line before where they
/// were ([`Session::current_run`]). Empty for a line suggestion whose line is gone.
fn dot_paras(session: &Session, doc: &Document, r: usize, i: usize) -> Vec<ParaId> {
    let run = session.live_run(doc, r, i);
    let item = session.item(r, i);
    if !run.is_empty() || !item.block {
        return run;
    }
    // A cut (or lines gone): the line before where they were, or the first
    // line if that was the start of the script.
    let at = session.current_run(doc, r, i).after.or_else(|| doc.paragraphs().first().map(|p| p.id));
    at.into_iter().collect()
}

/// The line to show item `i` of review `r` at: its first dot, else its
/// first line (gone).
fn anchor_para(session: &Session, doc: &Document, r: usize, i: usize) -> ParaId {
    dot_paras(session, doc, r, i).first().copied().unwrap_or_else(|| session.item(r, i).para)
}

/// Which item of review `r` each line belongs to, for clicks and undo: a
/// line a suggestion covers wins over a cut's position on it.
fn items_by_para(session: &Session, doc: &Document, r: usize) -> HashMap<ParaId, usize> {
    let mut map = HashMap::new();
    let n = session.reviews[r].items.len();
    let (mut covering, mut positions) = (vec![], vec![]);
    for i in 0..n {
        let run = session.live_run(doc, r, i);
        if run.is_empty() {
            positions.extend(dot_paras(session, doc, r, i).into_iter().map(|p| (p, i)));
        } else {
            covering.extend(run.into_iter().map(|p| (p, i)));
        }
    }
    map.extend(positions);
    map.extend(covering);
    map
}

/// The sidebar's text for item `i`: its first live line, else what it was
/// made on, after "Cut:" (a cut accepted) or "Lines gone:" (removed some
/// other way; naming the review whose decision removed them, if one did).
fn item_text(session: &Session, doc: &Document, r: usize, i: usize) -> String {
    let item = session.item(r, i);
    if let Some(p) = session.live_run(doc, r, i).first().and_then(|p| doc.paragraph(*p)) {
        return p.text();
    }
    let Some(l) = item.before_lines.first().filter(|_| item.block) else {
        return doc.paragraph(item.para).map(|p| p.text()).unwrap_or_default();
    };
    match session.status(doc, r, i) {
        Status::Accepted(k) if item.option_lines.get(k).is_some_and(|o| o.is_empty()) => format!("Cut: {}", l.text),
        _ => match session.removed_by(doc, r, i) {
            Some(review) => format!("Lines gone (cut by {review}): {}", l.text),
            None => format!("Lines gone: {}", l.text),
        },
    }
}

/// The card's warning for a block, if any: lines gone while pending (and
/// which review cut them), lines cut by hand after deciding, or lines
/// changed since the suggestion was written.
fn block_notice(status: Status, removed_by: Option<&str>, outdated: bool, gone: bool) -> Option<String> {
    match status {
        Status::Removed => Some(match removed_by {
            Some(review) => format!("Lines cut by {review}, so this can't be decided."),
            None => "These lines have been removed, so the suggestion can't be decided.".into(),
        }),
        _ if gone && status != Status::Pending => {
            Some("Lines cut: these lines were removed after this was decided.".into())
        }
        Status::Pending if outdated => Some("These lines have changed since the suggestion was written.".into()),
        _ => None,
    }
}

fn mark_of(session: &Session, doc: &Document, r: usize, i: usize) -> Mark {
    match session.status(doc, r, i) {
        Status::Pending if session.is_outdated(doc, r, i) => Mark::Outdated,
        Status::Pending => Mark::Pending,
        Status::Accepted(_) => Mark::Accepted,
        Status::Rejected => Mark::Kept,
        Status::Rewritten => Mark::Rewritten,
        Status::Removed => Mark::Outdated,
    }
}

/// The card's warning about how the line changed since the suggestion was
/// written, if it did (structurally, or by hand while pending).
fn card_change_notice(change: Option<LineChange>, status: Status, outdated: bool) -> Option<&'static str> {
    match change {
        Some(LineChange::Removed) if status == Status::Removed => {
            Some("This line has been removed, so the suggestion can't be decided.")
        }
        Some(LineChange::Removed) => Some("Line cut: this line was removed after it was decided."),
        Some(LineChange::Split) => Some(
            "The line was split since the suggestion was written, so it can't be accepted: press E to rewrite this line.",
        ),
        Some(LineChange::Joined) => Some(
            "The line was joined with the next since the suggestion was written, so it can't be accepted: press E to \
             rewrite this line.",
        ),
        _ if outdated => Some("This line has changed since the suggestion was written."),
        _ => None,
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

pub fn button(id: impl Into<gpui::ElementId>, text: &str, key: Option<&str>, primary: bool) -> Stateful<Div> {
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

impl Focusable for ScriptView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ScriptView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let card = self.review().map(|_| self.render_card(window, cx));
        div()
            .key_context("ScriptView")
            .size_full()
            .flex()
            .flex_col()
            .bg(CHROME)
            .text_color(INK)
            .text_size(px(13.))
            .on_action(cx.listener(Self::apply_to_fdx))
            .on_action(cx.listener(Self::on_undo_any))
            .on_action(cx.listener(Self::on_redo_any))
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

#[cfg(test)]
mod tests {
    use super::*;
    use scriptorium_core::{ElementKind, Pos, Selection};

    fn doc() -> Document {
        Document::new(Script {
            paragraphs: vec![
                Paragraph::plain(ParaId(0), ElementKind::SceneHeading, "INT. HOUSE - DAY"),
                Paragraph::plain(ParaId(1), ElementKind::Character, "ANA"),
                Paragraph::plain(ParaId(2), ElementKind::Dialogue, "Hi."),
            ],
            ..Script::default()
        })
    }

    #[test]
    fn typing_keeps_the_structure_key_and_splits_change_it() {
        let mut d = doc();
        let key = structure_key(d.script());
        d.insert_text(Selection::cursor(Pos::new(ParaId(2), 3)), " there").unwrap();
        assert_eq!(structure_key(d.script()), key);
        d.enter(Selection::cursor(Pos::new(ParaId(2), 2))).unwrap();
        assert_ne!(structure_key(d.script()), key);
    }

    #[test]
    fn context_is_looked_up_by_live_position() {
        let mut d = doc();
        // A paragraph inserted above moves the dialogue down: an as-opened
        // index would now point at the wrong line (or past the end).
        d.enter(Selection::cursor(Pos::new(ParaId(1), 0))).unwrap();
        assert_eq!(d.index_of(ParaId(2)), Some(3));
        let ctx = annotate(d.script());
        assert_eq!(para_ctx(&ctx, &d, ParaId(2)).unwrap().speaker, "ANA");
        assert!(para_ctx(&ctx, &d, ParaId(99)).is_none());
        // A stale, shorter outline gives None rather than panicking.
        assert!(para_ctx(&ctx[..1], &d, ParaId(2)).is_none());
        assert_eq!(scene_rows(d.script(), &ctx).len(), 1);
    }

    /// The card names a split or joined line's way out as the app's own key,
    /// and a decided line that is gone as "line cut".
    #[test]
    fn change_notices_use_the_apps_words() {
        for c in [LineChange::Split, LineChange::Joined] {
            let text = card_change_notice(Some(c), Status::Pending, true).unwrap();
            assert!(text.ends_with("press E to rewrite this line."), "{text}");
        }
        let cut = card_change_notice(Some(LineChange::Removed), Status::Rewritten, false).unwrap();
        assert!(cut.starts_with("Line cut"), "{cut}");
        let refusal = scriptorium_review::Refusal::LineChanged { para: ParaId(2), how: LineChange::Split };
        assert!(refusal.message(REWRITE_HINT).ends_with("; press E to rewrite this line"));
    }

    /// A block's dots: on each line it covers while pending; once a cut is
    /// accepted, on the line before where it was, without taking that line
    /// from the suggestion that covers it.
    #[test]
    fn block_dots_follow_the_lines_and_a_cut_sits_on_the_line_before() {
        let d = doc();
        let text = "SCRIPT REVIEW v2\n---\nPARA: 1, 2\nBEFORE:\n  CHARACTER: ANA\n  DIALOGUE: Hi.\nAFTER: [cut]\n";
        let review = Review::open("r", text, d.script(), |_| false, "");
        assert_eq!(review.items.len(), 1, "{:?}", review.unmatched);
        let session = Session::new(vec![review]);
        let mut d = d;
        assert_eq!(dot_paras(&session, &d, 0, 0), vec![ParaId(1), ParaId(2)]);
        assert!(session.item(0, 0).block);
        let sel = Selection::cursor(d.start());
        session.decide(&mut d, sel, 0, 0, Decision::Accept(0)).unwrap();
        assert_eq!(session.status(&d, 0, 0), Status::Accepted(0));
        assert_eq!(dot_paras(&session, &d, 0, 0), vec![ParaId(0)]);
        assert_eq!(items_by_para(&session, &d, 0).get(&ParaId(0)), Some(&0));
        assert_eq!(item_text(&session, &d, 0, 0), "Cut: ANA");
    }

    #[test]
    fn block_notices_name_who_cut_the_lines() {
        let by = block_notice(Status::Removed, Some("notes.review"), false, true).unwrap();
        assert_eq!(by, "Lines cut by notes.review, so this can't be decided.");
        assert!(block_notice(Status::Rewritten, None, false, true).unwrap().starts_with("Lines cut"));
        assert!(block_notice(Status::Accepted(0), None, false, false).is_none());
        assert!(block_notice(Status::Pending, None, true, false).unwrap().contains("changed"));
        assert!(BLOCK_HINT.ends_with(&format!("press {COMMIT_KEY}")));
        let refusal = Refusal::Restructured { para: ParaId(1) };
        assert!(refusal.message(BLOCK_HINT).ends_with(COMMIT_KEY));
    }

    /// Refusals name lines by their first words, never by paragraph number,
    /// and lines partly gone by the review that removed them.
    #[test]
    fn refusals_quote_lines_and_name_reviews() {
        let words = |id: ParaId| blocks::quote(if id == ParaId(1) { "Grandma laughs." } else { "?" });
        let all = [
            Refusal::LineRemoved { para: ParaId(1) },
            Refusal::LineChanged { para: ParaId(1), how: LineChange::Split },
            Refusal::Restructured { para: ParaId(1) },
            Refusal::RemovedBy { para: ParaId(1), review: "notes.review".into() },
            Refusal::DecidedElsewhere { para: ParaId(1), review: "notes.review".into() },
        ];
        for r in &all {
            let text = refusal_text(r, BLOCK_HINT, words, None);
            assert!(text.contains("\u{201c}Grandma laughs.\u{201d}"), "{text}");
            assert!(!text.contains('\u{b6}') && !text.contains("review notes"), "{text}");
        }
        let generic = refusal_text(&all[2], BLOCK_HINT, words, None);
        assert!(generic.contains("split, partly removed or moved apart"), "{generic}");
        let named = refusal_text(&all[2], BLOCK_HINT, words, Some("notes.review"));
        assert!(named.starts_with("Lines removed by notes.review:"), "{named}");
        assert!(named.ends_with(COMMIT_KEY), "{named}");
        let elsewhere = refusal_text(&all[4], BLOCK_HINT, words, None);
        assert_eq!(elsewhere, "\u{201c}Grandma laughs.\u{201d} is decided in notes.review; undecide it there first");
    }

    /// Lines another review's decision removed read "Lines gone", naming
    /// that review, not "Cut:" (which is for a cut accepted).
    #[test]
    fn lines_gone_name_the_review_that_cut_them() {
        let mut d = doc();
        let cut =
            "SCRIPT REVIEW v2\nId: a\n---\nPARA: 1, 2\nBEFORE:\n  CHARACTER: ANA\n  DIALOGUE: Hi.\nAFTER: [cut]\n";
        let change = "SCRIPT REVIEW v2\nId: b\n---\nPARA: 1, 2\nBEFORE:\n  CHARACTER: ANA\n  DIALOGUE: Hi.\n\
                      AFTER:\n  CHARACTER: ANA\n  DIALOGUE: Hello.\n";
        let a = Review::open("a.review", cut, d.script(), |_| false, "");
        let b = Review::open("b.review", change, d.script(), |_| false, "");
        let session = Session::new(vec![a, b]);
        let sel = Selection::cursor(d.start());
        session.decide(&mut d, sel, 0, 0, Decision::Accept(0)).unwrap();
        assert_eq!(item_text(&session, &d, 0, 0), "Cut: ANA");
        assert_eq!(session.status(&d, 1, 0), Status::Removed);
        assert_eq!(item_text(&session, &d, 1, 0), "Lines gone (cut by a.review): ANA");
    }

    /// A block whose lines another review cut is reported once on reopen,
    /// not also as "didn't match".
    #[test]
    fn a_block_cut_elsewhere_is_reported_once() {
        let d = doc();
        let text = "SCRIPT REVIEW v2\nId: b\n---\nPARA: 1\nBEFORE:\n  CHARACTER: NOBODY\nAFTER:\n  CHARACTER: ANA\n";
        let a = Review::open("a.review", "SCRIPT REVIEW v2\nId: a\n---\n", d.script(), |_| false, "");
        let mut b = Review::open("b.review", text, d.script(), |_| false, "");
        assert_eq!(b.unmatched.len(), 1);
        b.unmatched[0].why = Some(scriptorium_review::Unplaced::RemovedBy { review: 0 });
        let session = Session::new(vec![a, b]);
        let notes = review_notes(&session, 1);
        assert_eq!(notes, ["b.review: a suggestion's lines cut by a.review."], "{notes:?}");
    }
}
