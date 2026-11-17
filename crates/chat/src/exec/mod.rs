//! Running the agent's tool calls against the script: the one path both the
//! CLI and the app take, so a thread's review is created, named, re-attached,
//! re-checked and written identically whichever program runs the thread.
//!
//! The program supplies the files through two traits:
//!
//! - [`Script`] opens the script as it is now, once per tool call. A call
//!   that may change files ([`scriptorium_tools::writes`]: adding
//!   suggestions, making a version) opens it for writing: the store then holds the session lock (or whatever the
//!   program uses instead) until it is dropped.
//! - [`ReviewStore`] is one such opening: the live document and session, and
//!   the few file operations the apply path needs (attach, create, reload,
//!   write). The CLI's is [`FileStore`] (over [`scriptorium_fsio::opened::Opened`]);
//!   the app's `LiveStore` borrows its open script view (see the rules on
//!   [`ReviewStore`]).
//!
//! [`crate::host::run`] doesn't see either: it runs calls through
//! [`crate::host::Tools`]. [`Direct`] is the `Tools` over a `Script` in the
//! same thread (the CLI: `Direct(FileScript)`); the app's `Tools` sends each
//! call to its UI thread, which runs [`run_call`] over a `LiveScript` inside
//! the view's `update`, and blocks on the answer.
//!
//! Everything else is decided here, once: which review a call means by
//! default ([`thread_review`]), where a suggestion goes ([`apply_append`]:
//! the thread's own review `thread-<id>`, re-attached from
//! [`ThreadFile::review_path`] after a detach and created as
//! `<title>.review.txt` only when it has none), the check that a review's
//! file hasn't changed on disk since it was read, the write, and whether to
//! ask before sending the whole script ([`whole_script_gate`]).

mod file;

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use scriptorium_agent::file::ThreadFile;
use scriptorium_agent::{ToolCall, ToolResult};
use scriptorium_core::{Document, ElementKind, ParaId, Script as OriginalScript};
use scriptorium_fsio::opened::{agent_review, new_review_id, not_a_review, review_salt};
use scriptorium_fsio::ops::{Choice, SuggestionRef};
use scriptorium_fsio::{Folders, agent_file_in_folder};
use scriptorium_providers::Price;
use scriptorium_review::file::{Case, FORMAT_HEADER, insert_id, is_review_text, parse_typed_lines};
use scriptorium_review::sessionfile::{resolve_path, stored_path};
use scriptorium_review::{Line, Review, ReviewFile, RowResolver, Session};
use scriptorium_tools::{Append, Applied, Ctx, Failed, LinksCtx, Outcome, Target};
use scriptorium_versions::PairView;
use serde_json::Value;

use crate::naming;

pub use file::{FileScript, FileStore, new_version_result};

/// The tool result when the script's files stay busy (another program holds
/// the session lock).
pub const BUSY: &str = "The script's files are busy in another program; try again.";

/// The reason a proposal on the line the writer is rewriting is refused
/// ([`ReviewStore::editing_paras`]).
pub const EDITING_LINE: &str = "The writer is editing this line right now; propose it later.";

/// Why a store couldn't do what was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewError {
    /// The script's files are busy (the session lock is held elsewhere, or
    /// the app is rewriting a line). Not told to the model at once: the call
    /// comes back with [`CallDone::busy`], and [`crate::host::run`] runs it
    /// again every [`BUSY_RETRY`] for up to [`BUSY_FOR`]; only then is the
    /// result [`BUSY`].
    Busy,
    /// [`ReviewStore::create`]: a file already has that name. The apply path
    /// picks the next name and tries again.
    Exists,
    /// Anything else, told to the model as the tool result.
    Failed(String),
}

