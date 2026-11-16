//! The imperative shell (feature `shell`, on by default): the accounts file
//! on disk ([`config`]), API keys ([`keys`]), and carrying requests
//! ([`Transport`]: [`Http`] live, [`Replay`] from a cassette;
//! [`receive`] streams one reply through a parser). Nothing in the pure core
//! calls in here.

mod cancel;
pub mod config;
mod http;
pub mod keys;
mod receive;
mod replay;
pub mod time;

use std::path::Path;

use scriptorium_agent::{AgentError, Request};

pub use cancel::CancelToken;
pub use http::Http;
pub use keys::{KEYRING_ENV, KeyError, KeySource, KeyStatus, Keys, keyring_off};
pub use receive::{Received, receive};
pub use replay::Replay;
pub use time::TimeScale;

/// Whether to keep reading a streamed reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// Stop reading and close the connection: the reply is complete (its
    /// final event arrived; some proxies hold the body open after it), or
    /// it can't be read.
    Stop,
}

/// How a [`Transport::send`] that didn't fail ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The body ended, or `line` returned [`Flow::Stop`].
    Finished,
    /// The [`CancelToken`] was cancelled.
    Cancelled,
}

/// Carries a request to a provider. Blocking: call it on a worker thread.
pub trait Transport: Send + Sync {
    /// Send `req` (a POST of its JSON body; a GET when the body is `null`,
    /// as for [`models_request`](crate::models_request)), calling `line` for
    /// each line of a successful reply's body, without its line ending,
    /// until it returns [`Flow::Stop`], the body ends, or `cancel` is
    /// cancelled. Most callers want [`receive`], which does the parsing.
    ///
    /// A failure before or during the body is an [`AgentError`] built by
    /// [`Failure::to_error`](crate::Failure::to_error); lines already passed
    /// to `line` stay passed. Once `cancel` is cancelled the result is
    /// `Ok(Ended::Cancelled)` whatever the connection did.
    ///
    /// Cancelling shuts the connection down, so `send` returns promptly, but
    /// not synchronously: a caller that shows a Stop (the panel's driver)
    /// marks the turn stopped at once and ignores any `line` calls or result
    /// that arrive after it.
    fn send(
        &self,
        req: &Request,
        cancel: &CancelToken,
        line: &mut dyn FnMut(&str) -> Flow,
    ) -> Result<Ended, AgentError>;

    /// Called once when the caller is done with the transport. A
    /// [`Replay`] reports exchanges left unused (so a test whose program sent
    /// fewer requests than the cassette expected fails); others do nothing.
    fn finish(&self) -> Result<(), AgentError> {
        Ok(())
    }
}

/// The environment variable naming a cassette to replay instead of
/// talking to providers.
pub const REPLAY_ENV: &str = "SCRIPTORIUM_REPLAY";

/// The transport the CLI and the app use: a [`Replay`] of the cassette
/// named by `SCRIPTORIUM_REPLAY` when it's set (with `SCRIPTORIUM_TEST_TIME_SCALE`
/// applied to its delays), else [`Http`], recording to `record` when given
/// (see [`Http::record_to`] for what a recording holds). Asking to record
/// while replaying is an error.
pub fn transport_from_env(record: Option<&Path>) -> Result<Box<dyn Transport>, AgentError> {
    match (std::env::var_os(REPLAY_ENV).filter(|v| !v.is_empty()), record) {
        (Some(_), Some(_)) => {
            Err(crate::Failure::Replay(format!("can't record while replaying ({REPLAY_ENV} is set)")).to_error())
        }
        (Some(path), None) => Ok(Box::new(Replay::open(Path::new(&path), TimeScale::from_env())?)),
        (None, Some(path)) => Ok(Box::new(Http::new().record_to(path)?)),
        (None, None) => Ok(Box::new(Http::new())),
    }
}
