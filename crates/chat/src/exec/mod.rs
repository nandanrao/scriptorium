//! Running the agent's tool calls against the script: the one path both the
//! CLI and the app take, so a thread's review is created, named, re-attached,
//! re-checked and written identically whichever program runs the thread.
//!
//! The program supplies the files through two traits:
//!
//! - [`Script`] opens the script as it is now, once per tool call. A call
//!   that may add suggestions ([`scriptorium_tools::may_append`]) opens it
//!   for writing: the store then holds the session lock (or whatever the
//!   program uses instead) until it is dropped.
//! - [`ReviewStore`] is one such opening: the live document and session, and
//!   the few file operations the apply path needs (attach, create, reload,
//!   write). The CLI's is [`FileStore`] (over [`scriptorium_fsio::opened::Opened`]);
//!   the app's `LiveStore` sits over its open script view.
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
use scriptorium_core::{Document, ElementKind};
use scriptorium_providers::Price;
use scriptorium_review::Session;
use scriptorium_review::file::{FORMAT_HEADER, insert_id};
use scriptorium_review::sessionfile::{resolve_path, stored_path};
use scriptorium_tools::{Append, Applied, Ctx, Outcome, Target};

use crate::naming;

pub use file::{FileScript, FileStore};

/// The tool result when the script's files stay busy (another program holds
/// the session lock).
pub const BUSY: &str = "The script's files are busy in another program; try again.";

/// Why a store couldn't do what was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewError {
    /// The script's files are busy in another program (the session lock is
    /// held elsewhere, or the app can't take changes right now). The app
    /// retries; the result told to the model is [`BUSY`].
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

    /// The file at `path` as it is on disk now, `None` if it can't be read.
    fn disk_text(&self, path: &Path) -> Option<String> {
        fs::read_to_string(path).ok()
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
    /// Write review `r`'s new file text, then record the session (for the
    /// CLI: save the session file, which also saves an attach).
    fn write(&mut self, r: usize, text: String) -> Result<(), ReviewError>;
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
/// review with id `thread-<id>`, else the one stored at the thread's
/// [`ThreadFile::review_path`]. What read tools default to; never "the only
/// attached review".
pub fn thread_review(store: &dyn ReviewStore, file: &ThreadFile) -> Option<usize> {
    let session = store.session();
    let id = naming::review_id(&file.id);
    session.reviews.iter().position(|r| r.id == id).or_else(|| {
        let path = file.review_path.as_deref()?;
        scriptorium_tools::find_review(session, path)
    })
}

/// The new, empty review file for thread `thread_id` of the script named
/// `script_name`: the format line, `Script:` and `Id: thread-<id>`.
pub fn new_review_text(script_name: &str, thread_id: &str) -> String {
    insert_id(&format!("{FORMAT_HEADER}\nScript: {script_name}\n\n"), &naming::review_id(thread_id))
}

/// How many names [`apply_append`] tries for a new review before giving up.
const NAME_TRIES: usize = 100;

/// Adds the suggestions in `append` to the review its target names, for
/// thread `file`, and writes it: the one apply path.
///
/// 1. The review. [`Target::Own`] is the thread's review: attached, it is
///    used; else the file at [`ThreadFile::review_path`] is re-attached if
///    it's still there (never a second file); else `<title>.review.txt` is
///    created (`" 2"`, `" 3"`… only when the name is taken by another file)
///    with `Id: thread-<id>`. The thread records the review's id and stored
///    path. [`Target::Review`] is found by its id; [`Target::File`] by path,
///    attached or created if it isn't attached.
/// 2. A review that was already attached is compared with its file on disk;
///    if another program changed the file, the session is read again first
///    ([`scriptorium_tools::apply`] re-validates against what's there).
/// 3. [`scriptorium_tools::apply`], then [`ReviewStore::write`] when it
///    produced new text, or when the review was attached here (so the attach
///    is saved even if every suggestion was refused).
///
/// The `Err` is the tool result (an error).
pub fn apply_append(store: &mut dyn ReviewStore, file: &mut ThreadFile, append: Append) -> Result<Applied, String> {
    let (r, attached_here) = resolve(store, file, &append.target).map_err(|e| e.to_string())?;
    let result = {
        let (session, doc, caps) = store.session_mut();
        scriptorium_tools::apply(session, doc, r, append, caps)
    };
    match result {
        Ok(applied) => {
            let text = match &applied.file_text {
                Some(text) => Some(text.clone()),
                None if attached_here => Some(store.session().reviews[r].text().to_string()),
                None => None,
            };
            if let Some(text) = text {
                store.write(r, text).map_err(|e| e.to_string())?;
            }
            Ok(applied)
        }
        Err(e) => {
            if attached_here {
                let text = store.session().reviews[r].text().to_string();
                store.write(r, text).map_err(|e| e.to_string())?;
            }
            Err(e.to_string())
        }
    }
}

/// The review `target` names, and whether it was attached by this call.
fn resolve(store: &mut dyn ReviewStore, file: &mut ThreadFile, target: &Target) -> Result<(usize, bool), ReviewError> {
    match target {
        Target::Own => own_review(store, file),
        Target::Review { index, id } => {
            let session = store.session();
            let r = match session.reviews.get(*index) {
                Some(r) if r.id == *id => *index,
                _ => session.reviews.iter().position(|r| r.id == *id).ok_or_else(|| {
                    ReviewError::Failed(format!("the review with id {id} isn't attached any more: call the tool again"))
                })?,
            };
            Ok((recheck(store, r)?, false))
        }
        Target::File(path) => file_review(store, path),
    }
}

/// The thread's own review: attached, re-attached, or created.
fn own_review(store: &mut dyn ReviewStore, file: &mut ThreadFile) -> Result<(usize, bool), ReviewError> {
    if let Some(r) = thread_review(store, file) {
        let r = recheck(store, r)?;
        record(store, file, r);
        return Ok((r, false));
    }
    if let Some(stored) = file.review_path.clone() {
        let at = resolve_path(store.dir(), &stored);
        if store.disk_text(&at).is_some() {
            let r = store.attach(&at)?;
            record(store, file, r);
            return Ok((r, true));
        }
    }
    let text = new_review_text(store.script_name(), &file.id);
    let mut taken = store.file_names()?;
    for _ in 0..NAME_TRIES {
        let name = naming::review_file_name(&file.title, &taken);
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
                if !taken.iter().any(|t| t.to_lowercase() == name.to_lowercase()) {
                    taken.push(name);
                }
            }
            Err(e) => return Err(e),
        }
    }
    Err(ReviewError::Failed("couldn't find a free file name for this conversation's review".into()))
}