impl fmt::Display for ReviewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReviewError::Busy => f.write_str(BUSY),
            ReviewError::Exists => f.write_str("a file with that name already exists"),
            ReviewError::Failed(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ReviewError {}

/// The script opened for one tool call: what the tools read, and the file
/// operations the apply path needs. Paths are absolute.
///
/// Every review a store attaches or creates must be named by its stored path
/// ([`stored_path`] of the canonical path from [`Self::dir`]), as the session
/// file stores it: tool results quote `Review::name`, so the CLI and the app
/// give the model the same text only if they name reviews alike.
///
/// The rules for the app's `LiveStore` (over its open script view):
///
/// - [`Self::write`] writes only the review file, then marks the session
///   dirty for the view's autosave: the session file isn't written here.
/// - [`Self::reload`] returns [`ReviewError::Busy`] while the writer is
///   rewriting a line (the session can't be swapped under an edit); the call
///   is retried, off the UI thread.
/// - A review's `text()` must equal its file on disk byte for byte whenever
///   the store is opened: the apply path compares the two to find files
///   another program changed (and then reloads), so a store that keeps
///   unsaved review text would reload over it.
pub trait ReviewStore {
    /// The script's folder, canonical: review paths are stored relative to it.
    fn dir(&self) -> &Path;
    /// The script's file name, e.g. `film.fdx`.
    fn script_name(&self) -> &str;
    /// The script as it is now.
    fn doc(&self) -> &Document;
    fn session(&self) -> &Session;
    /// Element kinds the script shows in capitals.
    fn is_caps(&self) -> &dyn Fn(&ElementKind) -> bool;
    /// The session to add suggestions to, with the document and caps (one
    /// call, so all three can be borrowed together).
    #[allow(clippy::type_complexity)]
    fn session_mut(&mut self) -> (&mut Session, &Document, &dyn Fn(&ElementKind) -> bool);

    /// The resolver placing `ROW:` suggestions (rows of a pair of versions)
    /// on the document as it is now, built from the pair the store opened
    /// with (`Opened::row_resolver` in fsio). `None`: no pair, or this is
    /// the original; `ROW:` suggestions are then not placed.
    fn row_resolver(&self) -> Option<Box<RowResolver<'static>>> {
        None
    }

    /// The pair of versions the store opened with, its rows laid out over
    /// the document as it is now, for the tools' `Ctx::links`
    /// (`list_versions`, `read_rows`): `None` without one, the reason when a
    /// links file names the script but can't be used. Over fsio's pair:
    /// `Opened::pair_view`, or `PairState::view` with the store's own session.
    fn pair_view(&self) -> Option<Result<PairView<'_>, String>> {
        None
    }

    /// Whether the script may be the version of its pair (fsio's
    /// `PairState::maybe_made`): a links file naming it that can't be used
    /// then stops suggestions on it, since they are placed by its rows. The
    /// default says yes, refusing rather than writing suggestions that could
    /// be lost.
    fn may_be_version(&self) -> bool {
        true
    }

    /// `new_version`: make a version of the script in `language` beside it
    /// (or find the one made before), the script's own language being
    /// `original_language` (`scriptorium_fsio::versions::version_new`, under
    /// the session lock this store holds); the tool result
    /// ([`new_version_result`], `switch` passed on: the caller is an outside
    /// agent). The default refuses: only a store over the script's files
    /// makes one.
    fn new_version(&mut self, language: &str, original_language: &str, switch: bool) -> Result<String, ReviewError> {
        let _ = (language, original_language, switch);
        Err(ReviewError::Failed(format!(
            "a version can't be made here: the writer makes it with `scriptorium version new {} --lang <code>`",
            self.script_name()
        )))
    }

    /// The lines the writer is rewriting right now (the app's; every line
    /// of a block being rewritten): a proposal touching any of them is
    /// refused with [`EDITING_LINE`]; the rest of a batch goes on. The CLI
    /// has none.
    fn editing_paras(&self) -> Vec<ParaId> {
        vec![]
    }

    /// The file at `path` as it is on disk now, `None` if it can't be read.
    fn disk_text(&self, path: &Path) -> Option<String> {
        fs::read_to_string(path).ok()
    }
    /// Whether the agent may write into the review file at `path`: only a
    /// review inside [`Self::dir`] ([`scriptorium_fsio::opened::agent_may_write`]),
    /// so a session listing a file elsewhere, or one that isn't a review,
    /// can't aim the agent's writes at it.
    fn may_write(&self, path: &Path) -> Result<(), ReviewError> {
        scriptorium_fsio::opened::agent_may_write(self.dir(), path).map_err(ReviewError::Failed)
    }
    /// The names of the files in [`Self::dir`], for picking a review file
    /// name that isn't taken.
    fn file_names(&self) -> Result<Vec<String>, ReviewError> {
        list_names(self.dir())
    }

    /// Read the session and its reviews again from disk (another program
    /// changed a review file since they were read). Review indices may change.
    fn reload(&mut self) -> Result<(), ReviewError>;
    /// Attach the existing review file at `path` (its `Id:` pinned if it has
    /// none, as `reviews add` does); returns its index.
    fn attach(&mut self, path: &Path) -> Result<usize, ReviewError>;
    /// Create `path` holding `text` (a new, empty review), failing with
    /// [`ReviewError::Exists`] if anything is there (`create_new`: never
    /// overwrite), and attach it; returns its index.
    fn create(&mut self, path: &Path, text: &str) -> Result<usize, ReviewError>;
    /// Write review `r`'s new file text, then record the session (the CLI
    /// saves the session file, which also saves an attach; the app marks its
    /// session dirty).
    fn write(&mut self, r: usize, text: String) -> Result<(), ReviewError>;

    /// Add `review`, a new review whose file isn't written yet (an outside
    /// agent's `review_file` that doesn't exist: written with its first
    /// suggestions, or taken out of the session again if there are none);
    /// returns its index. The default adds it to [`Self::session_mut`]'s
    /// session. The app's `LiveStore` overrides it to also record the path
    /// in its `attached_since_sync`, so a reload before the session is saved
    /// doesn't drop the review.
    fn add_new(&mut self, review: Review) -> Result<usize, ReviewError> {
        self.session_mut().0.add(review).map_err(ReviewError::Failed)
    }

    // The host operations: the tools only outside agents are offered
    // ([`scriptorium_tools::AGENT_ONLY`]), each run by [`run_call`] for
    // [`Caller::Agent`] on a store opened for writing. Each returns the
    // tool result, the text `scriptorium_fsio::ops` gives, so every host
    // answers alike. The defaults refuse ([`not_offered`]).

    /// `attach_reviews`: attach these review files (each already checked:
    /// an existing review inside [`Self::dir`], canonical) and save; the
    /// text of `ops::attach`.
    fn attach_reviews(&mut self, files: &[PathBuf]) -> Result<String, ReviewError> {
        let _ = files;
        Err(not_offered("attach_reviews"))
    }
    /// `detach_reviews`: detach these files (each as attached, or absolute:
    /// [`resolve_in`]) and save; `ops::detach`.
    fn detach(&mut self, files: &[String]) -> Result<String, ReviewError> {
        let _ = files;
        Err(not_offered("detach_reviews"))
    }
    /// `decide_suggestion`: decide suggestion `n` of `review` (an attached
    /// review's name or file, or a review file inside [`Self::dir`]; `None`:
    /// the only attached review) and save; `ops::decide`.
    fn decide(&mut self, review: Option<&str>, n: SuggestionRef, choice: Choice) -> Result<String, ReviewError> {
        let _ = (review, n, choice);
        Err(not_offered("decide_suggestion"))
    }
    /// `edit_paragraph`: replace paragraph `para`'s text and save; `ops::edit`.
    fn edit(&mut self, para: u64, text: &str) -> Result<String, ReviewError> {
        let _ = (para, text);
        Err(not_offered("edit_paragraph"))
    }
    /// `undo` / `redo`: the document's last step, whoever made it (the
    /// writer's or an agent's), and save; `ops::undo_redo`.
    fn undo_redo(&mut self, redo: bool) -> Result<String, ReviewError> {
        Err(not_offered(if redo { "redo" } else { "undo" }))
    }
    /// `write_script`: write the script to `output` as the agent named it
    /// (`None`: the default output), only where an agent may write
    /// (`ops::agent_output`); `ops::write`.
    fn write_script(&mut self, output: Option<&str>) -> Result<String, ReviewError> {
        let _ = output;
        Err(not_offered("write_script"))
    }
}

/// A host operation's default answer: this store doesn't offer `tool`.
pub fn not_offered(tool: &str) -> ReviewError {
    ReviewError::Failed(format!("{tool} isn't offered here"))
}

/// A file an agent named for `detach_reviews`, relative to the script's
/// folder `dir`: as given when absolute, else joined to `dir` (never the
/// process's working folder, which `ops::detach` would use).
pub fn resolve_in(dir: &Path, file: &str) -> String {
    if Path::new(file).is_absolute() {
        return file.to_string();
    }
    dir.join(file).to_string_lossy().into_owned()
}

