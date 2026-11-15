//! What can go wrong carrying a request, and the one mapping from that to an
//! [`AgentError`].
//!
//! Every transport (the live [`Http`](crate::shell::Http) one and the
//! [`Replay`](crate::shell::Replay) one alike) describes a failure as a
//! [`Failure`] and turns it into an error with [`Failure::to_error`], so a
//! replayed 529 is exactly the error a live 529 would be, and the writer sees
//! the same row for both.

use scriptorium_agent::protocol;
use scriptorium_agent::{AgentError, ErrorKind};

/// A failed request, as the transport saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The provider answered with an error status. `retry_after` is the raw
    /// `retry-after` header, `body` the response body (the provider's error
    /// JSON, usually).
    Status { status: u16, retry_after: Option<String>, body: String },
    /// No connection: refused, DNS, TLS, timeout before any reply.
    Offline(String),
    /// The connection broke after the reply had started.
    CutOff(String),
    /// A line of the streamed body isn't text (not UTF-8).
    BadLine(String),
    /// A file the transport needs (a replay cassette) can't be read.
    File(String),
    /// A replayed exchange doesn't fit the request, or the cassette ran out.
    /// A test setup error: never retried.
    Replay(String),
}

impl Failure {
    /// The error the agent loop and the writer see.
    ///
    /// - a status: the provider's error type, else the status (401/403 Auth,
    ///   429 RateLimit, 503/529 Overloaded, other 4xx BadRequest, 5xx Other,
    ///   retryable), with `retry-after` read as seconds;
    /// - no connection or cut off: Offline, retryable;
    /// - a line that isn't text: Parse;
    /// - an unreadable cassette: File;
    /// - a replay mismatch: Other, not retryable, naming the exchange.
    pub fn to_error(&self) -> AgentError {
        match self {
            Failure::Status { status, retry_after, body } => protocol::api_error(*status, body, retry_after.as_deref()),
            Failure::Offline(m) => AgentError::offline(m.clone()),
            Failure::CutOff(m) => AgentError::offline(format!("the reply was cut off: {m}")),
            Failure::BadLine(m) => AgentError::Parse(m.clone()),
            Failure::File(m) => AgentError::File(m.clone()),
            Failure::Replay(m) => AgentError::Api {
                status: None,
                kind: ErrorKind::Other,
                retryable: false,
                message: format!("replay: {m}"),
                retry_after: None,
            },
        }
    }
}

impl From<Failure> for AgentError {
    fn from(f: Failure) -> Self {
        f.to_error()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(status: u16, retry_after: Option<&str>, body: &str) -> AgentError {
        Failure::Status { status, retry_after: retry_after.map(String::from), body: body.into() }.to_error()
    }

    fn parts(e: &AgentError) -> (Option<u16>, ErrorKind, bool, Option<u64>) {
        match e {
            AgentError::Api { status, kind, retryable, retry_after, .. } => (*status, *kind, *retryable, *retry_after),
            other => panic!("not an API error: {other:?}"),
        }
    }

    #[test]
    fn statuses() {
        assert_eq!(parts(&status(401, None, "")), (Some(401), ErrorKind::Auth, false, None));
        assert_eq!(parts(&status(403, None, "")), (Some(403), ErrorKind::Auth, false, None));
        assert_eq!(parts(&status(429, Some("7"), "")), (Some(429), ErrorKind::RateLimit, true, Some(7)));
        assert_eq!(parts(&status(529, Some("1"), "")), (Some(529), ErrorKind::Overloaded, true, Some(1)));
        assert_eq!(parts(&status(503, None, "")), (Some(503), ErrorKind::Overloaded, true, None));
        assert_eq!(parts(&status(500, None, "")), (Some(500), ErrorKind::Other, true, None));
        assert_eq!(parts(&status(400, None, "")), (Some(400), ErrorKind::BadRequest, false, None));
        assert_eq!(parts(&status(404, None, "")), (Some(404), ErrorKind::BadRequest, false, None));
    }

    #[test]
    fn provider_error_bodies() {
        let overloaded = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        let e = status(529, None, overloaded);
        assert_eq!(parts(&e).1, ErrorKind::Overloaded);
        assert!(e.to_string().contains("Overloaded"), "{e}");
        let quota = r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota"}}"#;
        assert_eq!(parts(&status(429, None, quota)), (Some(429), ErrorKind::RateLimit, false, None));
        let model = r#"{"error":{"type":"not_found_error","message":"model: claude-nope"}}"#;
        let e = status(404, None, model);
        assert_eq!(parts(&e).1, ErrorKind::BadRequest);
        assert!(e.to_string().contains("claude-nope"));
        // A body that isn't JSON still gives the status's kind.
        assert_eq!(parts(&status(502, None, "<html>Bad gateway</html>")).1, ErrorKind::Other);
    }

    #[test]
    fn transport_failures() {
        let e = Failure::Offline("connection refused".into()).to_error();
        assert_eq!(parts(&e), (None, ErrorKind::Offline, true, None));
        let e = Failure::CutOff("reset".into()).to_error();
        assert_eq!(parts(&e), (None, ErrorKind::Offline, true, None));
        assert!(e.to_string().contains("cut off"));
        assert!(matches!(Failure::BadLine("x".into()).to_error(), AgentError::Parse(_)));
        assert!(matches!(Failure::File("x".into()).to_error(), AgentError::File(_)));
        let e = Failure::Replay("exchange 3: wrong model".into()).to_error();
        assert_eq!(parts(&e), (None, ErrorKind::Other, false, None));
        assert!(e.to_string().contains("exchange 3"));
    }
}