/// A review file named by path (MCP's `review_file`, if a thread's tools have
/// it): relative to the script's folder, or absolute.
fn file_review(store: &mut dyn ReviewStore, path: &str) -> Result<(usize, bool), ReviewError> {
    let at = canonical(&resolve_path(store.dir(), path));
    let stored = stored_path(store.dir(), &at);
    if let Some(r) = scriptorium_tools::find_review(store.session(), &stored) {
        return Ok((recheck(store, r)?, false));
    }
    if store.disk_text(&at).is_some() {
        return Ok((store.attach(&at)?, true));
    }
    let id = scriptorium_fsio::opened::new_review_id(path, &scriptorium_fsio::opened::review_salt(&at));
    let text = insert_id(&format!("{FORMAT_HEADER}\nScript: {}\n\n", store.script_name()), &id);
    Ok((store.create(&at, &text)?, true))
}

/// `path` canonical, or its folder canonical when the file doesn't exist yet.
fn canonical(path: &Path) -> PathBuf {
    if let Ok(p) = path.canonicalize() {
        return p;
    }
    match (path.parent().and_then(|d| d.canonicalize().ok()), path.file_name()) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => path.to_path_buf(),
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
}

/// Suggestions a call added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Added {
    /// The review's id (the thread's own: `thread-<id>`).
    pub review_id: String,
    /// Per proposal, its number as `list_suggestions` shows it, or `None`
    /// if it wasn't added ([`Applied::numbers`]).
    pub numbers: Vec<Option<usize>>,
}

/// Runs one tool call for thread `file`: opens the script (for writing when
/// the tool may add suggestions), runs the tool with the thread's review as
/// the default, and carries out what it asks with [`apply_append`]. Failures
/// are error results for the model, never a stop. The whole-script check is
/// the caller's, before this ([`whole_script_gate`]).
pub fn run_call(script: &mut dyn Script, file: &mut ThreadFile, call: &ToolCall) -> CallDone {
    let done = |content: String, is_error: bool, added: Option<Added>| CallDone {
        result: ToolResult { id: call.id.clone(), content, is_error },
        added,
    };
    let writes = scriptorium_tools::may_append(&call.name);
    let mut store = match script.open(writes) {
        Ok(s) => s,
        Err(e) => return done(e.to_string(), true, None),
    };
    let outcome = {
        let ctx = Ctx {
            doc: store.doc(),
            session: store.session(),
            script_name: store.script_name(),
            is_caps: store.is_caps(),
            default_review: thread_review(&*store, file),
        };
        scriptorium_tools::call(&ctx, &call.name, &call.input)
    };
    match outcome {
        Err(e) => done(e.to_string(), true, None),
        Ok(Outcome::Text(text)) => done(text, false, None),
        Ok(Outcome::Append(_)) if !writes => done(format!("{} can't change anything", call.name), true, None),
        Ok(Outcome::Append(append)) => match apply_append(&mut *store, file, append) {
            Ok(applied) => {
                let added = applied.numbers.iter().any(Option::is_some).then(|| Added {
                    review_id: file.review_id.clone().unwrap_or_default(),
                    numbers: applied.numbers.clone(),
                });
                done(applied.message, false, added)
            }
            Err(message) => done(message, true, None),
        },
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
    /// Ask the writer ([`whole_script_question`]), store the answer in
    /// [`ThreadFile::whole_script`], then run or refuse.
    Ask {
        tokens: usize,
        cost: Option<f64>,
    },
}

/// Whether tool call `name` may run now as far as the whole-script rule
/// goes: only `read_script` is checked. `allow`: the writer allowed it for
/// this run (`--allow-whole-script`), which overrides a stored "Not now".
/// Otherwise the thread's stored answer decides; with none, a script above
/// [`WHOLE_SCRIPT_TOKENS`] or [`WHOLE_SCRIPT_DOLLARS`] (input at `price`)
/// asks.
pub fn whole_script_gate(
    script: &mut dyn Script,
    file: &ThreadFile,
    name: &str,
    allow: bool,
    price: Option<&Price>,
) -> Result<Gate, ReviewError> {
    if name != "read_script" || allow || file.whole_script == Some(true) {
        return Ok(Gate::Run);
    }
    if file.whole_script == Some(false) {
        return Ok(Gate::Refuse);
    }
    let store = script.open(false)?;
    let (_, tokens) = scriptorium_tools::script_size(store.doc(), store.is_caps());
    Ok(gate_for(tokens, price))
}

/// [`Gate::Run`] or [`Gate::Ask`] for a script of `tokens` tokens.
pub fn gate_for(tokens: usize, price: Option<&Price>) -> Gate {
    let cost = price.map(|p| tokens as f64 * p.rates(tokens as u64).0 / 1_000_000.0);
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
