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

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, HighlightStyle,
    MouseButton, ScrollStrategy, SharedString, Stateful, StyledText, Subscription, Task, UniformListScrollHandle,
    Window, actions, div, prelude::*, px, relative, uniform_list,
};
use scriptorium_core::diff::{WordOp, word_diff};
use scriptorium_core::doc::Event;
use scriptorium_core::outline::{ParaContext, annotate};
use scriptorium_core::{Document, ElementKind, ParaId, Paragraph, Script};
use scriptorium_fdx::FdxFile;
use scriptorium_fsio::versions::{self, InPlaceError, PairState};
use scriptorium_review::file::RowRef;
use scriptorium_review::sessionfile::{self, Change, Disk, Resumed, Seen, SessionFile};
use scriptorium_review::{Decision, LineChange, Refusal, Review, Session, Status};
use scriptorium_tools::item_numbers;
use scriptorium_versions::links::Links;

use crate::agent_panel::{self, AgentPanel, AgentPanelEvent};
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
/// Written as the menus write shortcuts (`menus::shortcut`): ⌘ on macOS.
const COMMIT_KEY: &str = if cfg!(target_os = "macos") { "⌘Return" } else { "Ctrl+Enter" };

/// Everything read from disk to open a script.
pub struct Opened {
    pub script_path: PathBuf,
    raw: String,
    fdx: FdxFile,
    opened: sessionfile::ResumedSession,
    /// The script's pair of versions, loaded (re-found first) before the
    /// session was resumed.
    pair: PairState,
    session_path: PathBuf,
    /// The script's folder, canonical: review paths are stored relative to it.
    dir: PathBuf,
    /// The session file as read (to notice another program changing it).
    seen: Seen,
    /// Review files attached now that the session file doesn't list yet.
    unsynced: Vec<PathBuf>,
    notices: Vec<String>,
}

// This mirrors `scriptorium_fsio::opened::Opened` rather than using it: the app sets a
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
        let (mut opened, pair, mut seen, mut notices) =
            resume_from_disk(&script_path, &fdx, &session_path, review_paths);
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
            Resumed::Resumed { .. } => notices.insert(0, RESUMED.into()),
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
        notices.extend(pair_note(&pair));
        Ok(Self { script_path, raw, fdx, opened, pair, session_path, dir, seen, unsynced, notices })
    }
}

/// Resume the session saved next to the script with the review files it
/// attaches plus `review_paths` (as opening does, and again when another
/// program changed the files), through the one open path every host uses
/// (`scriptorium_fsio::versions::resume`): the script's pair is loaded
/// first, re-found if a member changed (the links file is the only thing
/// written, under the links lock, released before this returns), and `ROW:`
/// suggestions are placed through its resolver. Returns the resumed session
/// (its `notes` not yet shown), the pair, the session file as read, and
/// notices about review files and the pair.
fn resume_from_disk(
    script_path: &Path,
    fdx: &FdxFile,
    session_path: &Path,
    review_paths: &[PathBuf],
) -> (sessionfile::ResumedSession, PairState, Seen, Vec<String>) {
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
    // One try, no waiting (this is the UI thread): a busy links lock or a
    // member mid-save leaves the pair unusable until a later tick.
    let (opened, pair) = versions::resume_with(
        fdx,
        &dir,
        &script_name,
        sources,
        saved.as_deref(),
        versions::Patience::ONCE,
        &mut notices,
    );
    for r in 0..opened.session.reviews.len() {
        notices.append(&mut review_notes(&opened.session, r));
    }
    (opened, pair, seen, notices)
}

/// The status bar's note on opening the original of a pair: its rows are
/// translated (or, two files in one language, reviewed) in the version.
fn pair_note(pair: &PairState) -> Option<String> {
    let p = pair.loaded()?;
    if p.this_is_version() {
        return None;
    }
    let version = &p.links.members[p.links.sides().1].file;
    let what = if p.links.same_language() { "review it" } else { "translate" };
    Some(format!("Lined up with {version}: open that file to {what}."))
}

/// A file's modification time and length: looked at every tick, before
/// reading it again.
type FileSig = Option<(std::time::SystemTime, u64)>;

fn file_sig(path: &Path) -> FileSig {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// The `*.links.txt` files in `dir`, by name.
fn links_files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| n.to_lowercase().ends_with(".links.txt"))
        .collect();
    names.sort();
    names
}

/// How long after a pair couldn't be used to try loading it again: a links
/// lock held a moment, or Final Draft halfway through saving the other
/// file. Then only a change to its files loads it again.
const PAIR_RETRIES: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
];

/// What the app looks at every tick to know when to load the script's pair
/// again: loading may re-find and write the links file, so it isn't done on
/// every tick, only when
/// - a file of the pair changed: the links file or the other member (time
///   and length first, then, only then, its bytes against the hashes the
///   pair was loaded from);
/// - without a pair, a `*.links.txt` file came or went in the folder (a
///   version made by the CLI or Claude while the original is open);
/// - a pair that couldn't be used is due another try ([`PAIR_RETRIES`]).
#[derive(Debug, Default)]
struct PairWatch {
    files: Vec<Watched>,
    /// Without a pair: the folder and its links files when loaded.
    folder: Option<(PathBuf, Vec<String>)>,
    /// An unusable pair: when to try again, and the tries left after that.
    retry: Option<(Instant, usize)>,
}

#[derive(Debug)]
struct Watched {
    path: PathBuf,
    /// `sessionfile::hash` of the bytes the pair was loaded from; `None`:
    /// any change counts (a pair that couldn't be used).
    hash: Option<String>,
    /// The file's [`FileSig`] when its bytes were last found to be `hash`;
    /// `None` until then.
    seen: Option<FileSig>,
}

impl PairWatch {
    /// What to look at for `pair`, just loaded for the script `script_name`
    /// in `dir`.
    fn of(pair: &PairState, dir: &Path, script_name: &str) -> Self {
        let any_change = |path: PathBuf| Watched { seen: Some(file_sig(&path)), path, hash: None };
        match pair {
            PairState::None => Self { folder: Some((dir.to_path_buf(), links_files(dir))), ..Self::default() },
            PairState::Loaded(p) => Self {
                files: vec![
                    Watched { path: p.path.clone(), hash: Some(p.links_hash.clone()), seen: None },
                    Watched { path: p.other_path.clone(), hash: Some(p.other_hash.clone()), seen: None },
                ],
                ..Self::default()
            },
            PairState::Unusable { links, .. } => {
                let mut files = vec![];
                if let Some(l) = links {
                    // The other member too: Final Draft finishing its save
                    // makes the pair usable.
                    let other = std::fs::read_to_string(l)
                        .ok()
                        .and_then(|t| scriptorium_versions::links::parse(&t).ok())
                        .and_then(|x| {
                            x.member_of(script_name).map(|m| x.members[if m == 0 { 1 } else { 0 }].file.clone())
                        });
                    files.push(any_change(l.clone()));
                    files.extend(other.map(|o| any_change(dir.join(o))));
                }
                let retry = Some((Instant::now() + PAIR_RETRIES[0], 1));
                Self { files, folder: None, retry }
            }
        }
    }

    /// Whether to load the pair again now (see [`PairWatch`]).
    fn changed(&mut self) -> bool {
        if let Some((at, n)) = self.retry
            && Instant::now() >= at
        {
            self.retry = PAIR_RETRIES.get(n).map(|d| (Instant::now() + *d, n + 1));
            return true;
        }
        if let Some((dir, names)) = &self.folder {
            return links_files(dir) != *names;
        }
        for w in &mut self.files {
            // Before reading: a write after this shows as a newer time later.
            let sig = file_sig(&w.path);
            if w.seen == Some(sig) {
                continue;
            }
            let Some(hash) = &w.hash else { return true };
            match std::fs::read_to_string(&w.path) {
                Ok(text) if sessionfile::hash(&text) == *hash => w.seen = Some(sig),
                _ => return true,
            }
        }
        false
    }
}

/// On the version of a loaded pair: for each of its lines, the original's
/// line(s) of its row, shown dimmed while the line is blank (what is left to
/// translate reads in place). Rows of one version line and several original
/// lines show them all; several version lines pair with the original's in
/// order where the counts agree, else the first takes them all.
fn pair_ghosts(pair: &PairState) -> HashMap<ParaId, String> {
    let Some(p) = pair.loaded().filter(|p| p.this_is_version()) else { return HashMap::new() };
    let (o, v) = p.links.sides();
    let text = |id: &ParaId| p.other_script.paragraphs.iter().find(|q| q.id == *id).map(|q| q.text());
    let by_id: HashMap<ParaId, String> =
        p.links.rows.iter().flat_map(|row| row.cells[o].iter()).filter_map(|id| text(id).map(|t| (*id, t))).collect();
    let mut out = HashMap::new();
    for row in &p.links.rows {
        let (orig, ver) = (&row.cells[o], &row.cells[v]);
        let texts: Vec<&str> = orig.iter().filter_map(|id| by_id.get(id).map(String::as_str)).collect();
        if ver.len() == texts.len() {
            out.extend(ver.iter().zip(&texts).map(|(id, t)| (*id, t.to_string())));
        } else if let Some(first) = ver.first() {
            out.insert(*first, texts.join(" "));
        }
    }
    out.retain(|_, t| !t.trim().is_empty());
    out
}

/// What the card shows above a `ROW:` suggestion: the original's line(s) it
/// was made for (`ROW TEXT`, which the original's file still reads: an
/// entry whose original line changed isn't placed, so it has no card).
#[derive(Debug, Clone, PartialEq, Eq)]
struct RowCard {
    /// The original's label ("English", or its file stem).
    label: String,
    /// The version's label ("Español").
    version: String,
    /// `ROW TEXT`, one line per paragraph.
    text: String,
    /// What the card says for the version's line while it is blank.
    blank: &'static str,
}