/// A leading `~` or `~/` in a path an agent names, as `$HOME` (models write
/// `~/Scripts/x.fdx`, and no shell expands it). Anything else, `~name`
/// included, as given.
pub fn expand_home(path: &str) -> String {
    expand_home_with(path, std::env::var("HOME").ok().as_deref())
}

fn expand_home_with(path: &str, home: Option<&str>) -> String {
    match (home, path.strip_prefix('~')) {
        (Some(h), Some("")) if !h.is_empty() => h.to_string(),
        (Some(h), Some(rest)) if !h.is_empty() && rest.starts_with('/') => format!("{}{rest}", h.trim_end_matches('/')),
        _ => path.to_string(),
    }
}

/// Opens the script for one tool call.
pub trait Script {
    /// The script as it is now. `writes`: the call may add suggestions, so
    /// the store holds the session lock until dropped; a busy lock is
    /// [`ReviewError::Busy`]. A read-only opening never writes.
    fn open(&mut self, writes: bool) -> Result<Box<dyn ReviewStore + '_>, ReviewError>;
    /// Things opening the script turned up to tell the writer (a review file
    /// that can't be read…), each once; the run loop shows them as notices.
    fn take_notes(&mut self) -> Vec<String> {
        vec![]
    }
    /// `detach_reviews` ([`ReviewStore::detach`]), `files` as the agent
    /// named them, relative to the script's folder. The default opens the
    /// script for writing; the CLI's [`FileScript`] edits the session file
    /// without opening it, so a session that can't be replayed can still
    /// lose a review.
    fn detach(&mut self, files: &[String]) -> Result<String, ReviewError> {
        let mut store = self.open(true)?;
        let files: Vec<String> = files.iter().map(|f| resolve_in(store.dir(), f)).collect();
        store.detach(&files)
    }
}

/// The names of the files in `dir`.
pub fn list_names(dir: &Path) -> Result<Vec<String>, ReviewError> {
    let entries = fs::read_dir(dir).map_err(|e| ReviewError::Failed(format!("{}: {e}", dir.display())))?;
    Ok(entries.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
}

/// Creates `path` with `text`, never replacing a file that's there:
/// [`ReviewError::Exists`] if one is. For [`ReviewStore::create`].
pub fn create_new(path: &Path, text: &str) -> Result<(), ReviewError> {
    use std::io::Write;
    let failed = |e: io::Error| ReviewError::Failed(format!("{}: {e}", path.display()));
    let mut f = match fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(ReviewError::Exists),
        Err(e) => return Err(failed(e)),
    };
    f.write_all(text.as_bytes()).and_then(|()| f.sync_all()).map_err(failed)
}

/// The thread's own review in `store`'s session, if it is attached: the
/// review with id `thread-<id>`, else (once the thread has had one, so
/// [`ThreadFile::review_id`] is set) the one stored at its
/// [`ThreadFile::review_path`]. A path only planned ([`plan_review_path`])
/// never matches. What read tools default to; never "the only attached
/// review".
pub fn thread_review(store: &dyn ReviewStore, file: &ThreadFile) -> Option<usize> {
    let session = store.session();
    let id = naming::review_id(&file.id);
    session.reviews.iter().position(|r| r.id == id).or_else(|| {
        file.review_id.as_ref()?;
        let path = file.review_path.as_deref()?;
        scriptorium_tools::find_review(session, path)
    })
}

/// The file name the thread's review will be created under, for a thread
/// that has none yet ([`ThreadFile::review_path`] unset): `<title>.review.txt`
/// in `dir`, or the next free name. [`crate::host::run`] stores it in the
/// thread and saves the thread *before* a call that may create the review,
/// so a crash between creating the file and saving the thread leaves the
/// thread pointing at it. `None` when the thread already has a path.
pub fn plan_review_path(dir: &Path, file: &ThreadFile) -> Option<String> {
    if file.review_path.is_some() {
        return None;
    }
    let taken = list_names(dir).unwrap_or_default();
    Some(naming::review_file_name(&file.title, &taken))
}

/// The new, empty review file for thread `thread_id` of the script named
/// `script_name`: the format line, `Script:` and `Id: thread-<id>`.
pub fn new_review_text(script_name: &str, thread_id: &str) -> String {
    insert_id(&format!("{FORMAT_HEADER}\nScript: {script_name}\n\n"), &naming::review_id(thread_id))
}

/// How many names [`apply_append`] tries for a new review before giving up.
const NAME_TRIES: usize = 100;

/// Who a tool call is run for.
#[derive(Debug)]
pub enum Caller<'a> {
    /// A built-in agent thread (the in-app chat, `scriptorium chat`): reads
    /// default to the thread's own review ([`thread_review`]), and
    /// suggestions with no review named go into it ([`Target::Own`]),
    /// created when it has none.
    Thread(&'a mut ThreadFile),
    /// An outside agent (MCP, the CLI `tool`), offered
    /// [`scriptorium_tools::Offer::Agent`]: the default review is the only
    /// attached one, as the CLI `tool` has it; with several or none, a
    /// suggestion must name its `review_file` ([`NO_REVIEW`] otherwise).
    /// Never creates a thread's review.
    Agent,
}

/// The tool result when an outside agent ([`Caller::Agent`]) proposes
/// without a `review_file` while several reviews, or none, are attached.
pub const NO_REVIEW: &str =
    "no review to add to (several or none are attached): give review_file, the review file to add to";

