//! The link: a Unix domain socket between the agent connector (`scriptorium
//! mcp`, the [`Client`]) and the running app (the [`Server`]), carrying the
//! line protocol in [`proto`]. See `docs/plans/agent-connect.md`, "The
//! internal protocol" and "The socket".
//!
//! - Only the same user gets in: the folder is `0700`, the socket `0600`, and
//!   the server checks each peer's uid.
//! - Whoever holds [`proto::LOCK_NAME`] (an exclusive `flock`, released when
//!   the process dies) owns the socket; a second app doesn't serve.
//! - The client tells "not delivered" ([`Gone::NotDelivered`]: safe to run
//!   the call on the files) from "maybe applied" ([`Gone::AfterTaken`]).

pub mod proto;

#[cfg(unix)]
pub use shell::*;

#[cfg(unix)]
mod shell {
    use std::fmt;
    use std::fs;
    use std::io::{self, BufRead, BufReader, Write};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::proto::{self, Call, Done, Hello, Reply, Request, Route};
    use crate::{LockError, SessionLock};

    /// How long connecting and the hello may take before the connector works
    /// on the files instead (and how long the app waits for a hello).
    pub const HELLO_TIMEOUT: Duration = Duration::from_secs(1);

    /// How long the app waits to write one reply to a connector that isn't
    /// reading, before taking it as gone.
    pub const REPLY_TIMEOUT: Duration = Duration::from_secs(1);

