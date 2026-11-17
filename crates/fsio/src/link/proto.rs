//! The link's line protocol and socket path rules, with no I/O: one JSON
//! object per line (see `docs/plans/agent-connect.md`, "The internal
//! protocol").
//!
//! ```text
//! → {"hello": {"proto": 1, "version": "0.6.0", "exe": "…/scriptorium"}}
//! ← {"hello": {"proto": 1, "version": "0.6.0", "script": "…/PURPLE LIST.fdx"}}
//! → {"id": 7, "call": {"name": "suggest_changes", "arguments": {…}}}
//! ← {"id": 7, "taken": true, "script": "…/PURPLE LIST.fdx"}
//! ← {"id": 7, "done": {"text": "…", "is_error": false}}
//! ← {"id": 7, "route": "files"}        (instead of taken)
//! ```

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The protocol's version. Both sides must send the same one (and the same
/// app version, see [`compatible`]).
pub const PROTO: u32 = 1;

/// The longest socket path the platform takes, NUL included: `sun_path` is
/// 104 bytes on macOS and the BSDs, 108 on Linux. The rule uses the smaller,
/// so the app and the connector agree whatever they run on.
pub const SUN_PATH_MAX: usize = 104;

/// The socket's file name, in [`socket_dir`].
pub const SOCKET_NAME: &str = "agent.sock";

/// The lock file beside the socket: whoever holds it owns the socket.
pub const LOCK_NAME: &str = "agent.lock";

/// The first line each side sends. The connector sets `exe` (its own path);
/// the app sets `script` (the script open in it, if any). `script` is
/// informational only (logs, notes): the script may change before any call,
/// and the script a call actually ran on comes back with its `taken`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub proto: u32,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
}

impl Hello {
    /// This build's hello ([`PROTO`], the workspace version), with neither
    /// `exe` nor `script` set.
    pub fn this_build() -> Self {
        Hello { proto: PROTO, version: env!("CARGO_PKG_VERSION").to_string(), exe: None, script: None }
    }

    /// The hello as one line (no newline).
    pub fn encode(&self) -> String {
        encode(&HelloLine { hello: self.clone() })
    }

    /// A hello line.
    pub fn decode(line: &str) -> Result<Self, ProtoError> {
        Ok(decode::<HelloLine>(line)?.hello)
    }
}

#[derive(Serialize, Deserialize)]
struct HelloLine {
    hello: Hello,
}

/// Same protocol and same app version, or no link: a connector from another
/// copy of the app works on the files instead.
pub fn compatible(ours: &Hello, theirs: &Hello) -> bool {
    ours.proto == theirs.proto && ours.version == theirs.version
}

/// What the connector tells the agent once (in its first tool result) when
/// the app that answered is another version.
pub fn mismatch_notice(ours: &Hello, theirs: &Hello) -> String {
    format!(
        "Scriptorium {} is open but this connection is from {} (another copy of the app). \
         Working on the files; the app shows the changes.",
        theirs.version, ours.version
    )
}

/// A tool call, as MCP's `tools/call` names it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Call {
    pub name: String,
    /// The tool's arguments; `{}` when the line leaves them out.
    #[serde(default = "empty_object")]
    pub arguments: Value,
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

/// The connector's request: one call, numbered by the connector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub call: Call,
}

impl Request {
    pub fn encode(&self) -> String {
        encode(self)
    }

    pub fn decode(line: &str) -> Result<Self, ProtoError> {
        decode(line)
    }
}

/// A finished call's result, as MCP returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Done {
    pub text: String,
    pub is_error: bool,
}

/// Where a call should run instead of the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Route {
    /// Not the open script, or nothing open: the connector runs the call on
    /// the files.
    Files,
}

/// One of the app's answers to a request. A call gets either [`Reply::Taken`]
/// then [`Reply::Done`], or [`Reply::Route`] alone. (The client also takes a
/// repeated `taken`, and a `done` with no `taken` before it.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// The app's UI task picked the call up and is about to run it. From now
    /// on the call may have been applied. `script`: the script the app
    /// resolved the call to (canonical path), so a "maybe applied" can say
    /// which one.
    Taken {
        script: Option<String>,
    },
    Done(Done),
    Route(Route),
}