/// The card's [`RowCard`] for `row` (an item's `ROW:`) in the pair `links`.
fn row_card(links: &Links, row: &RowRef) -> RowCard {
    let (o, v) = links.sides();
    let blank = if links.same_language() { "empty" } else { "empty \u{2014} not yet translated" };
    RowCard { label: links.label(o), version: links.label(v), text: row.texts.join("\n"), blank }
}

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Read one review file, to open with the script.
fn read_review(path: &Path, dir: &Path) -> Result<sessionfile::ReviewSource, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    scriptorium_review::file::refuse_links(&text).map_err(|e| format!("{}: {e}", file_name(path)))?;
    Ok(sessionfile::ReviewSource { name: review_title(path), path: sessionfile::stored_path(dir, path), text })
}

/// What to tell the user about a review just opened.
fn review_notes(session: &Session, r: usize) -> Vec<String> {
    let review = &session.reviews[r];
    let mut notes: Vec<String> = review.warnings.iter().map(|w| format!("{}: {w}", review.name)).collect();
    // Blocks whose lines another review cut are named (they can't be
    // placed), and not counted again as not matching.
    let mut n = 0;
    // Suggestions on rows of a pair not placed, by reason, in order.
    let mut rows: Vec<(&str, usize)> = vec![];
    for u in &review.unmatched {
        match &u.why {
            Some(why @ scriptorium_review::Unplaced::RemovedBy { .. }) => {
                notes.push(format!("{}: a suggestion's {}.", review.name, session.unplaced_label(why)));
            }
            Some(scriptorium_review::Unplaced::NoRow(reason)) => match rows.iter_mut().find(|(r, _)| r == reason) {
                Some((_, k)) => *k += 1,
                None => rows.push((reason, 1)),
            },
            _ => n += 1,
        }
    }
    for (reason, k) in rows {
        notes.push(format!("{}: {}", review.name, row_note(reason, k)));
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

/// What the status bar says about `n` suggestions on rows of a pair not
/// shown, for one reason (the resolver's sentence), in plain words.
fn row_note(reason: &str, n: usize) -> String {
    let changed = reason.strip_prefix("the ").and_then(|r| r.strip_suffix(" line changed since this suggestion"));
    match changed {
        Some(label) => {
            let a = if label.starts_with(['A', 'E', 'I', 'O', 'U', 'a', 'e', 'i', 'o', 'u']) { "an" } else { "a" };
            if n == 1 {
                format!(
                    "1 suggestion is for {a} {label} line that changed since: ask Claude to translate that row again."
                )
            } else {
                format!(
                    "{n} suggestions are for {label} lines that changed since: ask Claude to translate those rows again."
                )
            }
        }
        None if n == 1 => format!("1 suggestion isn't shown: {reason}."),
        None => format!("{n} suggestions aren't shown: {reason}."),
    }
}

pub fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

pub fn has_extension(p: &Path, ext: &str) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// "PURPLE LIST.review.txt" → "PURPLE LIST review"; a name that already
/// says what it is keeps it: "Translation from English.review.txt" →
/// "Translation from English" (the agent's translation review).
fn review_title(p: &Path) -> String {
    let name = file_name(p);
    let name = name.strip_suffix(".txt").unwrap_or(&name);
    if let Some(stem) = name.strip_suffix(".review")
        && stem.starts_with("Translation from ")
    {
        return stem.to_string();
    }
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

/// A rail label's tooltip: the view's name.
struct RailTip(SharedString);

impl Render for RailTip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(PAPER)
            .border_1()
            .border_color(RULE)
            .text_size(px(12.))
            .text_color(INK)
            .child(self.0.clone())
    }
}

/// The session lock as [`ScriptView::try_lock_session`] hands it out:
/// shared while anything here holds it; released when the last one drops.
/// The inner None: the OS can't lock here (logged; the app goes on unlocked).
pub(crate) type SessionLease = Rc<Option<scriptorium_fsio::SessionLock>>;

/// Why the live store can't do what a tool call asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LiveError {
    /// Busy: another program holds the lock, or changed the files while the
    /// writer is rewriting a line. The tool result says to try again.
    Busy,
    /// The reason, for the tool result.
    Failed(String),
}

/// Emitted when Write .fdx wrote the version in place (or got as far as
/// its `.fdx`): the window drops this view without saving it and opens
/// `path`, saying `notice` (plan D4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reopen {
    pub path: PathBuf,
    pub notice: String,
}

impl EventEmitter<Reopen> for ScriptView {}

pub struct ScriptView {
    editor: Entity<ScriptEditor>,
    session: Rc<Session>,
    /// The events that made the document's starting point (kept in the session file).
    base: Vec<Event>,
    script_path: PathBuf,
    dir: PathBuf,
    raw: Rc<str>,
    fdx: Rc<FdxFile>,
    /// The script's pair of versions as last loaded: its resolver places
    /// `ROW:` suggestions whenever a review is opened here.
    pair: Rc<PairState>,
    /// The pair's files as loaded, looked at every tick ([`PairWatch`]).
    pair_watch: PairWatch,
    /// [`pair_ghosts`] of the pair as loaded.
    ghosts: Rc<HashMap<ParaId, String>>,
    /// Load the pair again on the next tick (a version was just made).
    recheck_pair: bool,
    /// Written in place and being replaced by the written file
    /// ([`Reopen`]): never saves, looks at files or writes again.
    retired: bool,
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
    /// The status bar's message: it stays until replaced or the writer's
    /// next action, except as `notice_life` says.
    notice: SharedString,
    /// When `notice` was set, and how long it stays.
    notice_at: Instant,
    notice_life: NoticeLife,
    /// Clears the notice set at that time once [`NOTICE_FOR`] is up.
    notice_timer: Option<(Instant, Task<()>)>,
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
    /// The session lock while something here holds it (see
    /// [`Self::lock_session`]): taken again, it is shared, never re-locked.
    held_lock: RefCell<Weak<Option<scriptorium_fsio::SessionLock>>>,
    /// The agent panel, once it has been opened (kept while closed, so a
    /// draft or a run survives Ctrl+L), and whether it shows.
    agent: Option<(Entity<AgentPanel>, Subscription)>,
    agent_open: bool,
    /// Looks at the files every so often (see `crate::disk`).
    _watch: Task<()>,
    _subscription: Subscription,
}

/// How long a passing notice ("Undid the decision on #1.", what a reload
/// brought in) stays in the status bar.
const NOTICE_FOR: Duration = Duration::from_secs(10);

/// The notice when a saved session is picked up (it passes, see [`NoticeLife`]).
const RESUMED: &str = "Picked up where you left off.";

/// How long the status bar's notice stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoticeLife {
    /// Until another notice replaces it or the writer's next action clears
    /// it: warnings, refusals, instructions.
    UntilNext,
    /// Also goes by itself after [`NOTICE_FOR`]: undo/redo results, what a
    /// reload brought in (reviews attached or detached from outside).
    Passing,
    /// The agent's line for a closed panel: goes when the panel opens.
    UntilPanelOpen,
}

/// How long the notice said on opening stays: "Picked up where you left
/// off" alone is a greeting that passes; anything else (a review taken off,
/// a fresh start) stays until the writer acts.
fn startup_life(notices: &[String]) -> NoticeLife {
    if notices.len() == 1 && notices[0] == RESUMED { NoticeLife::Passing } else { NoticeLife::UntilNext }
}

/// How long a reload's notice stays: news passes, but changes of the
/// writer's that couldn't be applied, or notes about review files (one taken
/// off), stay until the writer acts.
fn reload_life(summary: &disk::Summary) -> NoticeLife {
    if summary.lost.is_empty() && summary.notes.is_empty() { NoticeLife::Passing } else { NoticeLife::UntilNext }
}

/// Said when a rewrite is undone because its line went away.
const REWRITE_LOST: &str = "The line you were rewriting was removed, so the rewrite was undone.";

/// Write .fdx on a version while the agent panel's run goes on.
const AGENT_BUSY: &str =
    "The assistant is still working on this script: wait for it to finish, or stop it, then write.";

/// How often the app looks for changes another program made to its files.
const WATCH_EVERY: Duration = Duration::from_millis(1000);

impl ScriptView {
    pub fn new(opened: Opened, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Opened { script_path, raw, fdx, opened, pair, session_path, dir, seen, unsynced, notices } = opened;
        let sessionfile::ResumedSession { session, doc, base, .. } = opened;
        let synced = doc.log().len();
        let pair_watch = PairWatch::of(&pair, &dir, &file_name(&script_path));
        let originals: HashMap<ParaId, Paragraph> = fdx.to_script().paragraphs.into_iter().map(|p| (p.id, p)).collect();

        let editor = cx.new(|cx| ScriptEditor::new(doc, cx));
        let subscription = cx.subscribe_in(&editor, window, Self::on_editor_event);
        let watch = Self::start_watch(cx);
        let n = session.reviews.len();
        let mut this = Self {
            editor,
            session: Rc::new(session),
            base,
            script_path,
            dir,
            raw: raw.into(),
            fdx: Rc::new(fdx),
            pair_watch,
            ghosts: Rc::new(pair_ghosts(&pair)),
            recheck_pair: false,
            retired: false,
            pair: Rc::new(pair),
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
            notice_at: Instant::now(),
            // "Picked up where you left off" alone is only a greeting;
            // anything else said on opening (a review taken off, a fresh
            // start) stays until the writer acts.
            notice_life: startup_life(&notices),
            notice_timer: None,
            error: "".into(),
            reload_note: None,
            shown_notes: notices.iter().cloned().collect(),
            applied: None,
            autosave: None,
            seen_session: seen,
            synced,
            attached_since_sync: unsynced,
            stuck: None,
            held_lock: RefCell::new(Weak::new()),
            agent: None,
            agent_open: false,
            _watch: watch,
            _subscription: subscription,
        };
        this.build_rows(cx);
        let first = if n > 0 { View::Review(0) } else { View::Script };
        this.set_view(first, window, cx);
        // Open as the writer left it, without moving focus into it.
        if crate::settings::agent_panel_open() {
            this.agent_panel(window, cx);
            this.agent_open = true;
        }
        this
    }

