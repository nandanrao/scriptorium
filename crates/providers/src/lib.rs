//! AI provider accounts for the built-in agent.
//!
//! The writer's accounts ([`Account`], stored together as an
//! [`AccountsFile`]) say which provider to talk to, with which model and
//! settings. [`resolve()`] turns an account (plus the thread's model, if it
//! has one) into everything a request needs, and [`build_request`] builds
//! the HTTP request for a thread with [`scriptorium_agent::protocol`].
//! [`models`] holds what we know about models: what they accept
//! ([`Capabilities`]) and what they cost ([`Price`]); [`cost`] prices a
//! thread for the token/cost display.
//!
//! v0.4.0 supports two kinds of endpoint: Anthropic, and OpenAI-compatible
//! (OpenAI, OpenRouter, and any other server, local ones included, by its
//! base URL).
//!
//! This is the crate's pure core (AGENTS.md rule 1): values in, values out.
//! API keys arrive as arguments; reading them from the keyring or the
//! environment, the config file on disk, and HTTP belong to the shell.

pub mod account;
pub mod cost;
pub mod models;
pub mod resolve;

pub use account::{Account, AccountsFile, Provider};
pub use cost::{PriceBook, ThreadCost, reply_cost, thread_cost};
pub use models::{Capabilities, Price, PriceTier, capabilities};
pub use resolve::{Resolved, build_request, models_request, resolve};

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