/// The wire shape of a reply: `id` and exactly one of the others.
#[derive(Serialize, Deserialize)]
struct ReplyWire {
    id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    taken: Option<bool>,
    /// Only with `taken`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    script: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    done: Option<Done>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    route: Option<Route>,
}

impl Reply {
    /// The reply to request `id` as one line (no newline).
    pub fn encode(&self, id: u64) -> String {
        let mut wire = ReplyWire { id, taken: None, script: None, done: None, route: None };
        match self {
            Reply::Taken { script } => {
                wire.taken = Some(true);
                wire.script = script.clone();
            }
            Reply::Done(d) => wire.done = Some(d.clone()),
            Reply::Route(r) => wire.route = Some(*r),
        }
        encode(&wire)
    }

    /// A reply line: its request's id and the reply. Exactly one of `taken`
    /// (true), `done` and `route` must be there; `script` only with `taken`.
    pub fn decode(line: &str) -> Result<(u64, Reply), ProtoError> {
        let wire: ReplyWire = decode(line)?;
        let reply = match (wire.taken, wire.done, wire.route) {
            (Some(true), None, None) => Reply::Taken { script: wire.script },
            (None, Some(d), None) if wire.script.is_none() => Reply::Done(d),
            (None, None, Some(r)) if wire.script.is_none() => Reply::Route(r),
            _ => return Err(ProtoError("a reply needs exactly one of taken, done, route".into())),
        };
        Ok((wire.id, reply))
    }
}

/// A line that isn't the message expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtoError(pub String);

impl fmt::Display for ProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bad link message: {}", self.0)
    }
}

impl std::error::Error for ProtoError {}

/// JSON never holds a raw newline (they're escaped inside strings), so this
/// is always one line.
fn encode<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("link messages always serialize")
}

fn decode<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, ProtoError> {
    serde_json::from_str(line.trim_end_matches(['\n', '\r'])).map_err(|e| ProtoError(e.to_string()))
}

/// The socket's path on macOS, from the home folder:
/// `~/Library/Application Support/Scriptorium/agent.sock`, or, when that is
/// too long for a socket address ([`SUN_PATH_MAX`]), `~/.scriptorium/agent.sock`.
/// One rule from `home` alone, so the app and the connector agree with no
/// file to read. Never `$TMPDIR`: an agent may not pass it to the connector.
pub fn socket_path(home: &Path) -> PathBuf {
    let long = home.join("Library/Application Support/Scriptorium").join(SOCKET_NAME);
    if fits(&long) { long } else { home.join(".scriptorium").join(SOCKET_NAME) }
}

/// The socket's path on Linux (development and tests):
/// `$XDG_RUNTIME_DIR/scriptorium/agent.sock`; without a runtime folder, the
/// short macOS one, `~/.scriptorium/agent.sock`.
pub fn linux_socket_path(runtime_dir: Option<&Path>, home: &Path) -> PathBuf {
    match runtime_dir {
        Some(d) if d.is_absolute() => d.join("scriptorium").join(SOCKET_NAME),
        _ => home.join(".scriptorium").join(SOCKET_NAME),
    }
}

/// Whether `path` fits a socket address, NUL included.
pub fn fits(path: &Path) -> bool {
    path.as_os_str().len() < SUN_PATH_MAX
}