    /// Look at the files every [`WATCH_EVERY`] ([`Self::watch_files`]).
    fn start_watch(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(WATCH_EVERY).await;
                if this.update(cx, |w, cx| w.watch_files(cx)).is_err() {
                    break;
                }
            }
        })
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
        self.set_notice_line(text.into(), NoticeLife::UntilNext);
        self.error = "".into();
        self.reload_note = None;
    }

    /// Like [`Self::say`], for news that needs no answer (what an undo took
    /// back): it also goes by itself after [`NOTICE_FOR`].
    fn say_briefly(&mut self, text: impl Into<SharedString>) {
        self.say(text);
        self.notice_life = NoticeLife::Passing;
    }

    fn set_notice_line(&mut self, text: SharedString, life: NoticeLife) {
        self.notice = text;
        self.notice_at = Instant::now();
        self.notice_life = life;
    }

    /// A passing notice goes [`NOTICE_FOR`] after it was set. Called on
    /// render, where the notice shows.
    fn expire_notice(&mut self, cx: &mut Context<Self>) {
        if self.notice.is_empty() || self.notice_life != NoticeLife::Passing {
            self.notice_timer = None;
            return;
        }
        let at = self.notice_at;
        if self.notice_timer.as_ref().is_some_and(|(t, _)| *t == at) {
            return;
        }
        let left = NOTICE_FOR.saturating_sub(at.elapsed());
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(left).await;
            this.update(cx, |this, cx| {
                if this.notice_at == at && this.notice_life == NoticeLife::Passing {
                    this.notice = "".into();
                    this.reload_note = None;
                    cx.notify();
                }
            })
            .ok();
        });
        self.notice_timer = Some((at, task));
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
            self.set_notice_line(text.into(), reload_life(&summary));
            self.reload_note = Some((Instant::now(), summary));
        }
    }

    /// File › Add review: open each file against the script (by `PARA:` as
    /// it is now, by `BEFORE` as it is in the `.fdx`), attach it, apply the decisions saved in it (one undo step
    /// each), and show the first one added.
    pub fn add_reviews(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        self.commit_rewrite_quietly(cx);
        let (mut notes, mut first) = (vec![], None);
        for p in paths {
            if !has_extension(p, "txt") {
                notes.push(format!("{} isn't a review file (they end in .txt).", file_name(p)));
                continue;
            }
            let p = p.canonicalize().unwrap_or_else(|_| p.clone());
            // Held from reading the file through pinning its id, so a
            // concurrent CLI/MCP append isn't overwritten.
            let lock = self.lock_session(scriptorium_fsio::LOCK_WAIT);
            let r = match self.attach_file(&p, lock.is_some(), &mut notes, cx) {
                Ok(r) => r,
                Err(e) => {
                    notes.push(e);
                    continue;
                }
            };
            drop(lock);
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

    /// Attach the review file at `p` (canonical): open it against the script
    /// (by `PARA:` as it is now, by `BEFORE` as it is in the `.fdx`), add it,
    /// pin its id into a file with no `Id:` header (only when `locked`: the
    /// caller holds the session lock), and apply the decisions saved in it
    /// (one undo step). Notes for the writer go to `notes`. Rows, outline
    /// notes and saving are the caller's.
    fn attach_file(
        &mut self,
        p: &Path,
        locked: bool,
        notes: &mut Vec<String>,
        cx: &mut Context<Self>,
    ) -> Result<usize, String> {
        let source = read_review(p, &self.dir)?;
        let fdx = &self.fdx;
        let rows = self.pair.resolver(fdx, self.doc(cx));
        let review = Review::open_live(
            source.name,
            &source.text,
            &fdx.to_script(),
            self.doc(cx),
            |k| fdx.is_caps(k.name()),
            &file_name(&self.script_path),
            rows.as_deref(),
        )
        .with_path(source.path);
        let restored = review.restored();
        let r = Rc::make_mut(&mut self.session).add(review)?;
        self.attached_since_sync.push(p.to_path_buf());
        // Attached: pin the review's id into a file that has no `Id:`
        // header, so later edits to the file don't orphan its decisions.
        // Written now, before the session that names it (host contract).
        if let Some(text) = Rc::make_mut(&mut self.session).reviews[r].pin_id() {
            if !locked {
                notes.push(format!(
                    "{} is busy in another program; its Id: wasn't written (it is attached).",
                    file_name(p)
                ));
            } else if let Err(e) = scriptorium_fsio::write_atomic(p, text.as_bytes()) {
                notes.push(format!("Couldn't write the review's Id: into {}: {e}", p.display()));
            }
        }
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
        Ok(r)
    }

    /// Before the window closes or switches script: finish a rewrite, save.
    /// Never drops the writer's work silently: if it can't be saved to the
    /// session file (another program left one that can't be used, or the
    /// write failed), it is written beside it as
    /// `X.scriptorium.set-aside-<time>.json`. Returns what to tell the writer.
    pub fn save_now(&mut self, cx: &mut Context<Self>) -> Option<String> {
        if self.retired {
            return None;
        }
        self.commit_rewrite_quietly(cx);
        self.autosave = None;
        // Worth waiting a moment for another program to finish writing; if
        // it doesn't, the work is set aside below.
        self.save_session_within(scriptorium_fsio::LOCK_WAIT, cx);
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
        let msg = match scriptorium_fsio::write_atomic(&path, json.as_bytes()) {
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
            (0, _) if !matches!(*self.pair, PairState::None) => {
                format!("No changes since {}", file_name(&self.script_path))
            }
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
        let mut decor = Decor { originals: self.originals.clone(), ghosts: self.ghosts.clone(), ..Decor::default() };
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
            }
            View::Script => {}
        }
        self.focus_main(window, cx);
        self.refresh(cx);
    }

    /// Focus the script: the card in a review view, else the editor (and the
    /// editor while a line is being rewritten).
    fn focus_main(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.review().is_some() && self.rewriting.is_none() {
            window.focus(&self.focus, cx);
        } else {
            let focus = self.editor.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
        }
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

    /// Tests: show review `id`'s suggestion `n` and start rewriting it (E).
    #[cfg(test)]
    pub(crate) fn rewrite_for_test(&mut self, id: &str, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.show_suggestion(id, n, window, cx);
        self.start_rewrite(window, cx);
    }

    /// Tests: whether a rewrite is in progress.
    #[cfg(test)]
    pub(crate) fn is_rewriting(&self) -> bool {
        self.rewriting.is_some()
    }

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
        if self.stuck.is_some() || self.retired {
            return true;
        }
        // Held from looking at the files through writing them, so no other
        // program writes in between (see `scriptorium_fsio::SessionLock`).
        let Some(_lock) = self.lock_session(wait) else { return false };
        match self.disk_plan().1 {
            Plan::Save => _ = self.write_session(cx),
            Plan::Reload => self.reload(cx),
            // Finishing the rewrite saves again.
            Plan::Wait => {}
        }
        true
    }

    /// Write the session file (call it holding the session lock). For the
    /// version Scriptorium made (or one that may be: its links file can't be
    /// used), each review's decisions are written into its file first, so
    /// they outlive a Final Draft save of the version (plan C2), as the
    /// CLI's save does. If that fails (a review file changed on disk since
    /// it was read), the app says so and still writes the session file, so
    /// nothing done here is lost; the next look at the files reloads the
    /// review and the save after it writes the decisions. (The CLI instead
    /// refuses the whole save: its command fails and nothing is written.)
    /// False: the session file wasn't written (said).
    fn write_session(&mut self, cx: &mut Context<Self>) -> bool {
        if self.pair.maybe_made() {
            let doc = &self.editor.read(cx).doc;
            match versions::write_progress(&self.dir, Rc::make_mut(&mut self.session), doc) {
                Ok(notes) => {
                    let new: Vec<String> = notes.into_iter().filter(|n| self.shown_notes.insert(n.clone())).collect();
                    if !new.is_empty() {
                        crate::log::warn(&new.join("; "));
                        self.say(new.join("  "));
                    }
                }
                // A review file changed on disk since it was read: the next
                // look at the files reloads it, and the save after that
                // writes its decisions.
                Err(e) => {
                    crate::log::warn(&format!("decisions not written into the review files: {e}"));
                    self.say_error(format!("Couldn't write the decisions into the review files: {e}"));
                }
            }
        }
        let doc = self.doc(cx);
        let len = doc.log().len();
        let json = SessionFile::new(&self.raw, self.session.attached(), &self.base, doc.log()).to_json();
        match scriptorium_fsio::write_atomic_no_link(&self.session_path, json.as_bytes()) {
            Ok(()) => {
                self.seen_session = Seen::wrote(&json);
                self.synced = len;
                self.attached_since_sync.clear();
                true
            }
            Err(e) => {
                self.say_error(format!("Couldn't save the session to {}: {e}", self.session_path.display()));
                cx.notify();
                false
            }
        }
    }

    /// Take the script's session lock, waiting up to `wait`. None: another
    /// program holds it. A lock that can't be taken for another reason (the
    /// folder is read-only, the file system has no locks) is logged and the
    /// app goes on unlocked, as it did before there was a lock.
    ///
    /// If the app already holds it (an agent tool call in progress, say),
    /// that lock is shared: the lock is per open file, so locking again here
    /// would find the app's own lock busy.
    fn lock_session(&self, wait: Duration) -> Option<SessionLease> {
        if let Some(held) = self.held_lock.borrow().upgrade() {
            return Some(held);
        }
        let at = scriptorium_fsio::SessionLock::path_for(&self.script_path);
        let lock = match scriptorium_fsio::SessionLock::acquire(&at, wait) {
            Ok(lock) => Some(lock),
            Err(scriptorium_fsio::LockError::Busy) => return None,
            Err(e) => {
                crate::log::warn(&format!("couldn't lock the session ({e}); going on without the lock"));
                None
            }
        };
        let lease = Rc::new(lock);
        *self.held_lock.borrow_mut() = Rc::downgrade(&lease);
        Some(lease)
    }

    /// Compare the session file and each review file with what the app
    /// holds: whether the session file changed, and what to do.
    fn disk_plan(&mut self) -> (bool, Plan) {
        let (session, reviews_changed) = self.disk_check();
        if let Disk::Touched(seen) = &session {
            self.seen_session = seen.clone();
        }
        (session == Disk::Changed, disk::plan(&session, reviews_changed, self.rewriting.is_some()))
    }

    /// The session file as against what the app last read or wrote, and
    /// whether any attached review file differs from the text held for it.
    fn disk_check(&self) -> (Disk, bool) {
        let path = &self.session_path;
        let session = sessionfile::check(&self.seen_session, modified(path), || std::fs::read_to_string(path).ok());
        let reviews_changed = self.session.reviews.iter().any(|r| {
            let at = sessionfile::resolve_path(&self.dir, &r.path);
            sessionfile::review_changed(r.text(), std::fs::read_to_string(at).ok().as_deref())
        });
        (session, reviews_changed)
    }

    /// Every so often: pick up what another program wrote, so a suggestion an
    /// agent adds shows up without waiting for the next edit here. While the
    /// session file can't be used, only a new version of it (or its removal)
    /// is worth another look.
    fn watch_files(&mut self, cx: &mut Context<Self>) {
        if self.retired {
            return;
        }
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
            return;
        }
        // The pair: its links file or the other member changed (another
        // window, the CLI, Final Draft), or a version was just made here.
        // Loading it again re-places `ROW:` suggestions, so it goes through
        // the reload, which can't run under a rewrite.
        if self.rewriting.is_none()
            && self.stuck.is_none()
            && (std::mem::take(&mut self.recheck_pair) || self.pair_watch.changed())
        {
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
        let (opened, pair, seen, notes) = resume_from_disk(&self.script_path, &self.fdx, &self.session_path, &[]);
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
        // The pair as loaded with this session: reviews reopened here place
        // `ROW:` suggestions (an agent may just have appended some) too.
        notes.extend(pair_note(&pair));
        self.pair_watch = PairWatch::of(&pair, &self.dir, &script_name);
        self.ghosts = Rc::new(pair_ghosts(&pair));
        self.pair = Rc::new(pair);
        let rows = self.pair.resolver(&self.fdx, &replayed.doc);
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
                        rows.as_deref(),
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

    // ---- the agent panel ------------------------------------------------------

    /// The agent panel, made on first use.
    fn agent_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<AgentPanel> {
        if let Some((panel, _)) = &self.agent {
            return panel.clone();
        }
        let (me, path) = (cx.entity().downgrade(), self.script_path.clone());
        let panel = cx.new(|cx| AgentPanel::new(me, path, window, cx));
        let sub = cx.subscribe_in(&panel, window, |this, panel, e: &AgentPanelEvent, window, cx| match e {
            AgentPanelEvent::ReturnFocus => this.focus_main(window, cx),
            AgentPanelEvent::Close => {
                let had_focus = panel.read(cx).has_focus(window, cx);
                this.set_agent_open(false, cx);
                if had_focus {
                    this.focus_main(window, cx);
                }
            }
            AgentPanelEvent::Say(text) => {
                this.say(text.clone());
                // It goes when the writer sees it (opens the panel, or Unsay).
                this.notice_life = NoticeLife::UntilPanelOpen;
                cx.notify();
            }
            // Only that line: a newer notice or an error stays.
            AgentPanelEvent::Unsay(text) => {
                if this.notice == *text {
                    this.notice = "".into();
                    cx.notify();
                }
            }
        });
        self.agent = Some((panel.clone(), sub));
        panel
    }

    /// Ctrl/Cmd+L: closed, open it and focus the composer; open, focus the
    /// composer, unless it has focus already: then close the panel and focus
    /// the script. Remembered for next time.
    pub fn toggle_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = self.agent_panel(window, cx);
        if self.agent_open && panel.read(cx).composer_focused(window, cx) {
            self.set_agent_open(false, cx);
            self.focus_main(window, cx);
        } else {
            self.set_agent_open(true, cx);
            panel.update(cx, |p, cx| p.focus_composer(window, cx));
        }
    }

    /// Cmd/Ctrl+. outside the panel: stops the agent's run (or answers its
    /// question "no"), whether or not the panel is showing.
    pub(crate) fn stop_agent(&mut self, cx: &mut Context<Self>) {
        if let Some((panel, _)) = &self.agent {
            let composer = panel.read(cx).composer().clone();
            composer.update(cx, |c, cx| c.stop(cx));
        }
    }

    /// [`Self::say_reloaded`], for tests of the notice's lifetime.
    #[cfg(test)]
    pub(crate) fn say_reloaded_for_test(&mut self, summary: disk::Summary, cx: &mut Context<Self>) {
        self.say_reloaded(summary);
        cx.notify();
    }

    /// [`Self::say_briefly`], for tests of the notice's lifetime.
    #[cfg(test)]
    pub(crate) fn say_briefly_for_test(&mut self, text: &str, cx: &mut Context<Self>) {
        self.say_briefly(text.to_string());
        cx.notify();
    }

    /// The status bar's notice line.
    #[cfg(test)]
    pub(crate) fn notice_text(&self) -> SharedString {
        self.notice.clone()
    }

    /// The agent panel's conversation, once the panel exists.
    #[cfg(test)]
    pub(crate) fn agent_chat(&self, cx: &App) -> Option<Entity<crate::agent_panel::driver::Chat>> {
        self.agent.as_ref().map(|(p, _)| p.read(cx).chat().clone())
    }

    /// Whether the agent panel is showing.
    pub(crate) fn agent_open(&self) -> bool {
        self.agent_open
    }

    fn set_agent_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.agent_open != open {
            crate::log::info(&format!("agent panel {}", if open { "open" } else { "closed" }));
        }
        self.agent_open = open;
        // The agent's lines for a closed panel ("…Ctrl+L to see") are seen now.
        if open && self.notice_life == NoticeLife::UntilPanelOpen {
            self.notice = "".into();
            self.notice_life = NoticeLife::UntilNext;
        }
        crate::settings::set_agent_panel_open(open);
        cx.notify();
    }
}