/// Adds the suggestions in `append` to the review its target names, for
/// `caller`, and writes it: the one apply path.
///
/// 1. The review. [`Target::Own`] is the thread's review (an outside agent
///    has none: [`NO_REVIEW`]): attached, it is
///    used; else the file at [`ThreadFile::review_path`] is re-attached if
///    it's still there and still the thread's (its `Id:`); else any
///    `*.review.txt` in the folder with `Id: thread-<id>` (made by a run that
///    crashed before saving the thread); never a second file. Else it is
///    created, under [`ThreadFile::review_path`] when that is planned and
///    free, else `<title>.review.txt` (`" 2"`, `" 3"`… only when the name is
///    taken by another file), with `Id: thread-<id>`. The thread records the
///    review's id and stored path. [`Target::Review`] is found by its id;
///    [`Target::File`] by path ([`file_review`]): attached if it isn't, or
///    a new review added to the session in memory, its file (and folder)
///    written only with the suggestions.
/// 2. A review that was already attached is compared with its file on disk;
///    if another program changed the file, the session is read again first
///    ([`scriptorium_tools::apply`] re-validates against what's there).
/// 3. [`scriptorium_tools::apply`], then [`ReviewStore::write`] when it
///    produced new text, or when an existing file was attached here (so the
///    attach is saved even if every suggestion was refused). A new review
///    with nothing to write is taken out of the session again: never
///    written empty.
///
/// [`ReviewError::Busy`] is to be retried; any other `Err` is the tool
/// result (an error).
pub fn apply_append(store: &mut dyn ReviewStore, caller: Caller<'_>, append: Append) -> Result<Applied, ReviewError> {
    apply_named(store, caller, None, append).map(|(applied, _)| applied)
}

/// How the review a call adds to came to be in the session.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Got {
    /// It was attached before the call.
    Attached,
    /// An existing file attached (or created) by this call: written even
    /// with nothing new, so the attach is saved.
    AttachedHere,
    /// An existing file attached by this call for an outside agent: as the
    /// CLI `tool` does, it stays attached only if suggestions are written
    /// into it; else it is taken out of the session again (and a file store
    /// saves nothing).
    AttachedIfWritten,
    /// A review added to the session in memory by this call, its file at
    /// this path not written yet (nor, maybe, its folder).
    New(PathBuf),
}

/// [`apply_append`], with the `review_file` as the model gave it (`named`,
/// for [`Target::File`]; else the target's path); also the review's id.
fn apply_named(
    store: &mut dyn ReviewStore,
    caller: Caller<'_>,
    named: Option<&str>,
    mut append: Append,
) -> Result<(Applied, String), ReviewError> {
    let (r, got) = resolve(store, caller, &append.target, named)?;
    // A new review was checked where it will be ([`file_review`]); its file
    // isn't there yet to read.
    if !matches!(got, Got::New(_)) {
        store.may_write(&resolve_path(store.dir(), &store.session().reviews[r].path))?;
    }
    let editing = store.editing_paras();
    if !editing.is_empty() {
        refuse_editing(&mut append, &editing);
    }
    let result = {
        let rows = store.row_resolver();
        let (session, doc, caps) = store.session_mut();
        scriptorium_tools::apply(session, doc, r, append, caps, rows.as_deref())
    };
    let id = store.session().reviews[r].id.clone();
    let text = match (&result, &got) {
        (Ok(applied), _) if applied.file_text.is_some() => applied.file_text.clone(),
        (_, Got::AttachedHere) => Some(store.session().reviews[r].text().to_string()),
        _ => None,
    };
    match (text, &got) {
        (Some(text), Got::New(at)) => {
            // Its folder is made only now that there is something to write.
            if let Some(parent) = at.parent()
                && let Err(e) = fs::create_dir_all(parent)
            {
                forget(store, r);
                return Err(ReviewError::Failed(format!("can't create the folder {}: {e}", parent.display())));
            }
            if let Err(e) = store.write(r, text) {
                forget(store, r);
                return Err(e);
            }
        }
        (Some(text), _) => store.write(r, text)?,
        (None, Got::New(_) | Got::AttachedIfWritten) => forget(store, r),
        (None, _) => {}
    }
    match result {
        Ok(applied) => Ok((applied, id)),
        Err(e) => Err(ReviewError::Failed(e.to_string())),
    }
}

/// Takes review `r`, added by this call ([`Got::New`],
/// [`Got::AttachedIfWritten`]) and never written, out of the session again: a store that keeps its session (the
/// app's) must not list a review with no file.
fn forget(store: &mut dyn ReviewStore, r: usize) {
    let (session, _, _) = store.session_mut();
    if r < session.reviews.len() {
        session.reviews.remove(r);
    }
}

/// Moves the proposals that touch any of the lines `editing` to `append.failed`.
fn refuse_editing(append: &mut Append, editing: &[ParaId]) {
    let (refused, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut append.proposals)
        .into_iter()
        .partition(|p| p.entry.para.iter().any(|id| editing.contains(id)));
    append.proposals = kept;
    append.failed.extend(refused.into_iter().map(|p| Failed {
        index: p.index,
        paragraph_id: p.entry.para.first().map(|id| id.0),
        error: EDITING_LINE.into(),
    }));
}

/// The review `target` names, and how it came to be in the session.
/// `named`: a [`Target::File`]'s path as the model gave it.
fn resolve(
    store: &mut dyn ReviewStore,
    caller: Caller<'_>,
    target: &Target,
    named: Option<&str>,
) -> Result<(usize, Got), ReviewError> {
    match target {
        Target::Own => match caller {
            Caller::Thread(file) => {
                let (r, here) = own_review(store, file)?;
                Ok((r, if here { Got::AttachedHere } else { Got::Attached }))
            }
            Caller::Agent => Err(ReviewError::Failed(NO_REVIEW.into())),
        },
        Target::Review { index, id } => {
            let session = store.session();
            let r = match session.reviews.get(*index) {
                Some(r) if r.id == *id => *index,
                _ => session.reviews.iter().position(|r| r.id == *id).ok_or_else(|| {
                    ReviewError::Failed(format!("the review with id {id} isn't attached any more: call the tool again"))
                })?,
            };
            Ok((recheck(store, r)?, Got::Attached))
        }
        Target::File(path) => {
            let agent = matches!(caller, Caller::Agent);
            file_review(store, named.unwrap_or(path), agent)
        }
    }
}

