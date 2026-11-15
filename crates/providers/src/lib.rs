//! AI provider accounts for the built-in agent.
//!
//! The writer's accounts ([`Account`], stored together as an
//! [`AccountsFile`]) say which provider to talk to, with which model and
//! settings. [`resolve()`] turns an account (plus the thread's model, if it
//! has one) into everything a request needs, and [`build_request`] builds
//! the HTTP request for a thread with [`scriptorium_agent::protocol`].
//! [`presets`] are the providers offered by name. [`models`] holds what we know about models: what they accept
//! ([`Capabilities`]) and what they cost ([`Price`]); [`cost`] prices a
//! thread for the token/cost display.
//!
//! v0.4.0 supports two kinds of endpoint: Anthropic, and OpenAI-compatible
//! (OpenAI, OpenRouter, and any other server, local ones included, by its
//! base URL).
//!
//! The modules above, [`cassette`] and [`failure`] are the crate's pure
//! core (AGENTS.md rule 1): values in, values out. API keys arrive as
//! arguments. Reading them from the keyring or the environment, the config
//! file on disk, and HTTP belong to [`shell`] (feature `shell`, on by
//! default), which carries requests through a [`shell::Transport`]: live
//! ([`shell::Http`]) or from a cassette ([`shell::Replay`]). Both turn
//! failures into errors through the one mapping, [`Failure::to_error`].

pub mod account;
pub mod cassette;
pub mod cost;
pub mod failure;
pub mod models;
pub mod presets;
pub mod resolve;
#[cfg(feature = "shell")]
pub mod shell;

pub use account::{Account, AccountsFile, Provider};
pub use cost::{PriceBook, ThreadCost, reply_cost, thread_cost};
pub use failure::Failure;
pub use models::{Capabilities, ChatTools, Price, PriceTier, Retired, capabilities};
pub use presets::{PRESETS, Preset};
pub use resolve::{DEFAULT_MAX_TOKENS, Resolved, build_request, models_request, resolve};

use scriptorium_agent::AgentError;

#[derive(Debug, Clone, PartialEq)]
pub enum ProvidersError {
    /// The accounts file can't be read, or says something inconsistent.
    Config(String),
    /// No account has this id.
    UnknownAccount(String),
    /// Neither the thread nor the account names a model.
    NoModel { account: String },
    /// The account's provider needs an API key and none was given.
    MissingKey { account: String },
    /// From the agent crate (e.g. a thread that can't be sent yet).
    Agent(AgentError),
}

impl std::fmt::Display for ProvidersError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProvidersError::Config(m) => write!(f, "accounts file: {m}"),
            ProvidersError::UnknownAccount(id) => write!(f, "there is no account \"{id}\""),
            ProvidersError::NoModel { account } => write!(f, "choose a model for the account \"{account}\""),
            ProvidersError::MissingKey { account } => write!(f, "the account \"{account}\" has no API key"),
            ProvidersError::Agent(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ProvidersError {}

impl From<AgentError> for ProvidersError {
    fn from(e: AgentError) -> Self {
        ProvidersError::Agent(e)
    }
}