#[allow(dead_code)] // Helpers the live store (agent_panel/live.rs) doesn't use yet.
impl ScriptView {
    // ---- for the live store (agent_panel/live.rs, U5) ---------------------------
    //
    // A tool call runs inside one update of this view (`chat::exec::run_call`
    // over a `LiveScript` wrapping `&mut ScriptView`), between
    // [`Self::try_lock_session`] and dropping the lease. Nothing here waits.

    /// The session lock, without waiting: None while another program holds
    /// it (the tool result is "busy"; never wait on the UI thread). If the
    /// app holds it already, that lock is shared. Hold it for the whole call.
    pub(crate) fn try_lock_session(&self) -> Option<SessionLease> {
        self.lock_session(Duration::ZERO)
    }

    /// Under the lock: load what other programs wrote since the app last
    /// read or wrote the files. [`LiveError::Busy`] only when they changed
    /// while the writer is rewriting a line (the app can't reload until the
    /// rewrite ends); a rewrite with nothing new on disk is fine. Fails while
    /// the session file on disk can't be used.
    pub(crate) fn reload_or_busy(&mut self, cx: &mut Context<Self>) -> Result<(), LiveError> {
        let stuck = |why: &str| LiveError::Failed(format!("The script's session file can't be used: {why}"));
        if let Some(why) = &self.stuck {
            return Err(stuck(why));
        }
        let (session, reviews_changed) = self.disk_check();
        if let Disk::Touched(seen) = &session {
            self.seen_session = seen.clone();
        }
        if session != Disk::Changed && !reviews_changed {
            return Ok(());
        }
        if self.rewriting.is_some() {
            return Err(LiveError::Busy);
        }
        self.reload(cx);
        match &self.stuck {
            Some(why) => Err(stuck(why)),
            None => Ok(()),
        }
    }

    /// Under the lock: attach the review file at `path` (its `Id:` pinned if
    /// it has none, its saved decisions applied); its index. Call
    /// [`Self::after_agent_change`] after.
    pub(crate) fn attach_review(&mut self, path: &Path, cx: &mut Context<Self>) -> Result<usize, LiveError> {
        let p = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut notes = vec![];
        let r = self.attach_file(&p, true, &mut notes, cx).map_err(LiveError::Failed)?;
        if !notes.is_empty() {
            self.shown_notes.extend(notes.iter().cloned());
            self.say(notes.join("  "));
        }
        Ok(r)
    }

    /// The script's folder (review paths are stored relative to it).
    pub(crate) fn script_dir(&self) -> &Path {
        &self.dir
    }

    /// The script's file name, e.g. `film.fdx`.
    pub(crate) fn script_name(&self) -> String {
        file_name(&self.script_path)
    }