/// The thread's own review: attached, re-attached, found by id, or created.
fn own_review(store: &mut dyn ReviewStore, file: &mut ThreadFile) -> Result<(usize, bool), ReviewError> {
    if let Some(r) = thread_review(store, file) {
        let r = recheck(store, r)?;
        record(store, file, r);
        return Ok((r, false));
    }
    let id = naming::review_id(&file.id);
    let mut found = None;
    // The thread file's path is checked like any other: a thread file
    // edited (or planted) to name a file elsewhere never re-attaches it.
    if let Some(stored) = &file.review_path
        && let Ok(at) = scriptorium_fsio::review_in_folder(store.dir(), stored)
        && store.disk_text(&at).is_some_and(|text| is_review_text(&text) && is_ours(&text, &id, file))
    {
        found = Some(at);
    }
    if found.is_none() {
        found = find_by_id(store, &id)?;
    }
    // Only a review is ever re-attached (attaching pins an `Id:` into the
    // file at once, in the app before any write check): checked above.
    if let Some(at) = found {
        let r = store.attach(&at)?;
        record(store, file, r);
        return Ok((r, true));
    }
    let text = new_review_text(store.script_name(), &file.id);
    let mut taken = store.file_names()?;
    let is_taken = |taken: &[String], name: &str| taken.iter().any(|t| t.to_lowercase() == name.to_lowercase());
    // The thread file's planned name is checked like a model's: a planted
    // `Makefile` or `.zshenv` is never created; a fresh name is used instead.
    let mut planned =
        file.review_path.clone().filter(|p| scriptorium_fsio::plain_review_name(p) && !is_taken(&taken, p));
    for _ in 0..NAME_TRIES {
        let name = planned.take().unwrap_or_else(|| naming::review_file_name(&file.title, &taken));
        let at = store.dir().join(&name);
        match store.create(&at, &text) {
            Ok(r) => {
                record(store, file, r);
                return Ok((r, true));
            }
            // Another program took the name, or the disk folds names the
            // naming doesn't: look again, and never pick this one twice.
            Err(ReviewError::Exists) => {
                taken = store.file_names()?;
                if !is_taken(&taken, &name) {
                    taken.push(name);
                }
            }
            Err(e) => return Err(e),
        }
    }
    Err(ReviewError::Failed("couldn't find a free file name for this conversation's review".into()))
}

/// Whether review file `text` is the thread's: its `Id:` is `id` (or the id
/// the thread recorded), or it has none and the thread has had its review
/// (a path merely planned never claims another program's file).
fn is_ours(text: &str, id: &str, file: &ThreadFile) -> bool {
    match ReviewFile::parse(text).id {
        Some(found) => found == id || file.review_id.as_deref() == Some(found.as_str()),
        None => file.review_id.is_some(),
    }
}

/// A `*.review.txt` in the script's folder whose `Id:` is `id`.
fn find_by_id(store: &dyn ReviewStore, id: &str) -> Result<Option<PathBuf>, ReviewError> {
    let mut names = store.file_names()?;
    names.sort();
    for name in names.iter().filter(|n| n.to_lowercase().ends_with(".review.txt")) {
        let at = store.dir().join(name);
        if store
            .disk_text(&at)
            .is_some_and(|text| is_review_text(&text) && ReviewFile::parse(&text).id.as_deref() == Some(id))
        {
            return Ok(Some(at));
        }
    }
    Ok(None)
}

/// A review file named by path (a [`Target::File`]: an outside agent's
/// `review_file`), `path` as the model gave it, the way the CLI `tool` has
/// always taken it. The built-in agent is never offered `review_file`
/// (`check_args` refuses it), so for threads this is defence in depth.
///
/// - Attached already (compared in the form the session stores,
///   [`review_location`]): used, re-checked against its file.
/// - Else it must be a `*.review.txt` inside the script's folder once links
///   are resolved, its folders there or creatable inside it
///   ([`agent_file_in_folder`], [`Folders::MayCreate`], as
///   `opened::agent_review` checks it, the file read through the store);
///   anything else is refused, never attached, created or written.
/// - A file that's there must already be a review ([`is_review_text`]): it
///   is attached; for an outside agent (`agent`), as the CLI `tool` has it,
///   only if suggestions are written into it ([`Got::AttachedIfWritten`]).
/// - Else a new review is added to the session in memory, its `Id:` salted
///   from the resolved path ([`review_salt`]); [`apply_named`] writes it
///   (and makes its folder) only when there is something to write.
fn file_review(store: &mut dyn ReviewStore, path: &str, agent: bool) -> Result<(usize, Got), ReviewError> {
    let stored = stored_path(store.dir(), &review_location(store.dir(), path));
    if let Some(r) = scriptorium_tools::find_review(store.session(), &stored) {
        return Ok((recheck(store, r)?, Got::Attached));
    }
    // `opened::agent_review`'s checks, reading the file through the store.
    let refuse = |e: String| ReviewError::Failed(format!("review_file {e}"));
    let at = agent_file_in_folder(store.dir(), path, ".review.txt", Folders::MayCreate).map_err(refuse)?;
    match store.disk_text(&at) {
        Some(text) if !is_review_text(&text) => return Err(refuse(not_a_review(path))),
        Some(_) => {
            let got = if agent { Got::AttachedIfWritten } else { Got::AttachedHere };
            return Ok((store.attach(&at)?, got));
        }
        // Something there that can't be read is never written over.
        None if fs::symlink_metadata(&at).is_ok() => return Err(refuse(format!("{path}: can't be read"))),
        None => {}
    }
    let header = format!("{FORMAT_HEADER}\nScript: {}\n\n", store.script_name());
    let text = insert_id(&header, &new_review_id(&header, &review_salt(&at)));
    let script_name = store.script_name().to_string();
    let rows = store.row_resolver();
    // A new review has no suggestions: nothing in it is placed against the
    // script as it was read, so an empty one stands in for it.
    let review = Review::open_live(
        stored.clone(),
        &text,
        &OriginalScript::default(),
        store.doc(),
        store.is_caps(),
        &script_name,
        rows.as_deref(),
    )
    .with_path(stored);
    let r = store.add_new(review)?;
    Ok((r, Got::New(at)))
}

/// Where review file `file` (relative to the script's folder `dir`, or
/// absolute) is, resolved through links as far as it exists: the form
/// [`stored_path`] makes the session's, so [`scriptorium_tools::find_review`]
/// sees an attached file as attached however it is spelt.
fn review_location(dir: &Path, file: &str) -> PathBuf {
    let p = resolve_path(dir, file);
    if let Ok(at) = p.canonicalize() {
        return at;
    }
    match (p.parent().map(Path::canonicalize), p.file_name()) {
        (Some(Ok(folder)), Some(name)) => folder.join(name),
        _ => p,
    }
}