/// The folder the socket and [`LOCK_NAME`] live in.
pub fn socket_dir(socket: &Path) -> &Path {
    socket.parent().unwrap_or(Path::new("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hello(version: &str) -> Hello {
        Hello { proto: PROTO, version: version.into(), exe: None, script: None }
    }

    #[test]
    fn hello_lines_match_the_plan() {
        let mut h = hello("0.6.0");
        h.exe = Some("/A/scriptorium".into());
        assert_eq!(h.encode(), r#"{"hello":{"proto":1,"version":"0.6.0","exe":"/A/scriptorium"}}"#);
        assert_eq!(Hello::decode(&h.encode()).unwrap(), h);
        let back = Hello::decode(r#"{"hello":{"proto":1,"version":"0.6.0","script":"/F/P L.fdx","new":1}}"#).unwrap();
        assert_eq!(back.script.as_deref(), Some("/F/P L.fdx"));
        assert!(Hello::decode(r#"{"id":1}"#).is_err());
    }

    #[test]
    fn compatible_needs_same_proto_and_version() {
        assert!(compatible(&hello("0.6.0"), &hello("0.6.0")));
        assert!(!compatible(&hello("0.6.0"), &hello("0.5.1")));
        let mut other = hello("0.6.0");
        other.proto = 2;
        assert!(!compatible(&hello("0.6.0"), &other));
        // exe and script don't matter.
        let mut s = hello("0.6.0");
        s.script = Some("/x.fdx".into());
        assert!(compatible(&hello("0.6.0"), &s));
        assert_eq!(
            mismatch_notice(&hello("0.6.0"), &hello("0.5.1")),
            "Scriptorium 0.5.1 is open but this connection is from 0.6.0 (another copy of the app). \
             Working on the files; the app shows the changes."
        );
    }

    #[test]
    fn requests_round_trip_on_one_line() {
        let r = Request {
            id: 7,
            call: Call { name: "suggest_changes".into(), arguments: json!({"text": "a\nb", "script": "/x.fdx"}) },
        };
        let line = r.encode();
        assert!(!line.contains('\n'));
        assert_eq!(Request::decode(&line).unwrap(), r);
        assert_eq!(Request::decode(&format!("{line}\n")).unwrap(), r);
        let bare = Request::decode(r#"{"id":1,"call":{"name":"list_suggestions"}}"#).unwrap();
        assert_eq!(bare.call.arguments, json!({}));
        assert!(Request::decode(r#"{"id":1}"#).is_err());
        assert!(Request::decode("not json").is_err());
    }

    #[test]
    fn replies_round_trip_and_need_exactly_one_kind() {
        let taken = Reply::Taken { script: None };
        assert_eq!(taken.encode(7), r#"{"id":7,"taken":true}"#);
        let at = Reply::Taken { script: Some("/F/P L.fdx".into()) };
        assert_eq!(at.encode(7), r#"{"id":7,"taken":true,"script":"/F/P L.fdx"}"#);
        assert_eq!(Reply::Route(Route::Files).encode(7), r#"{"id":7,"route":"files"}"#);
        let done = Reply::Done(Done { text: "ok\nmore".into(), is_error: false });
        assert_eq!(done.encode(7), r#"{"id":7,"done":{"text":"ok\nmore","is_error":false}}"#);
        for r in [taken, at, Reply::Route(Route::Files), done] {
            assert_eq!(Reply::decode(&r.encode(3)).unwrap(), (3, r));
        }
        assert!(Reply::decode(r#"{"id":7}"#).is_err());
        assert!(Reply::decode(r#"{"id":7,"taken":false}"#).is_err());
        assert!(Reply::decode(r#"{"id":7,"taken":true,"route":"files"}"#).is_err());
        assert!(Reply::decode(r#"{"id":7,"route":"app"}"#).is_err());
        assert!(Reply::decode(r#"{"id":7,"route":"files","script":"/x.fdx"}"#).is_err());
    }

    #[test]
    fn a_long_home_folder_uses_the_short_path() {
        let home = Path::new("/Users/ann");
        assert_eq!(socket_path(home), Path::new("/Users/ann/Library/Application Support/Scriptorium/agent.sock"));
        // 51 bytes of suffix: a home of 52 bytes makes 103, the most that fits.
        let suffix = "/Library/Application Support/Scriptorium/agent.sock".len();
        let fitting = format!("/{}", "a".repeat(SUN_PATH_MAX - 1 - suffix - 1));
        assert!(socket_path(Path::new(&fitting)).starts_with(format!("{fitting}/Library")));
        let long = format!("{fitting}b");
        assert_eq!(socket_path(Path::new(&long)), Path::new(&long).join(".scriptorium/agent.sock"));
        assert!(fits(&socket_path(Path::new(&long))));
    }

    #[test]
    fn linux_uses_the_runtime_folder() {
        let home = Path::new("/home/ann");
        assert_eq!(
            linux_socket_path(Some(Path::new("/run/user/1000")), home),
            Path::new("/run/user/1000/scriptorium/agent.sock")
        );
        assert_eq!(linux_socket_path(None, home), Path::new("/home/ann/.scriptorium/agent.sock"));
        assert_eq!(linux_socket_path(Some(Path::new("rel")), home), Path::new("/home/ann/.scriptorium/agent.sock"));
    }
}
