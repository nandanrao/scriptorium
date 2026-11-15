//! The tools an agent uses on one script: reading it, listing reviews and
//! suggestions, and proposing changes. One catalogue and one implementation,
//! shared by the built-in agent and the MCP server.
//!
//! Pure (AGENTS.md rule 1): [`call`] reads a [`Ctx`] (the live document and the
//! session) and returns a value. Tools that change something don't do it
//! here: they return an [`Outcome::Append`] the host carries out. For suggestions the
//! host picks the review the [`Target`] names (creating and attaching one if
//! needed), calls [`apply`], saves [`Applied::file_text`] (if any) and gives the model
//! [`Applied::message`] as the tool result.
//!
//! Every read uses `ctx.doc`, the document as it is now (with decisions and
//! edits), never the file as it was opened.

mod args;
mod catalogue;
pub mod prompt;
mod read;
mod reviews;
mod suggest;

use std::fmt;

use scriptorium_core::{Document, ElementKind};
use scriptorium_review::{Entry, Session};
use serde_json::Value;

pub use catalogue::{
    HostField, MAX_BATCH, MAX_OPTIONS, MAX_PAGE, PAGE, ToolSpec, add_before, add_host_fields, catalogue,
};
pub use read::{NOT_EDITABLE, script_size};
pub use reviews::{find_review, item_numbered, item_numbers};
pub use suggest::apply;

/// What a tool call can see.
pub struct Ctx<'a> {
    /// The script as it is now.
    pub doc: &'a Document,
    pub session: &'a Session,
    /// The script's file name, e.g. `film.fdx`.
    pub script_name: &'a str,
    /// Element kinds the script shows in capitals (letter case changes there are invisible).
    pub is_caps: &'a dyn Fn(&ElementKind) -> bool,
    /// The review used when a call names none. The host decides: the agent
    /// passes its thread's review (`None` until it exists), MCP the only
    /// attached review.
    pub default_review: Option<usize>,
}

/// A tool's result.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The final text for the model (read tools, and batches where nothing
    /// passed validation).
    Text(String),
    /// Suggestions for the host to add with [`apply`]; the tool result is then
    /// [`Applied::message`].
    Append(Append),
}

/// Suggestions that passed validation, to add to one review with [`apply`],
/// in one write.
#[derive(Debug, Clone, PartialEq)]
pub struct Append {
    pub target: Target,
    pub proposals: Vec<Proposal>,
    /// Batch items refused by validation, reported back in the result.
    pub failed: Vec<Failed>,
    /// From `suggest_changes`: the result is the batch JSON, not a sentence.
    pub batch: bool,
}

/// One validated suggestion. `entry` has `para` and `before` (the line's live
/// text, trimmed) set.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    /// Position in the request (0 for `suggest_change`).
    pub index: usize,
    pub entry: Entry,
    /// The line was found by MCP's `before` approximately, not exactly: the
    /// result then quotes the line it went on (`entry.before`).
    pub fuzzy: bool,
}

/// Which review a suggestion goes into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The host's own review, which doesn't exist yet (`Ctx::default_review`
    /// was `None`): the agent host creates `<thread title>.review.txt` with
    /// `Id: thread-<id>` and attaches it.
    Own,
    /// An attached review: its index into `Session::reviews` when the call
    /// was made, and its [`Review::id`](scriptorium_review::Review::id).
    /// The index can go stale (a review detached meanwhile), so [`apply`]
    /// refuses a review whose id differs.
    /// This relies on hosts pinning an `Id:` in every review they attach
    /// ([`Review::pin_id`](scriptorium_review::Review::pin_id), per the host
    /// contract): two reviews with the same id can't be told apart.
    Review { index: usize, id: String },
    /// A review file by path (MCP's `review_file`), as the host gave it in
    /// the call. The host resolves it with [`find_review`], opening, creating
    /// and attaching it if it isn't attached. If it was already attached,
    /// duplicates were checked against it.
    File(String),
}

/// A batch item that wasn't added.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Failed {
    pub index: usize,
    /// `None` when the item gave no usable `paragraph_id`.
    pub paragraph_id: Option<u64>,
    pub error: String,
}

/// What [`apply`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// The review file's new contents, for the host to save; `None` when
    /// nothing was written (every proposal failed), so there is nothing to save.
    pub file_text: Option<String>,
    /// Per proposal, its number as `list_suggestions` shows it (1-based), or
    /// `None` if it wasn't added.
    pub numbers: Vec<Option<usize>>,
    /// Per proposal, its stable entry index in the review file (`e<entry>`),
    /// or `None` if it wasn't written.
    pub entries: Vec<Option<usize>>,
    /// The tool result.
    pub message: String,
}

/// Errors are told to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// No tool by that name.
    Unknown(String),
    /// The arguments are missing or malformed.
    Args(String),
    /// Well-formed, but can't be done (no such paragraph, invisible change, ...).
    Invalid(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(name) => write!(f, "unknown tool {name:?}"),
            Self::Args(m) | Self::Invalid(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ToolError {}

/// Run tool `name` with `args` (a JSON object).
pub fn call(ctx: &Ctx, name: &str, args: &Value) -> Result<Outcome, ToolError> {
    let a = args::Args::new(args)?;
    match name {
        "script_info" => Ok(Outcome::Text(read::script_info(ctx))),
        "list_paragraphs" => read::list_paragraphs(ctx, &a).map(Outcome::Text),
        "read_scene" => read::read_scene(ctx, &a).map(Outcome::Text),
        "read_script" => Ok(Outcome::Text(read::read_script(ctx))),
        "list_reviews" => Ok(Outcome::Text(reviews::list_reviews(ctx, a.bool("summary")?))),
        "list_suggestions" => reviews::list_suggestions(ctx, &a).map(Outcome::Text),
        "suggest_change" => suggest::suggest_change(ctx, &a),
        "suggest_changes" => suggest::suggest_changes(ctx, &a),
        _ => Err(ToolError::Unknown(name.to_string())),
    }
}
