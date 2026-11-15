//! The built-in agent, minus the I/O.
//!
//! A [`Thread`] is one conversation. To take a turn, build an HTTP request for
//! the account's wire [`Protocol`] ([`protocol::build_request`]), send it (the
//! caller does that), feed the streamed reply line by line to a
//! [`protocol::ReplyParser`], and hand the finished [`Turn`] to
//! [`Thread::push_turn`]. If the model asked for tools, the caller runs them
//! and adds the results with [`Thread::push_tool_results`], then takes another
//! turn. [`turn::Run`] drives that loop; [`turn::cancel`] answers calls left
//! waiting (e.g. by a crash).
//!
//! The thread is stored in a neutral form, so a conversation can continue on
//! another provider; what one provider needs replayed exactly (Anthropic's
//! thinking blocks, Gemini's thought signatures) is kept as-is and sent back
//! only to the endpoint and model that produced it. [`file`](mod@file) stores a thread
//! with what must stay fixed for it (the system prompt).
//!
//! Pure: values in, values out. No network, files, clocks or threads.

pub mod file;
pub mod presets;
pub mod protocol;
mod sse;
mod thread;
pub mod turn;

pub use protocol::{Call, Effort, Protocol, ReplyParser, Request, Thinking, ToolSpec};
pub use thread::{
    Block, Event, Message, Origin, Pushed, Role, STOPPED, StopReason, Thread, ToolCall, ToolResult, Turn, Usage,
};

/// What kind of failure a provider error is, for choosing what to tell the
/// writer and whether to offer a retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The key was refused or lacks permission.
    Auth,
    /// Rate limited, or out of credit.
    RateLimit,
    /// The provider is too busy right now.
    Overloaded,
    /// The connection failed or was cut off. The shell builds these for
    /// network failures ([`AgentError::offline`]).
    Offline,
    /// The request was refused as invalid (bad model name, too long…).
    BadRequest,
    Other,
}

/// Something went wrong talking to a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    /// The provider reported an error, or the connection failed.
    Api {
        /// The HTTP status, when the error came as one (not mid-stream).
        status: Option<u16>,
        kind: ErrorKind,
        /// Whether sending the same request again later may work.
        retryable: bool,
        /// For the writer.
        message: String,
        /// How long the provider asked to wait before retrying, in seconds
        /// (its `retry-after` header), when it said.
        retry_after: Option<u64>,
    },
    /// The reply didn't have the expected shape.
    Parse(String),
    /// The thread can't be sent as it is to the request's endpoint and model
    /// ([`Thread::check_sendable`], which [`protocol::build_request`] and
    /// [`turn::Run`] run first).
    NotReady(String),
    /// Tool results that don't answer the waiting calls exactly.
    Mismatch(String),
    /// A stored thread file can't be read.
    File(String),
}

impl AgentError {
    /// A network failure (no connection, timeout, closed early).
    pub fn offline(message: impl Into<String>) -> Self {
        AgentError::Api {
            status: None,
            kind: ErrorKind::Offline,
            retryable: true,
            message: message.into(),
            retry_after: None,
        }
    }

    pub fn kind(&self) -> Option<ErrorKind> {
        match self {
            AgentError::Api { kind, .. } => Some(*kind),
            _ => None,
        }
    }
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::Api { message, .. } => write!(f, "{message}"),
            AgentError::Parse(m) => write!(f, "unexpected reply: {m}"),
            AgentError::NotReady(m) => write!(f, "can't send yet: {m}"),
            AgentError::Mismatch(m) => write!(f, "tool results refused: {m}"),
            AgentError::File(m) => write!(f, "can't read the thread file: {m}"),
        }
    }
}

impl std::error::Error for AgentError {}