/// Review `r` again if its file changed on disk since it was read: the
/// session is reloaded and the review found again by id.
fn recheck(store: &mut dyn ReviewStore, r: usize) -> Result<usize, ReviewError> {
    let review = &store.session().reviews[r];
    let (id, name) = (review.id.clone(), review.name.clone());
    let at = resolve_path(store.dir(), &review.path);
    let changed = store.disk_text(&at).is_some_and(|disk| disk != review.text());
    if !changed {
        return Ok(r);
    }
    store.reload()?;
    store
        .session()
        .reviews
        .iter()
        .position(|r| r.id == id)
        .ok_or_else(|| ReviewError::Failed(format!("{name} was detached meanwhile: call the tool again")))
}

/// Records review `r` as the thread's own: its id and stored path.
fn record(store: &dyn ReviewStore, file: &mut ThreadFile, r: usize) {
    let review = &store.session().reviews[r];
    file.review_id = Some(review.id.clone());
    file.review_path = Some(review.path.clone());
}

/// What a tool call did: its result for the model, and where suggestions
/// went if any were added (for the app's "show it" link on the tool row).
#[derive(Debug, Clone, PartialEq)]
pub struct CallDone {
    pub result: ToolResult,
    pub added: Option<Added>,
    /// The script's files were busy ([`ReviewError::Busy`]): nothing ran or
    /// was written. Run the call again later (off the UI thread);
    /// [`crate::host::run`] does, and gives the model `result` (saying
    /// [`BUSY`]) only after [`BUSY_FOR`].
    pub busy: bool,
}

impl CallDone {
    /// The result `content` for `call`.
    pub fn new(call: &ToolCall, content: impl Into<String>, is_error: bool) -> Self {
        CallDone {
            result: ToolResult { id: call.id.clone(), content: content.into(), is_error },
            added: None,
            busy: false,
        }
    }

    /// The error result for `e`, flagged [`Self::busy`] when it is.
    pub fn failed(call: &ToolCall, e: &ReviewError) -> Self {
        CallDone { busy: *e == ReviewError::Busy, ..CallDone::new(call, e.to_string(), true) }
    }
}

/// How often a busy call is tried again.
pub const BUSY_RETRY: std::time::Duration = std::time::Duration::from_millis(200);
/// How long a busy call is tried for before the model is told [`BUSY`].
pub const BUSY_FOR: std::time::Duration = std::time::Duration::from_secs(10);

/// Suggestions a call added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Added {
    /// The review's id (a thread's own: `thread-<id>`).
    pub review_id: String,
    /// Per proposal, its number as `list_suggestions` shows it, or `None`
    /// if it wasn't added ([`Applied::numbers`]).
    pub numbers: Vec<Option<usize>>,
}

/// Runs one tool call for `caller`: opens the script (for writing when the
/// tool may change files), runs the tool with the caller's default review
/// (a thread's own, [`thread_review`]; for an outside agent the only
/// attached review), and carries out what it asks with [`apply_append`].
/// Failures are error results for the model, never a stop. The
/// whole-script check is a thread host's, before this
/// ([`whole_script_gate`]).
///
/// The call is checked against what the caller is offered
/// ([`scriptorium_tools::Offer::Chat`] for a thread, `Agent` for an outside
/// agent). An outside agent's `script` is the host's (it picked the
/// [`Script`]) and is dropped; its `review_file` is passed to the tools in
/// the form the session stores, so an attached file is seen as attached.
pub fn run_call(script: &mut dyn Script, caller: Caller<'_>, call: &ToolCall) -> CallDone {
    let offer = match caller {
        Caller::Thread(_) => scriptorium_tools::Offer::Chat,
        Caller::Agent => scriptorium_tools::Offer::Agent,
    };
    // Only the arguments the host offered ([`scriptorium_tools::offered`]):
    // the tools read more than that (a thread isn't offered `review_file`).
    // A name that wasn't offered is refused here too, so a tool `call`
    // knows but the catalogue doesn't can never run unchecked.
    let spec = match scriptorium_tools::offered(offer, &call.name) {
        Ok(spec) => spec,
        Err(e) => return CallDone::new(call, e.to_string(), true),
    };
    if let Err(e) = scriptorium_tools::check_args(&spec, &call.input) {
        return CallDone::new(call, e.to_string(), true);
    }
    let mut input = call.input.clone();
    let mut named = None;
    if let (Caller::Agent, Some(map)) = (&caller, input.as_object_mut()) {
        map.remove("script");
        named = map.get("review_file").and_then(Value::as_str).map(expand_home);
    }
    if scriptorium_tools::AGENT_ONLY.contains(&call.name.as_str()) {
        // Only an outside agent is offered these (checked above).
        return match host_op(script, &call.name, &input) {
            Ok(text) => CallDone::new(call, text.trim_end(), false),
            Err(e) => CallDone::failed(call, &e),
        };
    }
    if call.name == "guide" {
        // Needs no script.
        return match scriptorium_tools::guidance::guide(&input) {
            Ok(text) => CallDone::new(call, text, false),
            Err(e) => CallDone::new(call, e.to_string(), true),
        };
    }
    let writes = scriptorium_tools::writes(&call.name);
    let agent = matches!(caller, Caller::Agent);
    let mut store = match script.open(writes) {
        Ok(s) => s,
        Err(e) => return CallDone::failed(call, &e),
    };
    if let Some(f) = &named {
        input["review_file"] = Value::String(stored_path(store.dir(), &review_location(store.dir(), f)));
    }
    let outcome = {
        let default_review = match &caller {
            Caller::Thread(file) => thread_review(&*store, file),
            Caller::Agent => (store.session().reviews.len() == 1).then_some(0),
        };
        let ctx = Ctx {
            doc: store.doc(),
            session: store.session(),
            script_name: store.script_name(),
            is_caps: store.is_caps(),
            default_review,
            links: scriptorium_tools::uses_links(&call.name)
                .then(|| match store.pair_view()? {
                    // A links file that can't be used stops suggestions
                    // only on a script that may be the version.
                    Err(_) if scriptorium_tools::may_append(&call.name) && !store.may_be_version() => None,
                    v => Some(v.map(LinksCtx::from)),
                })
                .flatten(),
        };
        scriptorium_tools::call(&ctx, &call.name, &input)
    };
    match outcome {
        Err(e) => CallDone::new(call, e.to_string(), true),
        Ok(Outcome::Text(text)) => CallDone::new(call, text, false),
        Ok(Outcome::Append(_) | Outcome::NewVersion(_)) if !writes => {
            CallDone::new(call, format!("{} can't change anything", call.name), true)
        }
        Ok(Outcome::NewVersion(v)) => match store.new_version(&v.language, &v.original_language, agent) {
            Ok(text) => CallDone::new(call, text, false),
            Err(e) => CallDone::failed(call, &e),
        },
        Ok(Outcome::Append(append)) => match apply_named(&mut *store, caller, named.as_deref(), append) {
            Ok((applied, review_id)) => {
                let added = applied
                    .numbers
                    .iter()
                    .any(Option::is_some)
                    .then(|| Added { review_id, numbers: applied.numbers.clone() });
                CallDone { added, ..CallDone::new(call, applied.message, false) }
            }
            Err(e) => CallDone::failed(call, &e),
        },
    }
}