    /// The script as it is now (the editor's document).
    pub(crate) fn live_doc<'a>(&self, cx: &'a App) -> &'a Document {
        self.doc(cx)
    }

    /// The resolver placing `ROW:` suggestions on the live document, from
    /// the pair loaded with the session (`None` without one, or on the
    /// original).
    pub(crate) fn row_resolver(&self, cx: &App) -> Option<Box<scriptorium_review::RowResolver<'static>>> {
        self.pair.resolver(&self.fdx, self.doc(cx))
    }

    pub(crate) fn session(&self) -> &Session {
        &self.session
    }

    /// The pair's rows over the live session and document, for the tools'
    /// `Ctx::links` (`PairState::view`): `None` without a pair, the reason
    /// when its links file can't be used.
    pub(crate) fn pair_view<'a>(&'a self, cx: &'a App) -> Option<Result<scriptorium_versions::PairView<'a>, String>> {
        self.pair.view(&self.fdx, &self.session, self.doc(cx))
    }

    /// Whether this script is, or may be, the version of its pair
    /// (`PairState::maybe_made`).
    pub(crate) fn may_be_version(&self) -> bool {
        self.pair.maybe_made()
    }

    /// After the agent's `new_version` (made or refused): say `notes`, and
    /// load the pair again on the next look at the files.
    /// Why a version can't be made of this script now: it reads otherwise
    /// here than in its file (decisions or typing not written), and a
    /// version is made from the file.
    pub(crate) fn unwritten_changes(&self, cx: &App) -> Option<String> {
        let same = self.fdx.write_script(self.doc(cx).script()).is_ok_and(|raw| raw == *self.raw);
        (!same).then(|| {
            let name = file_name(&self.script_path);
            format!(
                "{name} has changes in Scriptorium that aren't in the file, and a version is made from the file: ask \
                 the writer to undo them first, or to bring them into {name} in Final Draft (Write .fdx writes {})",
                file_name(&output_path(&self.script_path))
            )
        })
    }

    pub(crate) fn after_new_version(&mut self, notes: Vec<String>, cx: &mut Context<Self>) {
        self.recheck_pair = true;
        let new: Vec<String> = notes.into_iter().filter(|n| self.shown_notes.insert(n.clone())).collect();
        if !new.is_empty() {
            self.say(new.join("  "));
            cx.notify();
        }
    }

    /// Where review `r`'s file is.
    pub(crate) fn review_file(&self, r: usize) -> PathBuf {
        sessionfile::resolve_path(&self.dir, &self.session.reviews[r].path)
    }

    /// Whether the script shows an element kind in capitals; owns what it
    /// needs, so it can be kept beside the borrows of [`Self::session_mut`].
    pub(crate) fn caps(&self) -> impl Fn(&ElementKind) -> bool + 'static {
        let fdx = self.fdx.clone();
        move |k| fdx.is_caps(k.name())
    }

    /// The session to add suggestions to, with the document they are against.
    /// Call [`Self::after_agent_change`] after changing it.
    pub(crate) fn session_mut<'a>(&'a mut self, cx: &'a App) -> (&'a mut Session, &'a Document) {
        (Rc::make_mut(&mut self.session), &self.editor.read(cx).doc)
    }

    /// The lines the writer is rewriting right now (every line of a block
    /// being rewritten, as it stands now, and the line it was made on):
    /// proposals touching any of them are refused.
    pub(crate) fn editing_paras(&self, cx: &App) -> Vec<ParaId> {
        let Some(rw) = &self.rewriting else { return vec![] };
        let mut paras = self.session.live_run(self.doc(cx), rw.r, rw.i);
        let item = self.session.item(rw.r, rw.i);
        for p in std::iter::once(item.para).chain(item.paras.iter().copied()) {
            if !paras.contains(&p) {
                paras.push(p);
            }
        }
        paras
    }

    /// After a tool call changed the session (attached a review, added
    /// suggestions): the sidebar and the card catch up, and the session is
    /// saved shortly (the lock is shared if the call still holds it).
    pub(crate) fn after_agent_change(&mut self, cx: &mut Context<Self>) {
        let doc = self.doc(cx).clone();
        Rc::make_mut(&mut self.session).note_removed_by(&doc);
        self.build_rows(cx);
        self.refresh(cx);
        self.schedule_autosave(cx);
        cx.notify();
    }
}

impl ScriptView {
    /// The panel's "Proposed 8 changes → Dialogue pass" link: show review
    /// `id` at suggestion number `n` (as `list_suggestions` numbers them) and
    /// focus the card, so A/R/E work at once.
    #[allow(dead_code)] // The message view (U3).
    pub(crate) fn show_suggestion(&mut self, id: &str, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.session.reviews.iter().position(|x| x.id == id) else {
            self.say("That review isn't attached any more.");
            cx.notify();
            return;
        };
        let i = item_numbers(self.doc(cx), &self.session.reviews[r].items).iter().position(|&k| k == n);
        if self.view != View::Review(r) {
            self.set_view(View::Review(r), window, cx);
        }
        match i {
            Some(i) => self.go_to(i, window, cx),
            None => {
                self.say(format!("{} has no suggestion #{n}.", self.session.reviews[r].name));
                window.focus(&self.focus, cx);
            }
        }
        cx.notify();
    }