    /// The socket path for this user on this platform: [`proto::socket_path`]
    /// from `$HOME` on macOS, [`proto::linux_socket_path`] elsewhere. Without
    /// `$HOME` (an agent may start the connector with almost no
    /// environment), the user's home folder from the user database; `None`
    /// without either.
    pub fn default_socket_path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from).or_else(|| {
            let user = nix::unistd::User::from_uid(nix::unistd::getuid()).ok().flatten()?;
            Some(user.dir).filter(|d| d.is_absolute())
        })?;
        if cfg!(target_os = "macos") {
            Some(proto::socket_path(&home))
        } else {
            let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
            Some(proto::linux_socket_path(runtime.as_deref(), &home))
        }
    }

    // ---- client -------------------------------------------------------------

    /// Why [`Client::connect`] gave no link. Every case means "work on the
    /// files"; [`NoLink::Mismatch`] also means telling the agent once
    /// ([`proto::mismatch_notice`]).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum NoLink {
        /// No socket, or nobody listening: the app isn't running.
        Absent,
        /// The app answered with another protocol or version.
        Mismatch(Hello),
        /// Anything else (timeout, a bad hello, an OS error), said plainly
        /// for the connector's stderr.
        Failed(String),
    }

    impl fmt::Display for NoLink {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                NoLink::Absent => f.write_str("Scriptorium isn't running"),
                NoLink::Mismatch(h) => write!(f, "Scriptorium {} (link protocol {}) answered", h.version, h.proto),
                NoLink::Failed(m) => f.write_str(m),
            }
        }
    }

    impl std::error::Error for NoLink {}

    /// The link is gone. The client can't be used again; connect anew.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Gone {
        /// The app hung up (EOF, a reset, or a refused send) before any reply
        /// to the call, or before it was sent: the call didn't run, so it may
        /// run on the files.
        NotDelivered,
        /// The call may or may not have been applied: the app hung up after
        /// `taken` and before `done`, or broke the protocol (a garbled line,
        /// a wrong id, a line cut off), which is taken the safe way.
        /// `script`: the script the app's `taken` named, if it got that far.
        AfterTaken { script: Option<String> },
    }

    impl fmt::Display for Gone {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(match self {
                Gone::NotDelivered => "Scriptorium closed before taking the call",
                Gone::AfterTaken { .. } => "Scriptorium closed during the call",
            })
        }
    }

    impl std::error::Error for Gone {}

    /// How a call delivered over the link ended.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Answer {
        /// The app ran it, on `script` if its `taken` said which.
        Done { done: Done, script: Option<String> },
        /// The app didn't take it (not the open script, nothing open): run it
        /// on the files.
        Files,
    }

    /// The connector's end: one connection, one call at a time.
    #[derive(Debug)]
    pub struct Client {
        reader: BufReader<UnixStream>,
        theirs: Hello,
        next_id: u64,
        broken: bool,
    }

    impl Client {
        /// Connect to the app at `socket` and exchange hellos (`ours` first),
        /// each within `timeout`. A different protocol or version closes the
        /// link: [`NoLink::Mismatch`].
        pub fn connect(socket: &Path, ours: &Hello, timeout: Duration) -> Result<Client, NoLink> {
            let stream = connect_within(socket, timeout).map_err(|e| match e.kind() {
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => NoLink::Absent,
                _ => NoLink::Failed(format!("{}: {e}", socket.display())),
            })?;
            let failed = |e: io::Error| NoLink::Failed(format!("hello over {}: {e}", socket.display()));
            stream.set_read_timeout(Some(timeout)).map_err(failed)?;
            stream.set_write_timeout(Some(timeout)).map_err(failed)?;
            write_line(&stream, &ours.encode()).map_err(failed)?;
            let mut reader = BufReader::new(stream);
            let line = match read_line(&mut reader) {
                Ok(Some(l)) => l,
                Ok(None) => return Err(NoLink::Absent),
                Err(e) => return Err(failed(e)),
            };
            let theirs = Hello::decode(&line).map_err(|e| NoLink::Failed(e.to_string()))?;
            if !proto::compatible(ours, &theirs) {
                return Err(NoLink::Mismatch(theirs));
            }
            // Calls have no timeout below the agent's own.
            let stream = reader.get_ref();
            unless_shut(stream.set_read_timeout(None)).map_err(failed)?;
            unless_shut(stream.set_write_timeout(None)).map_err(failed)?;
            Ok(Client { reader, theirs, next_id: 1, broken: false })
        }

        /// The app's hello (its version, and the script open when it was sent).
        pub fn server_hello(&self) -> &Hello {
            &self.theirs
        }

        /// Send `call` and wait for its end. After any [`Gone`] the client is
        /// done for: later calls return [`Gone::NotDelivered`] at once (not
        /// sent).
        pub fn call(&mut self, call: &Call) -> Result<Answer, Gone> {
            let result = self.call_inner(call);
            if result.is_err() {
                self.broken = true;
            }
            result
        }

        fn call_inner(&mut self, call: &Call) -> Result<Answer, Gone> {
            if self.broken || !self.still_up() {
                return Err(Gone::NotDelivered);
            }
            let id = self.next_id;
            self.next_id += 1;
            let line = Request { id, call: call.clone() }.encode();
            // EPIPE (the app hung up since the check) lands here. Rust
            // programs ignore SIGPIPE, so it is an error, not a signal.
            write_line(self.reader.get_ref(), &line).map_err(|_| Gone::NotDelivered)?;
            // `taken`: whether any `taken` came; `script`: the last one named.
            let mut taken = false;
            let mut script: Option<String> = None;
            loop {
                let line = match read_line(&mut self.reader) {
                    Ok(Some(line)) => line,
                    // A clean hang-up before any reply: never delivered.
                    Ok(None) if !taken => return Err(Gone::NotDelivered),
                    Err(e) if !taken && is_hang_up(&e) => return Err(Gone::NotDelivered),
                    _ => return Err(Gone::AfterTaken { script }),
                };
                // Anything but this call's reply: the app is confused, and
                // the call may have run.
                let Ok((rid, reply)) = Reply::decode(&line) else { return Err(Gone::AfterTaken { script }) };
                if rid != id {
                    return Err(Gone::AfterTaken { script });
                }
                match reply {
                    Reply::Taken { script: s } => {
                        taken = true;
                        script = s.or(script);
                    }
                    Reply::Done(done) => return Ok(Answer::Done { done, script }),
                    Reply::Route(Route::Files) if !taken => return Ok(Answer::Files),
                    Reply::Route(_) => return Err(Gone::AfterTaken { script }),
                }
            }
        }

        /// Whether the app is still there before a call: not hung up, and
        /// nothing unasked-for waiting to be read.
        fn still_up(&mut self) -> bool {
            if !self.reader.buffer().is_empty() {
                return false;
            }
            use nix::sys::socket::{MsgFlags, recv};
            use std::os::fd::AsRawFd;
            // A peek that never waits: EOF (0) is a hang-up, data is a line
            // nobody asked for, and "would block" is an app still there.
            let fd = self.reader.get_ref().as_raw_fd();
            matches!(
                recv(fd, &mut [0u8; 1], MsgFlags::MSG_PEEK | MsgFlags::MSG_DONTWAIT),
                Err(nix::errno::Errno::EAGAIN)
            )
        }
    }

    // ---- server -------------------------------------------------------------

    /// Why [`Server::bind`] didn't serve.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum BindError {
        /// Another process (another copy of the app) holds the lock and
        /// serves the socket.
        Owned,
        /// The folder, lock or socket couldn't be set up: what and why.
        Io(String),
    }

    impl fmt::Display for BindError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                BindError::Owned => f.write_str("Another Scriptorium is connected to your AI agents."),
                BindError::Io(m) => f.write_str(m),
            }
        }
    }

    impl std::error::Error for BindError {}

    /// The app's end: owns the socket while it lives, and removes it when
    /// dropped or [`Server::close`]d (if it is still the one it bound).
    #[derive(Debug)]
    pub struct Server {
        listener: UnixListener,
        path: PathBuf,
        /// The socket file's identity, so drop never removes a newer one.
        ino: (u64, u64),
        _lock: SessionLock,
    }

    impl Server {
        /// Serve at [`default_socket_path`].
        pub fn bind() -> Result<Server, BindError> {
            let path = default_socket_path().ok_or_else(|| BindError::Io("no home folder".into()))?;
            Self::bind_at(&path)
        }

        /// Serve at `socket`: make its folder (`0700`, ours, not a link),
        /// take [`proto::LOCK_NAME`] beside it without waiting
        /// ([`BindError::Owned`] if held), remove a stale socket, and bind
        /// with mode `0600`.
        pub fn bind_at(socket: &Path) -> Result<Server, BindError> {
            let io = |what: &Path, e: io::Error| BindError::Io(format!("{}: {e}", what.display()));
            if !proto::fits(socket) {
                return Err(BindError::Io(format!("{}: too long for a socket", socket.display())));
            }
            let dir = proto::socket_dir(socket);
            fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| io(dir, e))?;
            let meta = fs::symlink_metadata(dir).map_err(|e| io(dir, e))?;
            if !meta.is_dir() {
                return Err(BindError::Io(format!("{}: not a folder", dir.display())));
            }
            if meta.uid() != nix::unistd::getuid().as_raw() {
                return Err(BindError::Io(format!("{}: belongs to another user", dir.display())));
            }
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|e| io(dir, e))?;
            let lock = match SessionLock::acquire(&dir.join(proto::LOCK_NAME), Duration::ZERO) {
                Ok(l) => l,
                Err(LockError::Busy) => return Err(BindError::Owned),
                Err(e) => return Err(BindError::Io(e.to_string())),
            };
            // Holding the lock, any socket there is a dead owner's.
            match fs::remove_file(socket) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(io(socket, e)),
            }
            // Not `umask` around the bind: it is process-wide, and another
            // thread making a folder meanwhile would get one it can't enter.
            // The folder is already 0700, so the moment before the chmod
            // exposes nothing.
            let bound = UnixListener::bind(socket);
            let listener = bound.map_err(|e| io(socket, e))?;
            fs::set_permissions(socket, fs::Permissions::from_mode(0o600)).map_err(|e| io(socket, e))?;
            let m = fs::symlink_metadata(socket).map_err(|e| io(socket, e))?;
            Ok(Server { listener, path: socket.to_path_buf(), ino: (m.dev(), m.ino()), _lock: lock })
        }

        /// The socket's path.
        pub fn path(&self) -> &Path {
            &self.path
        }

        /// Wait for the next connection from this user; connections from
        /// other users are closed unread. The hello is the connection's own
        /// ([`Conn::handshake`]), so a slow peer never holds up the accepts.
        pub fn accept(&self) -> io::Result<Conn> {
            loop {
                let (stream, _) = self.listener.accept()?;
                if peer_uid(&stream).is_ok_and(|uid| uid == nix::unistd::getuid().as_raw()) {
                    let writer = Arc::new(Mutex::new(stream.try_clone()?));
                    return Ok(Conn { reader: BufReader::new(stream), writer: Replier(writer) });
                }
            }
        }
    }

    impl Server {
        /// Remove the socket (if it is still the one bound here), so no new
        /// connector finds it: for a clean quit, when the thread blocked in
        /// [`Server::accept`] keeps the server from being dropped. The lock
        /// stays held until the server is dropped or the process ends.
        pub fn close(&self) {
            if fs::symlink_metadata(&self.path).is_ok_and(|m| (m.dev(), m.ino()) == self.ino) {
                let _ = fs::remove_file(&self.path);
            }
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.close();
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
        let cred = nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)?;
        Ok(cred.uid())
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
        let (uid, _) = nix::unistd::getpeereid(stream)?;
        Ok(uid.as_raw())
    }

    /// One connector's connection, on the app's side.
    #[derive(Debug)]
    pub struct Conn {
        reader: BufReader<UnixStream>,
        writer: Replier,
    }

    /// A handle that writes replies on a connection, from any thread: the
    /// app's UI task sends `taken` through one from the same update that runs
    /// the call. Lines from it and from its [`Conn`] never interleave.
    #[derive(Debug, Clone)]
    pub struct Replier(Arc<Mutex<UnixStream>>);

    impl Replier {
        /// Answer request `id`.
        pub fn reply(&self, id: u64, reply: &Reply) -> io::Result<()> {
            let stream = self.0.lock().unwrap_or_else(|e| e.into_inner());
            write_line(&stream, &reply.encode(id))
        }
    }

    impl Conn {
        /// Read the connector's hello (within [`HELLO_TIMEOUT`]) and answer
        /// with `ours` (always, so the connector can say which version is
        /// open). Their hello if the two are [`proto::compatible`], `None` if
        /// not (the connector then closes); an error for no or a bad hello.
        pub fn handshake(&mut self, ours: &Hello) -> io::Result<Option<Hello>> {
            let stream = self.reader.get_ref();
            stream.set_read_timeout(Some(HELLO_TIMEOUT))?;
            stream.set_write_timeout(Some(HELLO_TIMEOUT))?;
            let line = read_line(&mut self.reader)?.ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
            let theirs = Hello::decode(&line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            self.writer.reply_raw(&ours.encode())?;
            let stream = self.reader.get_ref();
            unless_shut(stream.set_read_timeout(None))?;
            // Replies are written from the app's UI thread too (`taken`):
            // a connector that stops reading must never block it. A reply
            // that can't be written within this is a hang-up.
            unless_shut(stream.set_write_timeout(Some(REPLY_TIMEOUT)))?;
            Ok(proto::compatible(ours, &theirs).then_some(theirs))
        }

        /// The next request; `None` when the connector hung up.
        pub fn next_request(&mut self) -> io::Result<Option<Request>> {
            match read_line(&mut self.reader)? {
                None => Ok(None),
                Some(line) => {
                    Request::decode(&line).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
                }
            }
        }

        /// Answer request `id`.
        pub fn reply(&mut self, id: u64, reply: &Reply) -> io::Result<()> {
            self.writer.reply(id, reply)
        }

        /// A [`Replier`] for this connection.
        pub fn replier(&self) -> Replier {
            self.writer.clone()
        }
    }

    impl Replier {
        fn reply_raw(&self, line: &str) -> io::Result<()> {
            let stream = self.0.lock().unwrap_or_else(|e| e.into_inner());
            write_line(&stream, line)
        }
    }

    // ---- lines --------------------------------------------------------------

    fn write_line(mut stream: &UnixStream, line: &str) -> io::Result<()> {
        let mut buf = Vec::with_capacity(line.len() + 1);
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
        stream.write_all(&buf)?;
        stream.flush()
    }

    /// One whole line, without its newline; `None` at a clean EOF. A last
    /// line with no newline was cut off: [`io::ErrorKind::UnexpectedEof`].
    fn read_line(reader: &mut BufReader<UnixStream>) -> io::Result<Option<String>> {
        let mut buf = Vec::new();
        reader.read_until(b'\n', &mut buf)?;
        if buf.is_empty() {
            return Ok(None);
        }
        if buf.pop() != Some(b'\n') {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "a line cut off"));
        }
        String::from_utf8(buf).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// A timeout change, where the peer having already hung up isn't an
    /// error. macOS refuses `setsockopt` on a socket whose peer has closed
    /// (`EINVAL`, both directions shut), where Linux allows it; the next
    /// read or write sees the hang-up either way, so a hello answered and
    /// then a quick hang-up stays a link that is gone, not a failed connect.
    fn unless_shut(set: io::Result<()>) -> io::Result<()> {
        match set {
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => Ok(()),
            other => other,
        }
    }

    /// The peer hung up (as opposed to sending something wrong).
    fn is_hang_up(e: &io::Error) -> bool {
        matches!(
            e.kind(),
            io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionAborted
        )
    }

    /// Connect, giving up after `timeout`. std has no connect timeout for
    /// Unix sockets, and a full backlog makes a blocking connect wait; so the
    /// connect runs on a short-lived thread. One abandoned at the timeout
    /// ends when the app accepts or goes away (it holds nothing else).
    fn connect_within(socket: &Path, timeout: Duration) -> io::Result<UnixStream> {
        let (tx, rx) = std::sync::mpsc::channel();
        let path = socket.to_path_buf();
        std::thread::Builder::new().name("link-connect".into()).spawn(move || {
            let _ = tx.send(UnixStream::connect(&path));
        })?;
        rx.recv_timeout(timeout).unwrap_or_else(|_| Err(io::Error::from(io::ErrorKind::TimedOut)))
    }
}