/// A host operation (an [`scriptorium_tools::AGENT_ONLY`] tool) for an
/// outside agent: its arguments read (errors phrased for the model; paths
/// with `~` expanded) before anything is opened, then run on `script`
/// opened for writing. `attach_reviews` files are checked here, for every
/// store: existing reviews inside the script's folder
/// ([`agent_review`]).
fn host_op(script: &mut dyn Script, name: &str, input: &Value) -> Result<String, ReviewError> {
    let a = Args(input.as_object().cloned().unwrap_or_default());
    let failed = ReviewError::Failed;
    match name {
        "attach_reviews" | "detach_reviews" => {
            let files: Vec<String> = a.strs("files").map_err(failed)?.iter().map(|f| expand_home(f)).collect();
            if files.is_empty() {
                return Err(failed("give at least one file".into()));
            }
            if name == "detach_reviews" {
                return script.detach(&files);
            }
            let mut store = script.open(true)?;
            let mut checked = vec![];
            for f in &files {
                match agent_review(store.dir(), f, Folders::MustExist).map_err(failed)? {
                    (at, true) => checked.push(at),
                    (_, false) => return Err(failed(format!("{f}: no such file"))),
                }
            }
            store.attach_reviews(&checked)
        }
        "decide_suggestion" => {
            let review = a.opt_str("review").map(|r| expand_home(&r));
            let n = a.suggestion_ref("n").map_err(failed)?;
            let choice = match a.str("decision").map_err(failed)?.as_str() {
                "accept" => Choice::Accept(a.opt_positive("option").map_err(failed)?),
                "keep" => Choice::Keep,
                "rewrite" => match a.0.get("lines").filter(|v| !v.is_null()) {
                    Some(lines) => Choice::RewriteBlock(block_lines(lines).map_err(failed)?),
                    None => Choice::Rewrite(a.str("text").map_err(failed)?),
                },
                "undecide" => Choice::Undecide,
                d => return Err(failed(format!("unknown decision {d:?}: accept, keep, rewrite or undecide"))),
            };
            script.open(true)?.decide(review.as_deref(), n, choice)
        }
        "edit_paragraph" => {
            let id =
                a.0.get("paragraph_id")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| failed("missing `paragraph_id` (a paragraph id)".into()))?;
            let text = a.str("text").map_err(failed)?;
            script.open(true)?.edit(id, &text)
        }
        "undo" | "redo" => script.open(true)?.undo_redo(name == "redo"),
        "write_script" => {
            let output = a.opt_str("output").map(|o| expand_home(&o));
            script.open(true)?.write_script(output.as_deref())
        }
        _ => Err(failed(format!("unknown tool {name}"))),
    }
}

/// `lines` (`[{kind, text}]`, as `decide_suggestion` takes them) as lines;
/// `[]` is a cut.
fn block_lines(lines: &Value) -> Result<Vec<Line>, String> {
    let list = lines.as_array().ok_or("`lines` must be a list of {kind, text}")?;
    if list.is_empty() {
        return Ok(vec![]);
    }
    let mut typed = String::new();
    for l in list {
        let (Some(kind), Some(text)) = (l.get("kind").and_then(Value::as_str), l.get("text").and_then(Value::as_str))
        else {
            return Err("each line is {\"kind\": <element type>, \"text\": <text>}".into());
        };
        if [kind, text].iter().any(|s| s.contains(['\n', '\r', '\u{2028}', '\u{2029}'])) {
            return Err("a line can't contain a newline: give each paragraph as its own line".into());
        }
        if kind.contains(':') {
            return Err(format!("unknown element type {kind:?}"));
        }
        typed.push_str(&format!("{kind}: {text}\n"));
    }
    let lines = parse_typed_lines(&typed, Case::Any)?;
    Ok(lines.into_iter().map(|(k, t)| Line::typed(k, t)).collect())
}

/// A tool call's arguments, with errors phrased for the model.
struct Args(serde_json::Map<String, Value>);

impl Args {
    fn opt_str(&self, k: &str) -> Option<String> {
        self.0.get(k).and_then(Value::as_str).map(str::to_string)
    }
    fn str(&self, k: &str) -> Result<String, String> {
        self.opt_str(k).ok_or(format!("missing `{k}` (a string)"))
    }
    /// A whole number from 1, if given.
    fn opt_positive(&self, k: &str) -> Result<Option<usize>, String> {
        match self.0.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_u64()
                .filter(|n| *n > 0)
                .and_then(|n| usize::try_from(n).ok())
                .map(Some)
                .ok_or(format!("`{k}` must be a whole number from 1")),
        }
    }
    /// A suggestion number (from 1), or `e<entry>` for its stable place in
    /// the file. Nothing else: not `-1`, `2.5`, `"3"` or `"x"`.
    fn suggestion_ref(&self, k: &str) -> Result<SuggestionRef, String> {
        let bad = || format!("`{k}` must be a suggestion number from 1, or \"e<entry>\" (e.g. \"e3\")");
        match self.0.get(k) {
            Some(Value::Number(n)) => match n.as_u64().filter(|n| *n > 0).and_then(|n| usize::try_from(n).ok()) {
                Some(n) => Ok(SuggestionRef::Number(n)),
                None => Err(bad()),
            },
            Some(Value::String(s)) => match SuggestionRef::parse(s) {
                Some(r @ SuggestionRef::Entry(_)) => Ok(r),
                _ => Err(bad()),
            },
            _ => Err(bad()),
        }
    }
    fn strs(&self, k: &str) -> Result<Vec<String>, String> {
        let list = self.0.get(k).and_then(Value::as_array).ok_or(format!("missing `{k}` (a list of strings)"))?;
        list.iter().map(|v| v.as_str().map(str::to_string).ok_or(format!("`{k}` must be strings"))).collect()
    }
}