    pub fn apply_to_fdx(&mut self, _: &ApplyToFdx, _: &mut Window, cx: &mut Context<Self>) {
        if self.retired {
            return;
        }
        self.commit_rewrite_quietly(cx);
        if self.pair.is_made() {
            self.write_in_place(cx);
            cx.notify();
            return;
        }
        let out = output_path(&self.script_path);
        let script = self.doc(cx).script().clone();
        let said = match self.fdx.write_script(&script) {
            Ok(raw) => match scriptorium_fsio::write_atomic(&out, raw.as_bytes()) {
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

    /// Write .fdx on the version Scriptorium made: written over its own file
    /// (plan, "Writing a version in place"). The autosave and the watch are
    /// stopped first (`write_in_place` writes the session for the new bytes;
    /// nothing of this view may save after it); then, under the session
    /// lock, the session is saved (what other programs wrote reloaded
    /// first) and `write_in_place` reads everything fresh from disk. Written
    /// (even if only as far as the `.fdx`): [`Reopen`], and this view is
    /// retired without a final save. Refused: said, and the watch starts
    /// again.
    fn write_in_place(&mut self, cx: &mut Context<Self>) {
        // The agent may be about to add suggestions to this session.
        if self.agent.as_ref().is_some_and(|(p, _)| p.read(cx).chat().read(cx).is_running()) {
            self.say("");
            self.say_error(AGENT_BUSY);
            return;
        }
        self.autosave = None;
        self._watch = Task::ready(());
        let refused = |this: &mut Self, why: String, cx: &mut Context<Self>| {
            crate::log::warn(&format!("writing {} in place: {why}", this.script_path.display()));
            this.say("");
            this.say_error(format!("Not written: {why}"));
            this._watch = Self::start_watch(cx);
            // Anything not saved yet is saved as usual.
            this.schedule_autosave(cx);
        };
        let Some(lease) = self.lock_session(scriptorium_fsio::LOCK_WAIT) else {
            let why = format!("{}'s files are busy in another program; try again.", file_name(&self.script_path));
            return refused(self, why, cx);
        };
        // Save first: what other programs wrote (the pair too, so the
        // session matches what `write_in_place` opens and numbers), then the
        // app's own work.
        if self.disk_plan().1 == Plan::Reload || self.pair_watch.changed() {
            self.reload(cx);
        }
        if let Some(why) = &self.stuck {
            let why = format!("the session file can't be used ({why}).");
            return refused(self, why, cx);
        }
        if !self.write_session(cx) {
            return refused(self, "the session couldn't be saved first.".into(), cx);
        }
        let raw = self.raw.clone();
        match versions::write_in_place(&self.script_path, &raw) {
            Ok(w) => {
                crate::log::info(&format!("wrote {} in place", w.path.display()));
                self.retire(
                    Reopen {
                        path: w.path,
                        notice: std::iter::once(w.message).chain(w.notes).collect::<Vec<_>>().join("  "),
                    },
                    cx,
                );
            }
            Err(InPlaceError::WrittenButIncomplete { path, message }) => {
                crate::log::warn(&message);
                self.retire(Reopen { path, notice: message }, cx);
            }
            Err(e) => {
                // A failed `.fdx` write puts the review files back as they
                // were read, and they may differ from what the app holds:
                // load them before anything is saved again.
                if self.disk_plan().1 == Plan::Reload {
                    self.reload(cx);
                }
                let why = match e {
                    // Numbered in the session as saved on disk, which is
                    // this one now (saved, and reloaded if it changed).
                    InPlaceError::Blocked(items) => {
                        let doc = self.doc(cx).clone();
                        let numbered: Vec<(usize, LineChange)> = items
                            .into_iter()
                            .filter(|(r, i, _)| self.session.reviews.get(*r).is_some_and(|x| *i < x.items.len()))
                            .map(|(r, i, how)| (self.number(&doc, r, i), how))
                            .collect();
                        versions::blocked(&numbered)
                    }
                    other => other.to_string(),
                };
                drop(lease);
                refused(self, why, cx);
            }
        }
    }

    /// Stop for good and ask to be replaced by `reopen` (the window drops
    /// this view without saving it).
    fn retire(&mut self, reopen: Reopen, cx: &mut Context<Self>) {
        self.retired = true;
        self.autosave = None;
        self._watch = Task::ready(());
        self.outline_task = None;
        cx.emit(reopen);
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
        self.say_briefly(said.unwrap_or_default());
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
            self.say_briefly(if redo { "Nothing to redo." } else { "Nothing to undo." });
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
            self.say_briefly(said.unwrap_or_default());
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
        // (mark, text, dimmed: a blank line, named by what it is for)
        let marks: Rc<Vec<(Mark, SharedString, bool)>> = Rc::new(match self.view {
            View::Review(r) => self.session.reviews[r]
                .items
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    let (text, dim) = match item_text(&self.session, doc, r, i) {
                        t if t.trim().is_empty() => (blank_item_text(&self.session, doc, r, i), true),
                        t => (t, false),
                    };
                    (mark_of(&self.session, doc, r, i), text.into(), dim)
                })
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
                            let (mark, text, dim) = &marks[*i];
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
                                        .text_color(if !*dim && (*mark == Mark::Pending || *mark == Mark::Outdated) {
                                            INK
                                        } else {
                                            INK_3
                                        })
                                        .when(*dim, |d| d.italic())
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
            .w(px(agent_panel::SIDEBAR_WIDTH))
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

    /// The sidebar collapsed (agent panel open in a narrow window): the views
    /// as their Cmd/Ctrl+number shortcuts, a dot where suggestions are left.
    fn render_rail(&self, cx: &mut Context<Self>) -> Div {
        let doc = self.doc(cx);
        let items: Vec<AnyElement> = self
            .views()
            .into_iter()
            .enumerate()
            .map(|(n, v)| {
                let on = v == self.view;
                let left = match v {
                    View::Review(r) => self.session.progress(doc, r).pending,
                    View::Script => 0,
                };
                div()
                    .id(("rail", n))
                    .relative()
                    .size(px(32.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(INK_2)
                    .hover(|s| s.bg(ACCENT_SOFT))
                    .when(on, |d| d.bg(ACCENT_SOFT).text_color(ACCENT).font_weight(FontWeight::SEMIBOLD))
                    .child(if n == 0 { "S".to_string() } else { n.to_string() })
                    .tooltip({
                        // The letters alone mean little: say what each one is,
                        // what's left in it and its shortcut (Ctrl/Cmd+0…3).
                        let key = (n <= 3).then(|| crate::menus::shortcut(&n.to_string()));
                        let what = match v {
                            View::Review(r) if left > 0 => {
                                format!("Review: {} \u{b7} {left} left", self.session.reviews[r].name)
                            }
                            View::Review(r) => format!("Review: {}", self.session.reviews[r].name),
                            View::Script => "The script".to_string(),
                        };
                        let name: SharedString = match key {
                            Some(k) => format!("{what} ({k})").into(),
                            None => what.into(),
                        };
                        move |_, cx| cx.new(|_| RailTip(name.clone())).into()
                    })
                    .when(left > 0, |d| {
                        d.child(div().absolute().top(px(4.)).right(px(4.)).size(px(6.)).rounded_full().bg(ACCENT))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.set_view(v, window, cx)))
                    .into_any_element()
            })
            .collect();
        div()
            .flex_none()
            .w(px(agent_panel::RAIL_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .py_2()
            .bg(SIDE)
            .border_r_1()
            .border_color(RULE)
            .children(items)
    }

    fn render_card(&self, width: f32, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let focused = self.focus.is_focused(window);
        let typing = self.typing(window, cx);
        let mut body: Vec<AnyElement> = vec![];
        let shell = div()
            .id("card")
            .track_focus(&self.focus)
            .key_context("Review")
            .flex_none()
            .w(px(width))
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

        let row = self.row_card_for(r, i);
        let doc = self.doc(cx);
        let item = self.session.item(r, i);
        if item.block {
            self.block_card(&mut body, r, i, cx);
            let status = self.session.status(self.doc(cx), r, i);
            self.card_footer(&mut body, r, i, status, typing, cx);
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
        if let Some(card) = &row {
            self.row_section(&mut body, card);
        }
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
        let yours = row.as_ref().map_or("Yours now".to_string(), |c| format!("{} now", c.version));
        body.push(label_with(yours, chip));
        let hi = self.hi.min(item.options.len() - 1);
        let mine = if let Some(card) = row.as_ref().filter(|_| now.trim().is_empty()) {
            // A placeholder: say so rather than show an empty bar.
            StyledText::new(card.blank)
        } else if status == Status::Pending {
            diff_text(&now, &item.options[hi], WordOp::Del, caps)
        } else {
            plain_text(&now, caps)
        };
        let blank = row.is_some() && now.trim().is_empty();
        body.push(
            div()
                .border_l_2()
                .border_color(RULE)
                .pl_2()
                .child(if blank {
                    div().text_size(px(12.)).italic().text_color(INK_3).child(mine)
                } else {
                    script_text(mine).text_color(INK_2)
                })
                .into_any_element(),
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
        self.card_footer(&mut body, r, i, status, typing, cx);
        shell.children(body)
    }

    /// What the card says about item `i`'s row, for a `ROW:` suggestion on
    /// the version of a loaded pair.
    fn row_card_for(&self, r: usize, i: usize) -> Option<RowCard> {
        let row = self.session.item(r, i).row.as_ref()?;
        let pair = self.pair.loaded().filter(|p| p.this_is_version())?;
        Some(row_card(&pair.links, row))
    }

    /// The original's line a `ROW:` suggestion was made for, labelled with
    /// the original's language: under the card's heading, before the
    /// version's line.
    fn row_section(&self, body: &mut Vec<AnyElement>, card: &RowCard) {
        body.push(label(card.label.clone()));
        body.push(
            div()
                .border_l_2()
                .border_color(RULE)
                .pl_2()
                .child(script_text(plain_text(&card.text, false)).text_color(INK_2))
                .into_any_element(),
        );
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
        if let Some(card) = self.row_card_for(r, i) {
            self.row_section(body, &card);
        }
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
        typing: Typing,
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
        // While rewriting, the card's own line says what keys do.
        let into = match typing {
            Typing::ReviewKeys | Typing::Rewrite => None,
            Typing::Chat => Some("Typing goes to the chat"),
            Typing::ChatReadOnly => Some("This conversation is read only"),
            Typing::Script => Some("Typing goes to the script"),
        };
        if let Some(into) = into {
            body.push(small(format!("{into}. Esc or click here for review keys.")).into_any_element());
        }
    }

    /// Where keys typed now go, for the hints on the card and the status bar.
    fn typing(&self, window: &Window, cx: &App) -> Typing {
        let panel = self.agent.as_ref().filter(|_| self.agent_open).map(|(p, _)| p.read(cx));
        let chat = panel.filter(|p| p.has_focus(window, cx)).map(|p| p.chat().read(cx).read_only());
        typing_for(chat, self.review().is_some() && self.focus.is_focused(window), self.rewriting.is_some())
    }

    fn render_status(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let mut left = div().flex_none().flex().items_center().gap_3();
        let typing = self.typing(window, cx);
        let hints = status_hints(typing, self.review().is_some(), self.rewriting.is_some());
        match self.view {
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
            }
            View::Script => {
                left = left.child(format!("Script · {} paragraphs", self.doc(cx).paragraphs().len()));
            }
        }
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
                    // In a narrow window the line ends in "…" rather than
                    // running off the edge.
                    .min_w_0()
                    .truncate()
                    .when(!said.is_empty(), |d| d.text_color(INK).font_weight(FontWeight::SEMIBOLD))
                    .when(!self.error.is_empty(), |d| d.text_color(WARN))
                    .child(right),
            )
    }
}

/// Where keys typed now go ([`ScriptView::typing`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Typing {
    /// The script's text (the editor, or nothing in particular).
    Script,
    /// The review card's single keys (A, R, E, J/K…).
    ReviewKeys,
    /// The editor, rewriting the line under review (E).
    Rewrite,
    /// The chat panel (its message box, list or buttons).
    Chat,
    /// The chat panel, on a conversation another program has open: its
    /// message box is off.
    ChatReadOnly,
}

/// [`Typing`] from where focus is: in the chat panel (`Some(read_only)`), on
/// the review card, or else in the editor (rewriting or not).
fn typing_for(chat: Option<bool>, card: bool, rewriting: bool) -> Typing {
    match chat {
        Some(true) => Typing::ChatReadOnly,
        Some(false) => Typing::Chat,
        None if card => Typing::ReviewKeys,
        None if rewriting => Typing::Rewrite,
        None => Typing::Script,
    }
}

/// The status bar's hints for `typing`, in a review or the script view.
/// Esc from the chat goes where `focus_main` sends it: the card in a review,
/// the editor while rewriting or in the script view.
fn status_hints(typing: Typing, review: bool, rewriting: bool) -> String {
    let script = || {
        format!(
            "Enter new line · Tab line type · {} undo · {} write .fdx",
            crate::menus::shortcut("z"),
            crate::menus::shortcut("s")
        )
    };
    let back = if rewriting {
        "back to the rewrite"
    } else if review {
        "review keys"
    } else {
        "the script"
    };
    let close = format!("{} close the chat", crate::menus::shortcut("l"));
    match typing {
        Typing::ReviewKeys => {
            "A accept · 1 2 3 option · Tab next option · R keep mine · E rewrite · J/K next/prev · U undo".to_string()
        }
        Typing::Rewrite => format!("Rewriting the line · {COMMIT_KEY} saves and moves on · Esc puts it back"),
        Typing::Script if review => format!("Typing edits the script · Esc review keys · {}", script()),
        Typing::Script => script(),
        Typing::Chat => format!("Typing goes to the chat · Esc {back} · {close}"),
        Typing::ChatReadOnly => format!("This conversation is read only here · Esc {back} · {close}"),
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
    "edit the lines in the script, then press ⌘Return"
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

/// The sidebar's text for an item on a blank line: the original's line it
/// is for (`ROW TEXT`), else "(blank Action)".
fn blank_item_text(session: &Session, doc: &Document, r: usize, i: usize) -> String {
    let item = session.item(r, i);
    if let Some(row) = item.row.as_ref().filter(|row| row.texts.iter().any(|t| !t.trim().is_empty())) {
        return row.texts.join(" ");
    }
    let kind = doc.paragraph(item.para).map_or("line".to_string(), |p| p.kind.name().to_string());
    format!("(blank {kind})")
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
        self.expire_notice(cx);
        let agent = self.agent.as_ref().filter(|_| self.agent_open).map(|(panel, _)| panel.clone());
        let width = window.viewport_size().width / px(1.);
        // Keys going to the script keep it on screen (the card gives way).
        let keep_script = self.rewriting.is_some() || self.editor.read(cx).focus_handle().is_focused(window);
        let cols = agent_panel::columns(width, agent.is_some(), self.review().is_some(), keep_script);
        // A card that gave way to the script isn't drawn (its focus isn't in it).
        let card_shown = !cols.card_hidden(self.review().is_some());
        let card = self.review().filter(|_| card_shown).map(|_| {
            let card = self.render_card(cols.card, window, cx);
            if cols.script {
                card
            } else {
                card.child(small(format!(
                    "The script is hidden to make room. Widen the window or close the chat ({}) to see it.",
                    crate::menus::shortcut("l")
                )))
            }
        });
        if let Some(panel) = &agent {
            panel.update(cx, |p, cx| p.set_width(cols.panel, cx));
        }
        let side = if cols.rail { self.render_rail(cx) } else { self.render_sidebar(cx) };
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
                    .child(side)
                    // Too narrow for every column: the script gives way to the card.
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_hidden()
                            .when(!cols.script, |d| d.flex_none().w(px(0.)))
                            .child(self.editor.clone()),
                    )
                    .children(card)
                    .children(agent),
            )
            .child(self.render_status(window, cx))
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
    fn startup_and_reload_notices_pass_only_when_they_are_news() {
        assert_eq!(startup_life(&[RESUMED.to_string()]), NoticeLife::Passing);
        let warned = [RESUMED.to_string(), "notes.review.txt was taken off this script's reviews.".to_string()];
        assert_eq!(startup_life(&warned), NoticeLife::UntilNext);
        assert_eq!(startup_life(&["Started fresh: unreadable.".to_string()]), NoticeLife::UntilNext);
        let news = disk::Summary { attached: vec!["notes".into()], ..Default::default() };
        assert_eq!(reload_life(&news), NoticeLife::Passing);
        let lost = disk::Summary { lost: vec!["\"Laura…\"".into()], ..news.clone() };
        assert_eq!(reload_life(&lost), NoticeLife::UntilNext);
        let noted = disk::Summary { notes: vec!["x was taken off".into()], ..news };
        assert_eq!(reload_life(&noted), NoticeLife::UntilNext);
    }

    #[test]
    fn typing_follows_focus() {
        assert_eq!(typing_for(Some(false), true, false), Typing::Chat);
        assert_eq!(typing_for(Some(true), false, false), Typing::ChatReadOnly);
        assert_eq!(typing_for(None, true, false), Typing::ReviewKeys);
        assert_eq!(typing_for(None, false, true), Typing::Rewrite);
        assert_eq!(typing_for(None, false, false), Typing::Script);
    }

    #[test]
    fn status_hints_say_what_keys_do_now() {
        assert!(status_hints(Typing::ReviewKeys, true, false).starts_with("A accept"));
        let rw = status_hints(Typing::Rewrite, true, true);
        assert!(rw.contains(COMMIT_KEY) && rw.contains("Esc puts it back"), "{rw}");
        // Esc from the chat goes where focus_main sends it.
        assert!(status_hints(Typing::Chat, true, false).contains("Esc review keys"));
        assert!(status_hints(Typing::Chat, true, true).contains("Esc back to the rewrite"));
        assert!(status_hints(Typing::Chat, false, false).contains("Esc the script"));
        let ro = status_hints(Typing::ChatReadOnly, false, false);
        assert!(!ro.contains("Typing goes to the chat") && ro.contains("read only"), "{ro}");
        assert!(status_hints(Typing::Script, true, false).starts_with("Typing edits the script"));
        assert!(status_hints(Typing::Script, false, false).starts_with("Enter new line"));
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

#[cfg(test)]
mod row_tests {
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

    /// Opening and every reload go through `resume_from_disk`: a `ROW:`
    /// suggestion an agent appended to an attached review is placed on its
    /// row when the session is read again.
    #[test]
    fn a_review_reloaded_after_an_agent_append_places_row_suggestions() {
        let dir = std::env::temp_dir().join(format!("scriptorium-app-rows-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
        scriptorium_fsio::versions::version_new(&dir.join("film.fdx"), "es", Some("en"), None, &mut vec![]).unwrap();
        let links = std::fs::read_to_string(dir.join("film.es.links.txt")).unwrap();
        let id = links.lines().find_map(|l| l.strip_prefix("Id: ")).unwrap().to_string();

        let es = dir.join("film.es.fdx");
        let fdx = FdxFile::parse(std::fs::read_to_string(&es).unwrap()).unwrap();
        let session_path = dir.join(sessionfile::file_name_for("film.es.fdx"));
        let review = dir.join("tr.review.txt");
        std::fs::write(&review, "SCRIPT REVIEW v1\nScript: film.es.fdx\nId: tr-1\n").unwrap();
        // Opened with the (empty) review attached, and saved.
        let (opened, pair, _, _) = resume_from_disk(&es, &fdx, &session_path, std::slice::from_ref(&review));
        assert!(pair.is_made(), "{pair:?}");
        let json = SessionFile::new(fdx.raw(), opened.session.attached(), &opened.base, opened.doc.log()).to_json();
        std::fs::write(&session_path, json).unwrap();

        // An agent appends a suggestion for the blank dialogue's row.
        let mut text = std::fs::read_to_string(&review).unwrap();
        text.push_str(&format!(
            "\n---\nBEFORE: [empty]\nAFTER: Mañana voy al pueblo.\nWHY:\nROW: {id}\nROW TEXT: I am going to town \
             tomorrow.\n"
        ));
        std::fs::write(&review, text).unwrap();

        // Read again, as the watch tick's reload does.
        let (opened, _, _, notices) = resume_from_disk(&es, &fdx, &session_path, &[]);
        let r = &opened.session.reviews[0];
        assert!(r.unmatched.is_empty(), "{:?} {notices:?}", r.unmatched);
        assert_eq!(r.items.len(), 1);
        let dialogue = opened.doc.paragraphs().iter().find(|p| p.kind == ElementKind::Dialogue).unwrap().id;
        assert_eq!(r.items[0].para, dialogue);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Versions in the app (plan step A10): the original's line on the card,
/// the pair watched, Write .fdx over the version Scriptorium made.
#[cfg(test)]
mod version_tests {
    use super::*;
    use gpui::{TestAppContext, WindowHandle};
    use std::cell::RefCell;

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

    /// `film.fdx`, its version `film.es.fdx` (made by `version new`) and a
    /// review on the version with one `ROW:` suggestion for the blank
    /// dialogue, attached; removed on drop.
    struct Pair {
        dir: PathBuf,
        id: String,
    }

    impl Pair {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("scriptorium-app-version-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let dir = dir.canonicalize().unwrap();
            std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
            versions::version_new(&dir.join("film.fdx"), "es", Some("en"), None, &mut vec![]).unwrap();
            let links = std::fs::read_to_string(dir.join("film.es.links.txt")).unwrap();
            let id = links.lines().find_map(|l| l.strip_prefix("Id: ")).unwrap().to_string();
            let p = Self { dir, id };
            std::fs::write(
                p.at("tr.review.txt"),
                format!(
                    "SCRIPT REVIEW v1\nScript: film.es.fdx\nId: tr-1\n{}",
                    p.entry("[empty]", "Mañana voy al pueblo.", "I am going to town tomorrow.")
                ),
            )
            .unwrap();
            p
        }
        fn at(&self, name: &str) -> PathBuf {
            self.dir.join(name)
        }
        fn read(&self, name: &str) -> String {
            std::fs::read_to_string(self.at(name)).unwrap()
        }
        /// A `ROW:` entry for the line reading `before` (`[empty]`: blank)
        /// whose original reads `row_text`.
        fn entry(&self, before: &str, after: &str, row_text: &str) -> String {
            format!("\n---\nBEFORE: {before}\nAFTER: {after}\nWHY:\nROW: {}\nROW TEXT: {row_text}\n", self.id)
        }
        /// An agent appends an entry to the review.
        fn append(&self, entry: &str) {
            let mut text = self.read("tr.review.txt");
            text.push_str(entry);
            std::fs::write(self.at("tr.review.txt"), text).unwrap();
        }
    }

    impl Drop for Pair {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn open(cx: &mut TestAppContext, p: &Pair) -> (WindowHandle<ScriptView>, Entity<ScriptView>) {
        cx.update(gpui_base::init);
        let opened = Opened::load(&p.at("film.es.fdx"), &[p.at("tr.review.txt")]).unwrap();
        let window = cx.add_window(|window, cx| ScriptView::new(opened, window, cx));
        let view = window.root(cx).unwrap();
        (window, view)
    }

    /// Every [`Reopen`] the view emits.
    fn reopens(cx: &mut TestAppContext, view: &Entity<ScriptView>) -> Rc<RefCell<Vec<Reopen>>> {
        let got: Rc<RefCell<Vec<Reopen>>> = Rc::default();
        let g = got.clone();
        cx.update(|cx| cx.subscribe(view, move |_, e: &Reopen, _| g.borrow_mut().push(e.clone())).detach());
        got
    }

    #[test]
    fn the_card_and_the_blank_lines_show_the_original() {
        let p = Pair::new("card");
        let opened = Opened::load(&p.at("film.es.fdx"), &[p.at("tr.review.txt")]).unwrap();
        let pair = opened.pair.loaded().unwrap();
        let session = &opened.opened.session;
        let item = &session.reviews[0].items[0];
        let card = row_card(&pair.links, item.row.as_ref().unwrap());
        assert_eq!(
            card,
            RowCard {
                label: "English".into(),
                version: "Español".into(),
                text: "I am going to town tomorrow.".into(),
                blank: "empty \u{2014} not yet translated",
            }
        );
        // The blank dialogue reads the English, dimmed; the sidebar too.
        let ghosts = pair_ghosts(&opened.pair);
        assert_eq!(ghosts.get(&item.para).map(String::as_str), Some("I am going to town tomorrow."));
        assert_eq!(blank_item_text(session, &opened.opened.doc, 0, 0), "I am going to town tomorrow.");
        // The review's name says what it is.
        assert_eq!(review_title(Path::new("/x/Translation from English.review.txt")), "Translation from English");
        assert_eq!(review_title(Path::new("/x/PURPLE LIST.review.txt")), "PURPLE LIST review");
        // Nothing on the original.
        let original = Opened::load(&p.at("film.fdx"), &[]).unwrap();
        assert!(pair_ghosts(&original.pair).is_empty());
    }

    #[test]
    fn suggestions_not_placed_on_rows_are_told_in_plain_words() {
        assert_eq!(
            row_note("the English line changed since this suggestion", 1),
            "1 suggestion is for an English line that changed since: ask Claude to translate that row again."
        );
        assert_eq!(
            row_note("the English line changed since this suggestion", 2),
            "2 suggestions are for English lines that changed since: ask Claude to translate those rows again."
        );
        assert_eq!(
            row_note("its line was changed, split or joined since this suggestion", 1),
            "1 suggestion isn't shown: its line was changed, split or joined since this suggestion."
        );
        // Opened after the English changed: the status bar says so.
        let p = Pair::new("not-placed");
        std::fs::write(p.at("film.fdx"), SCRIPT.replace("going to town", "going to the city")).unwrap();
        let opened = Opened::load(&p.at("film.es.fdx"), &[p.at("tr.review.txt")]).unwrap();
        assert!(
            opened.notices.iter().any(|n| n.ends_with(
                "1 suggestion is for an English line that changed since: ask Claude to translate that row again."
            )),
            "{:?}",
            opened.notices
        );
        assert!(!opened.notices.iter().any(|n| n.contains("scriptorium review")), "{:?}", opened.notices);
    }

    #[test]
    fn opening_the_original_says_where_to_translate() {
        let p = Pair::new("original");
        let opened = Opened::load(&p.at("film.fdx"), &[]).unwrap();
        assert!(
            opened.notices.iter().any(|n| n == "Lined up with film.es.fdx: open that file to translate."),
            "{:?}",
            opened.notices
        );
        let opened = Opened::load(&p.at("film.es.fdx"), &[]).unwrap();
        assert!(!opened.notices.iter().any(|n| n.starts_with("Lined up")), "{:?}", opened.notices);
    }

    #[test]
    fn the_pair_is_loaded_again_only_when_its_files_change() {
        let p = Pair::new("watch");
        let opened = Opened::load(&p.at("film.es.fdx"), &[]).unwrap();
        let mut watch = PairWatch::of(&opened.pair, &p.dir, "film.es.fdx");
        assert_eq!(watch.files.len(), 2);
        assert!(!watch.changed(), "as loaded");
        // Written again with the same bytes: looked at, not loaded again.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(p.at("film.fdx"), SCRIPT).unwrap();
        std::fs::write(p.at("film.es.links.txt"), p.read("film.es.links.txt")).unwrap();
        assert!(!watch.changed());
        assert!(!watch.changed());
        // Final Draft saves the original.
        std::fs::write(p.at("film.fdx"), SCRIPT.replace("tomorrow", "today")).unwrap();
        assert!(watch.changed());
    }

    #[test]
    fn a_pair_made_or_saved_while_open_is_noticed() {
        // Without a pair: a links file coming into the folder.
        let dir = std::env::temp_dir().join(format!("scriptorium-app-version-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut watch = PairWatch::of(&PairState::None, &dir, "film.fdx");
        assert!(watch.files.is_empty());
        assert!(!watch.changed());
        std::fs::write(dir.join("film.es.links.txt"), "").unwrap();
        assert!(watch.changed());
        let _ = std::fs::remove_dir_all(&dir);

        // A pair that couldn't be used (the other member mid-save): its
        // file is watched, and it is tried again after a moment anyway.
        let p = Pair::new("unusable");
        let unusable = PairState::Unusable {
            links: Some(p.at("film.es.links.txt")),
            reason: "film.fdx can't be read".into(),
            maybe_made: true,
        };
        let mut watch = PairWatch::of(&unusable, &p.dir, "film.es.fdx");
        let watched: Vec<&Path> = watch.files.iter().map(|w| w.path.as_path()).collect();
        assert_eq!(watched, [p.at("film.es.links.txt").as_path(), p.at("film.fdx").as_path()]);
        assert!(!watch.changed());
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(p.at("film.fdx"), SCRIPT.replace("tomorrow", "today")).unwrap();
        assert!(watch.changed(), "Final Draft finished saving");
        let mut watch = PairWatch::of(&unusable, &p.dir, "film.es.fdx");
        watch.retry = Some((Instant::now(), 1));
        assert!(watch.changed(), "due a retry");
        assert_eq!(watch.retry.map(|(_, n)| n), Some(2));
    }

    #[gpui::test]
    fn write_in_place_keeps_an_agents_append_and_reopens_the_written_file(cx: &mut TestAppContext) {
        let p = Pair::new("write");
        let (w, view) = open(cx, &p);
        let got = reopens(cx, &view);
        w.update(cx, |v, window, cx| {
            assert!(v.pair.is_made());
            assert_eq!(v.row_card_for(0, 0).unwrap().label, "English");
            v.accept(0, window, cx);
            // An autosave is due when the writer writes.
            v.schedule_autosave(cx);
        })
        .unwrap();
        // An agent appends a suggestion for the heading (kept in English in
        // a new version) just before.
        p.append(&p.entry("EXT. FARM HOUSE - NIGHT", "EXT. GRANJA - NOCHE", "EXT. FARM HOUSE - NIGHT"));
        w.update(cx, |v, window, cx| v.apply_to_fdx(&ApplyToFdx, window, cx)).unwrap();
        cx.run_until_parked();

        let reopen = got.borrow().clone();
        assert_eq!(reopen.len(), 1, "{reopen:?}");
        assert_eq!(reopen[0].path, p.at("film.es.fdx"));
        assert_eq!(reopen[0].notice, "Wrote film.es.fdx. Undo starts again from here.");
        let es = p.read("film.es.fdx");
        assert!(es.contains("<Text>Mañana voy al pueblo.</Text>"), "{es}");
        let links = scriptorium_versions::links::parse(&p.read("film.es.links.txt")).unwrap();
        assert_eq!(links.members[1].hash, sessionfile::hash(&es));
        // The review holds the decision and the agent's entry.
        let review = p.read("tr.review.txt");
        assert!(review.contains("STATUS: accepted"), "{review}");
        assert!(review.contains("EXT. GRANJA - NOCHE"), "{review}");

        // The old view never saves again: the autosave and the watch are off.
        let session = p.read("film.es.scriptorium.json");
        cx.executor().advance_clock(Duration::from_secs(5));
        cx.run_until_parked();
        assert_eq!(p.read("film.es.scriptorium.json"), session);
        view.read_with(cx, |v, _| assert!(v.retired));

        // Reopened: the decision is back and the appended suggestion placed.
        let opened = Opened::load(&reopen[0].path, &[]).unwrap();
        let s = &opened.opened.session;
        assert_eq!(s.reviews.len(), 1);
        let r = &s.reviews[0];
        assert_eq!(r.items.len(), 2, "unmatched: {:?}", r.unmatched);
        let doc = &opened.opened.doc;
        let statuses: Vec<Status> = (0..2).map(|i| s.status(doc, 0, i)).collect();
        assert!(statuses.contains(&Status::Accepted(0)), "{statuses:?}");
        assert!(statuses.contains(&Status::Pending), "{statuses:?}");
    }

    #[gpui::test]
    fn a_refused_write_says_why_and_the_view_goes_on(cx: &mut TestAppContext) {
        let p = Pair::new("refused");
        let (w, view) = open(cx, &p);
        let got = reopens(cx, &view);
        w.update(cx, |v, window, cx| v.accept(0, window, cx)).unwrap();
        // Final Draft saved the version meanwhile.
        let opened_raw = p.read("film.es.fdx");
        let es = opened_raw.replace("<Text></Text>", "<Text>Hola.</Text>");
        std::fs::write(p.at("film.es.fdx"), &es).unwrap();
        w.update(cx, |v, window, cx| v.apply_to_fdx(&ApplyToFdx, window, cx)).unwrap();
        cx.run_until_parked();
        assert!(got.borrow().is_empty());
        assert_eq!(p.read("film.es.fdx"), es, "nothing written");
        view.read_with(cx, |v, _| {
            assert!(!v.retired);
            assert!(v.error.contains("film.es.fdx changed since you opened it"), "{}", v.error);
        });
        // Its work was saved first, its decision written into the review
        // too (C2), and saving goes on (the watch is back).
        let saved = SessionFile::from_json(&p.read("film.es.scriptorium.json")).unwrap();
        assert!(saved.is_for(&opened_raw));
        assert!(p.read("tr.review.txt").contains("STATUS: accepted"));
        view.read_with(cx, |v, _| assert!(v.autosave.is_some(), "saving as usual"));
        w.update(cx, |v, window, cx| v.undo_redo(false, window, cx)).unwrap();
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        assert!(!p.read("tr.review.txt").contains("STATUS: accepted"), "saved after the refusal");
    }

    /// The agent's `new_version` on the original is refused while it reads
    /// otherwise here than in its file (a version is made from the file).
    #[gpui::test]
    fn a_version_is_made_only_from_the_original_as_saved(cx: &mut TestAppContext) {
        let p = Pair::new("unwritten");
        std::fs::write(
            p.at("en.review.txt"),
            "SCRIPT REVIEW v1\nScript: film.fdx\nId: en-1\n\n---\nBEFORE: I am going to town tomorrow.\nAFTER: I go \
             to town tomorrow.\n",
        )
        .unwrap();
        cx.update(gpui_base::init);
        let opened = Opened::load(&p.at("film.fdx"), &[p.at("en.review.txt")]).unwrap();
        let w = cx.add_window(|window, cx| ScriptView::new(opened, window, cx));
        w.update(cx, |v, window, cx| {
            assert_eq!(v.unwritten_changes(cx), None);
            v.accept(0, window, cx);
            let why = v.unwritten_changes(cx).unwrap();
            assert!(why.starts_with("film.fdx has changes in Scriptorium that aren't in the file"), "{why}");
        })
        .unwrap();
    }

    #[gpui::test]
    fn the_pair_is_loaded_again_when_the_original_changes(cx: &mut TestAppContext) {
        let p = Pair::new("reload");
        let (_w, view) = open(cx, &p);
        view.read_with(cx, |v, _| assert!(v.session.reviews[0].unmatched.is_empty()));
        // Final Draft saves the English, the dialogue reworded.
        let en = SCRIPT.replace("I am going to town tomorrow.", "I am going to the city tomorrow.");
        std::fs::write(p.at("film.fdx"), &en).unwrap();
        cx.executor().advance_clock(WATCH_EVERY * 2);
        cx.run_until_parked();
        view.read_with(cx, |v, _| {
            assert_eq!(v.pair.loaded().unwrap().other_hash, sessionfile::hash(&en));
            // The suggestion was made for the old line: no longer placed.
            let r = &v.session.reviews[0];
            assert!(r.items.is_empty(), "{:?}", r.items);
            assert!(r.unmatched[0].why.is_some());
            assert!(v.notice.contains("film.fdx changed outside Scriptorium"), "{}", v.notice);
        });
    }
}
