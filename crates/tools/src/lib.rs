//! The tools an agent uses on one script: reading it, listing reviews and
//! suggestions, and proposing changes. One catalogue and one implementation,
//! shared by the built-in agent and the MCP server.
//!
//! Pure (AGENTS.md rule 1): [`call`] reads a [`Ctx`] (the live document and the
//! session) and returns a value. Tools that change something don't do it
//! here: they return an [`Outcome::Append`] or [`Outcome::NewVersion`] the
//! host carries out. For suggestions the
//! host picks the review the [`Target`] names (creating and attaching one if
//! needed), calls [`apply`], saves [`Applied::file_text`] (if any) and gives the model
//! [`Applied::message`] as the tool result.
//!
//! Every read uses `ctx.doc`, the document as it is now (with decisions and
//! edits), never the file as it was opened.

pub mod agent;
mod args;
mod block;
mod catalogue;
pub mod context;
pub mod guidance;
pub mod prompt;
mod read;
mod reviews;
mod rows;
mod suggest;

use std::fmt;

use scriptorium_core::{Document, ElementKind};
use scriptorium_review::{Entry, Session};
use serde_json::Value;

pub use agent::{AGENT_ONLY, annotations, mcp_tool, read_only};
pub use block::{MAX_LINES, Spot, block_entry};
pub use catalogue::{
    HostField, KINDS, MAX_BATCH, MAX_OPTIONS, MAX_PAGE, Offer, PAGE, ToolSpec, add_before, add_host_fields, catalogue,
    check_args, offered,
};
pub use read::{MIN_READ_BUDGET, NOT_EDITABLE, READ_BUDGET, script_size};
pub use reviews::{SuggestionRow, find_review, item_numbered, item_numbers, suggestion_rows};
pub use rows::{LinksCtx, MAX_ROWS_PAGE, ParaView, ROWS_BYTES, ROWS_PAGE, RowView, SuggestionView};
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
    /// The pair of versions the script is in (`list_versions`, `read_rows`):
    /// `None` when no links file beside it names it; `Some(Err(reason))`
    /// when one does but can't be used (it doesn't parse, the other file is
    /// missing...). Every host builds it from the pair it opened the script
    /// with (`LinksCtx::from` a `scriptorium_versions::PairView`), and only
    /// for the calls that read it ([`uses_links`]); `None` for the others.
    pub links: Option<Result<LinksCtx<'a>, String>>,
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
    /// `new_version`: the host makes the version beside the script, or finds
    /// the one made before (`scriptorium_fsio::versions::version_new`, under
    /// the script's session lock), and tells the model the version's
    /// absolute path ([`new_version_message`]).
    NewVersion(NewVersion),
}

/// A version to make of the script the tool was called on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewVersion {
    /// The version's language code (`es`).
    pub language: String,
    /// The script's own language code (`en`).
    pub original_language: String,
}

/// The first line of [`new_version_message`] for a version made before
/// (found again, with the messages of any file re-found): no counts, which
/// the links file's stamps can't give as they are now; `read_rows` can.
pub fn already_made(version_file: &str, links_file: &str, refound: &[String]) -> String {
    let mut out = format!("{version_file} was already made, with {links_file}.");
    for m in refound {
        out.push(' ');
        out.push_str(m);
    }
    out.push_str(" read_rows with needs on it lists the rows left to translate.");
    out
}

/// The tool result for a version made or found: the host's one-line
/// summary, then the version's absolute path `version`. `switch`: the host
/// takes a `script` argument (MCP, the CLI `tool`), so the model goes on
/// with that path; otherwise (the built-in agent, bound to its script) the
/// writer opens the version to translate it there.
pub fn new_version_message(summary: &str, version: &str, switch: bool) -> String {
    let next = if switch {
        "From now on, pass this path as `script`: rows are read and suggestions made on the version, never on \
         the original."
            .to_string()
    } else {
        let name = version.rsplit(['/', '\\']).next().unwrap_or(version);
        format!(
            "This conversation stays on the original, which takes no suggestions for the version: the writer opens \
             {name} in Scriptorium and asks for the translation there, in a new conversation."
        )
    };
    format!("{summary}\nThe version: {version}\n{next}")
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
    /// A review file by path (MCP's and the CLI `tool`'s `review_file`;
    /// the built-in agent isn't offered it, see [`check_args`]), as the host
    /// gave it in the call. The host resolves it with [`find_review`], opening, creating
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

/// Whether tool `name` can return [`Outcome::Append`] (the suggesting
/// tools): a host may create the review they write to.
pub fn may_append(name: &str) -> bool {
    matches!(name, "suggest_change" | "suggest_changes")
}

/// Whether tool `name` may change files: the suggesting tools
/// ([`may_append`]), `new_version` ([`Outcome::NewVersion`]) and the
/// agent-only tools ([`AGENT_ONLY`]: attach, detach, decide, edit, undo,
/// redo, write the script). Hosts take the script's session lock only for
/// these; every other tool only reads ([`read_only`]).
pub fn writes(name: &str) -> bool {
    may_append(name) || name == "new_version" || AGENT_ONLY.contains(&name)
}

/// Whether tool `name` reads [`Ctx::links`]: hosts lay the pair's rows out
/// only for these: `list_versions`, `read_rows`, `new_version`, and the
/// suggesting tools (a suggestion on a version goes on one of its rows).
///
/// For the suggesting tools, a host passes a links file that can't be used
/// (`Some(Err)`) only when the script may be the version
/// (`PairState::maybe_made` in fsio): suggesting on it is then refused,
/// since its suggestions are placed by rows. On a script that can't be the
/// version, it passes `None`.
pub fn uses_links(name: &str) -> bool {
    matches!(name, "list_versions" | "read_rows" | "new_version") || may_append(name)
}

/// Run tool `name` with `args` (a JSON object).
pub fn call(ctx: &Ctx, name: &str, args: &Value) -> Result<Outcome, ToolError> {
    let a = args::Args::new(args)?;
    match name {
        "script_info" => Ok(Outcome::Text(read::script_info(ctx))),
        "list_paragraphs" => read::list_paragraphs(ctx, &a).map(Outcome::Text),
        "read_scene" => read::read_scene(ctx, &a).map(Outcome::Text),
        "read_script" => read::read_script(ctx, &a).map(Outcome::Text),
        "list_reviews" => Ok(Outcome::Text(reviews::list_reviews(ctx, a.bool("summary")?))),
        "list_suggestions" => reviews::list_suggestions(ctx, &a).map(Outcome::Text),
        "list_versions" => rows::list_versions(ctx).map(Outcome::Text),
        "read_rows" => rows::read_rows(ctx, &a).map(Outcome::Text),
        "suggest_change" => suggest::suggest_change(ctx, &a),
        "suggest_changes" => suggest::suggest_changes(ctx, &a),
        "new_version" => rows::new_version(ctx, &a),
        "guide" => guidance::guide(args).map(Outcome::Text),
        // On the files there is no writer to place; the app answers it
        // itself from its open view (`context::text`).
        context::NAME => Ok(Outcome::Text(context::ON_FILES.to_string())),
        _ => Err(ToolError::Unknown(name.to_string())),
    }
}