/// [`crate::host::Tools`] over a [`Script`] in the same thread: the CLI's
/// (`Direct(FileScript)`), and tests'. [`run_call`] and
/// [`whole_script_gate`], nothing else.
pub struct Direct<S>(pub S);

impl<S: Script> crate::host::Tools for Direct<S> {
    fn gate(
        &mut self,
        file: &ThreadFile,
        call: &ToolCall,
        allow: bool,
        price: Option<&Price>,
    ) -> Result<Gate, ReviewError> {
        whole_script_gate(&mut self.0, file, &call.name, allow, price)
    }
    fn call(&mut self, file: &mut ThreadFile, call: &ToolCall) -> CallDone {
        run_call(&mut self.0, Caller::Thread(file), call)
    }
    fn take_notes(&mut self) -> Vec<String> {
        self.0.take_notes()
    }
}

/// Above this many tokens, `read_script` asks first.
pub const WHOLE_SCRIPT_TOKENS: usize = 20_000;
/// Above this many US dollars of input at the thread's model, `read_script`
/// asks first.
pub const WHOLE_SCRIPT_DOLLARS: f64 = 0.10;

/// The tool result when the writer declined to send the whole script.
pub const WHOLE_SCRIPT_REFUSED: &str = "The writer chose not to send the whole script. Read the scenes you need \
with read_scene, or find lines with list_paragraphs.";

/// Whether a `read_script` call may run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gate {
    Run,
    /// Answer it with [`WHOLE_SCRIPT_REFUSED`] ([`refused`]).
    Refuse,
    /// Ask the writer ([`whole_script_question`]), store the answer with
    /// [`record_whole_script`], then run or refuse.
    Ask {
        tokens: usize,
        cost: Option<f64>,
    },
}

/// The key in [`ThreadFile::extra`] holding the script's size (tokens) when
/// the writer answered [`ThreadFile::whole_script`].
pub const WHOLE_SCRIPT_TOKENS_KEY: &str = "whole_script_tokens";

/// Records the writer's answer about sending the whole script, with the
/// script's size then (`tokens`): `Some(true)` allowed, `Some(false)` "Not
/// now", `None` forgets it (ask again next time). The app's "Allow" on a
/// refused call overrides a stored no with `Some(true)` (see
/// [`crate::host::set_whole_script`]).
pub fn record_whole_script(file: &mut ThreadFile, answer: Option<bool>, tokens: Option<usize>) {
    file.whole_script = answer;
    match (answer, tokens) {
        (Some(_), Some(t)) => {
            file.extra.insert(WHOLE_SCRIPT_TOKENS_KEY.into(), t.into());
        }
        _ => {
            file.extra.remove(WHOLE_SCRIPT_TOKENS_KEY);
        }
    }
}

/// The script's size when the stored answer was given, if recorded.
pub fn whole_script_tokens(file: &ThreadFile) -> Option<usize> {
    file.extra.get(WHOLE_SCRIPT_TOKENS_KEY)?.as_u64().map(|t| t as usize)
}

/// Whether tool call `name` may run now as far as the whole-script rule
/// goes: only `read_script` is checked. `allow`: the writer allowed it for
/// this run (`--allow-whole-script`), which overrides a stored "Not now".
/// Otherwise the thread's stored answer decides, until the script has grown
/// past twice the size it was answered for ([`whole_script_tokens`]), when
/// it asks again; with none, a script above [`WHOLE_SCRIPT_TOKENS`] or
/// [`WHOLE_SCRIPT_DOLLARS`] (input at `price`) asks.
pub fn whole_script_gate(
    script: &mut dyn Script,
    file: &ThreadFile,
    name: &str,
    allow: bool,
    price: Option<&Price>,
) -> Result<Gate, ReviewError> {
    if name != "read_script" || allow {
        return Ok(Gate::Run);
    }
    let stored = whole_script_tokens(file);
    if file.whole_script.is_some() && stored.is_none() {
        return Ok(if file.whole_script == Some(true) { Gate::Run } else { Gate::Refuse });
    }
    let store = script.open(false)?;
    let (_, tokens) = scriptorium_tools::script_size(store.doc(), store.is_caps());
    Ok(gate_with(file.whole_script, stored, tokens, price))
}

/// The whole-script rule for a script of `tokens` tokens, given the stored
/// `answer` and the size it was given for.
pub fn gate_with(answer: Option<bool>, answered_at: Option<usize>, tokens: usize, price: Option<&Price>) -> Gate {
    let grown = answered_at.is_some_and(|at| tokens > at.saturating_mul(2));
    match answer {
        Some(_) if grown => Gate::Ask { tokens, cost: cost(tokens, price) },
        Some(true) => Gate::Run,
        Some(false) => Gate::Refuse,
        None => gate_for(tokens, price),
    }
}

fn cost(tokens: usize, price: Option<&Price>) -> Option<f64> {
    price.map(|p| tokens as f64 * p.rates(tokens as u64).0 / 1_000_000.0)
}

/// [`Gate::Run`] or [`Gate::Ask`] for a script of `tokens` tokens.
pub fn gate_for(tokens: usize, price: Option<&Price>) -> Gate {
    let cost = cost(tokens, price);
    if tokens > WHOLE_SCRIPT_TOKENS || cost.is_some_and(|c| c > WHOLE_SCRIPT_DOLLARS) {
        Gate::Ask { tokens, cost }
    } else {
        Gate::Run
    }
}

/// "The agent wants to read the whole script (about 35k tokens, ≈ $0.18)."
pub fn whole_script_question(tokens: usize, cost: Option<f64>) -> String {
    let size = if tokens >= 1000 {
        format!("about {}k tokens", (tokens + 500) / 1000)
    } else {
        format!("about {tokens} tokens")
    };
    match cost {
        Some(c) => format!("The agent wants to read the whole script ({size}, ≈ ${c:.2})."),
        None => format!("The agent wants to read the whole script ({size})."),
    }
}

/// The result for a `read_script` call the writer declined.
pub fn refused(call: &ToolCall) -> ToolResult {
    ToolResult { id: call.id.clone(), content: WHOLE_SCRIPT_REFUSED.into(), is_error: true }
}

#[cfg(test)]
mod tests;
