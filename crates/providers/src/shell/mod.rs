//! The imperative shell (feature `shell`, on by default): the accounts file
//! on disk ([`config`]), API keys ([`keys`]), and carrying requests
//! ([`Transport`]: [`Http`] live, [`Replay`] from a cassette). Nothing in the
//! pure core calls in here.

pub mod config;
mod http;
pub mod keys;
mod replay;
pub mod time;

use std::path::Path;

use scriptorium_agent::{AgentError, Request};

pub use http::Http;
pub use keys::{KeySource, KeyStatus, Keys};
pub use replay::Replay;
pub use time::TimeScale;

/// Whether to keep reading a streamed reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// Stop reading and drop the connection (the writer pressed Stop).
    Stop,
}

/// Carries a request to a provider.
pub trait Transport: Send + Sync {
    /// Send `req` (a POST of its JSON body; a GET when the body is `null`,
    /// as for [`models_request`](crate::models_request)), calling `line` for
    /// each line of a successful reply's body, without its line ending,
    /// until it returns [`Flow::Stop`] or the body ends. Feed the lines to a
    /// [`ReplyParser`](scriptorium_agent::ReplyParser).
    ///
    /// A failure before or during the body is an [`AgentError`] built by
    /// [`Failure::to_error`](crate::Failure::to_error); lines already passed
    /// to `line` stay passed (so the caller can keep them with
    /// `ReplyParser::abort`).
    fn send(&self, req: &Request, line: &mut dyn FnMut(&str) -> Flow) -> Result<(), AgentError>;
}

/// The environment variable naming a cassette to replay instead of
/// talking to providers.
pub const REPLAY_ENV: &str = "SCRIPTORIUM_REPLAY";

/// The transport the CLI and the app use: a [`Replay`] of the cassette
/// named by `SCRIPTORIUM_REPLAY` when it's set (with `SCRIPTORIUM_TEST_TIME_SCALE`
/// applied to its delays), else [`Http`], recording to `record` when given.
pub fn transport_from_env(record: Option<&Path>) -> Result<Box<dyn Transport>, AgentError> {
    match std::env::var_os(REPLAY_ENV).filter(|v| !v.is_empty()) {
        Some(path) => Ok(Box::new(Replay::open(Path::new(&path), TimeScale::from_env())?)),
        None => {
            let http = Http::new();
            Ok(Box::new(match record {
                Some(path) => http.record_to(path)?,
                None => http,
            }))
        }
    }
}
