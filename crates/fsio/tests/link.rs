//! The link's shell on real sockets in temporary folders.
#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use scriptorium_fsio::link::proto::{Call, Done, Hello, Reply, Route};
use scriptorium_fsio::link::{Answer, BindError, Client, Gone, HELLO_TIMEOUT, NoLink, Server};
use serde_json::json;

/// A fresh folder; the socket goes in a subfolder the server makes. Short
/// (under /tmp), so socket paths fit.
fn scratch() -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let d = PathBuf::from("/tmp").join(format!("sclink-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn sock(dir: &Path) -> PathBuf {
    dir.join("s").join("agent.sock")
}

fn hello(version: &str) -> Hello {
    Hello { version: version.into(), ..Hello::this_build() }
}

fn call(name: &str) -> Call {
    Call { name: name.into(), arguments: json!({"script": "/x.fdx"}) }
}

/// Serve one connection: the hello, then `script` for each request.
fn serve_one(server: Server, ours: Hello, script: impl FnOnce(&mut scriptorium_fsio::link::Conn) + Send + 'static) {
    thread::spawn(move || {
        let mut conn = server.accept().unwrap();
        if conn.handshake(&ours).unwrap().is_some() {
            script(&mut conn);
        }
        // `server` (and the socket) lives until here.
        drop(conn);
        drop(server);
    });
}

#[test]
fn hello_then_calls_are_answered() {
    let dir = scratch();
    let path = sock(&dir);
    let server = Server::bind_at(&path).unwrap();
    let mut theirs = Hello::this_build();
    theirs.script = Some("/F/PURPLE LIST.fdx".into());
    serve_one(server, theirs, |conn| {
        let r = conn.next_request().unwrap().unwrap();
        assert_eq!(r.call.name, "read_script");
        conn.reply(r.id, &Reply::Taken { script: None }).unwrap();
        conn.reply(r.id, &Reply::Done(Done { text: "lines".into(), is_error: false })).unwrap();
        let r = conn.next_request().unwrap().unwrap();
        conn.reply(r.id, &Reply::Route(Route::Files)).unwrap();
        assert!(conn.next_request().unwrap().is_none(), "the client hung up");
    });
    let mut ours = Hello::this_build();
    ours.exe = Some("/A/scriptorium".into());
    let mut client = Client::connect(&path, &ours, HELLO_TIMEOUT).unwrap();
    assert_eq!(client.server_hello().script.as_deref(), Some("/F/PURPLE LIST.fdx"));
    assert_eq!(
        client.call(&call("read_script")).unwrap(),
        Answer::Done { done: Done { text: "lines".into(), is_error: false }, script: None }
    );
    assert_eq!(client.call(&call("list_suggestions")).unwrap(), Answer::Files);
}

#[test]
fn a_version_mismatch_gives_no_link() {
    let dir = scratch();
    let path = sock(&dir);
    let server = Server::bind_at(&path).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut conn = server.accept().unwrap();
        tx.send(conn.handshake(&hello("0.5.1")).unwrap()).unwrap();
        drop(server);
    });
    let err = Client::connect(&path, &hello("0.6.0"), HELLO_TIMEOUT).unwrap_err();
    assert_eq!(err, NoLink::Mismatch(hello("0.5.1")));
    // The app saw the mismatch too and serves nothing on that connection.
    assert_eq!(rx.recv().unwrap(), None);
}

#[test]
fn no_app_is_absent() {
    let dir = scratch();
    assert_eq!(Client::connect(&sock(&dir), &Hello::this_build(), HELLO_TIMEOUT).unwrap_err(), NoLink::Absent);
    // A socket file nobody listens on (a crashed app's): absent too.
    fs::create_dir_all(dir.join("s")).unwrap();
    drop(UnixListener::bind(sock(&dir)).unwrap());
    assert_eq!(Client::connect(&sock(&dir), &Hello::this_build(), HELLO_TIMEOUT).unwrap_err(), NoLink::Absent);
}

#[test]
fn a_silent_app_times_out() {
    let dir = scratch();
    fs::create_dir_all(dir.join("s")).unwrap();
    let listener = UnixListener::bind(sock(&dir)).unwrap();
    let err = Client::connect(&sock(&dir), &Hello::this_build(), Duration::from_millis(100)).unwrap_err();
    assert!(matches!(err, NoLink::Failed(_)), "{err:?}");
    drop(listener);
}

#[test]
fn a_stale_socket_is_replaced() {
    let dir = scratch();
    let path = sock(&dir);
    fs::create_dir_all(dir.join("s")).unwrap();
    drop(UnixListener::bind(&path).unwrap());
    assert!(path.exists());
    let server = Server::bind_at(&path).unwrap();
    serve_one(server, Hello::this_build(), |_| {});
    assert!(Client::connect(&path, &Hello::this_build(), HELLO_TIMEOUT).is_ok());
}

#[test]
fn a_second_bind_is_owned() {
    let dir = scratch();
    let path = sock(&dir);
    let first = Server::bind_at(&path).unwrap();
    assert_eq!(Server::bind_at(&path).unwrap_err(), BindError::Owned);
    // The first is untouched and still serves.
    serve_one(first, Hello::this_build(), |_| {});
    assert!(Client::connect(&path, &Hello::this_build(), HELLO_TIMEOUT).is_ok());
}

#[test]
fn dropping_the_server_frees_the_socket_and_lock() {
    let dir = scratch();
    let path = sock(&dir);
    drop(Server::bind_at(&path).unwrap());
    assert!(!path.exists());
    drop(Server::bind_at(&path).unwrap());
}

#[test]
fn modes_are_0700_and_0600() {
    let dir = scratch();
    let path = sock(&dir);
    // A folder that already exists with wider permissions is narrowed.
    fs::create_dir_all(dir.join("s")).unwrap();
    fs::set_permissions(dir.join("s"), fs::Permissions::from_mode(0o755)).unwrap();
    let _server = Server::bind_at(&path).unwrap();
    let mode = |p: &Path| fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir.join("s")), 0o700);
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn a_linked_folder_is_refused() {
    let dir = scratch();
    fs::create_dir_all(dir.join("real")).unwrap();
    std::os::unix::fs::symlink(dir.join("real"), dir.join("s")).unwrap();
    assert!(matches!(Server::bind_at(&sock(&dir)), Err(BindError::Io(_))));
}

#[test]
fn eof_after_taken_is_gone_after_taken() {
    let dir = scratch();
    let path = sock(&dir);
    let server = Server::bind_at(&path).unwrap();
    serve_one(server, Hello::this_build(), |conn| {
        let r = conn.next_request().unwrap().unwrap();
        conn.reply(r.id, &Reply::Taken { script: None }).unwrap();
    });
    let mut client = Client::connect(&path, &Hello::this_build(), HELLO_TIMEOUT).unwrap();
    assert_eq!(client.call(&call("suggest_changes")), Err(Gone::AfterTaken { script: None }));
    // Done for: the next call isn't sent.
    assert_eq!(client.call(&call("read_script")), Err(Gone::NotDelivered));
}

#[test]
fn eof_before_taken_is_not_delivered() {
    let dir = scratch();
    let path = sock(&dir);
    let server = Server::bind_at(&path).unwrap();
    serve_one(server, Hello::this_build(), |conn| {
        conn.next_request().unwrap().unwrap();
    });
    let mut client = Client::connect(&path, &Hello::this_build(), HELLO_TIMEOUT).unwrap();
    assert_eq!(client.call(&call("suggest_changes")), Err(Gone::NotDelivered));
}

#[test]
fn an_app_gone_between_calls_is_not_delivered() {
    let dir = scratch();
    let path = sock(&dir);
    let server = Server::bind_at(&path).unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    serve_one(server, Hello::this_build(), move |conn| {
        let r = conn.next_request().unwrap().unwrap();
        conn.reply(r.id, &Reply::Taken { script: None }).unwrap();
        conn.reply(r.id, &Reply::Done(Done { text: "ok".into(), is_error: false })).unwrap();
        tx.send(()).unwrap();
    });
    let mut client = Client::connect(&path, &Hello::this_build(), HELLO_TIMEOUT).unwrap();
    assert!(matches!(client.call(&call("read_script")), Ok(Answer::Done { .. })));
    rx.recv().unwrap();
    // Let the server thread close the connection.
    for _ in 0..100 {
        if !path.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(client.call(&call("suggest_changes")), Err(Gone::NotDelivered));
}

#[test]
fn a_wrong_reply_id_after_taken_is_gone_after_taken() {
    // A hand-written app that answers the wrong id after `taken`.
    let dir = scratch();
    fs::create_dir_all(dir.join("s")).unwrap();
    let path = sock(&dir);
    let listener = UnixListener::bind(&path).unwrap();
    thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w: &UnixStream = &s;
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        writeln!(w, "{}", Hello::this_build().encode()).unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        writeln!(w, "{}", Reply::Taken { script: None }.encode(1)).unwrap();
        writeln!(w, "{}", Reply::Done(Done { text: String::new(), is_error: false }).encode(99)).unwrap();
        // Keep the connection open until the client gives up.
        line.clear();
        let _ = r.read_line(&mut line);
    });
    let mut client = Client::connect(&path, &Hello::this_build(), HELLO_TIMEOUT).unwrap();
    assert_eq!(client.call(&call("suggest_changes")), Err(Gone::AfterTaken { script: None }));
}

#[test]
fn a_long_home_folder_uses_the_short_path() {
    use scriptorium_fsio::link::proto::{fits, socket_path};
    let home = PathBuf::from(format!("/Users/{}", "n".repeat(60)));
    let p = socket_path(&home);
    assert_eq!(p, home.join(".scriptorium/agent.sock"));
    assert!(fits(&p));
    // And the server refuses a path that can't be a socket address.
    let too_long = PathBuf::from(format!("/tmp/{}/agent.sock", "n".repeat(120)));
    assert!(matches!(Server::bind_at(&too_long), Err(BindError::Io(_))));
}

/// A hand-written app: answers the hello, reads one request, then writes
/// `lines` (raw) and keeps the connection open until the client gives up.
fn raw_app(lines: &'static [&'static str]) -> PathBuf {
    let dir = scratch();
    fs::create_dir_all(dir.join("s")).unwrap();
    let path = sock(&dir);
    let listener = UnixListener::bind(&path).unwrap();
    thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w: &UnixStream = &s;
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        writeln!(w, "{}", Hello::this_build().encode()).unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        for l in lines {
            w.write_all(l.as_bytes()).unwrap();
        }
        line.clear();
        let _ = r.read_line(&mut line);
    });
    path
}

fn one_call(lines: &'static [&'static str]) -> Result<Answer, Gone> {
    let path = raw_app(lines);
    let mut client = Client::connect(&path, &Hello::this_build(), HELLO_TIMEOUT).unwrap();
    client.call(&call("suggest_changes"))
}

#[test]
fn protocol_breaks_before_taken_are_after_taken() {
    // Only a clean hang-up or `route: files` says the call didn't run.
    assert_eq!(one_call(&["garbage\n"]), Err(Gone::AfterTaken { script: None }));
    assert_eq!(one_call(&["{\"id\":99,\"taken\":true}\n"]), Err(Gone::AfterTaken { script: None }));
    assert_eq!(one_call(&["{\"id\":1,\"route\":\"files\"}\n"]), Ok(Answer::Files));
}

#[test]
fn a_cut_off_line_is_after_taken() {
    // A raw half line then EOF.
    let dir = scratch();
    fs::create_dir_all(dir.join("s")).unwrap();
    let path = sock(&dir);
    let listener = UnixListener::bind(&path).unwrap();
    thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w: &UnixStream = &s;
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        writeln!(w, "{}", Hello::this_build().encode()).unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        w.write_all(b"{\"id\":1,\"ta").unwrap();
    });
    let mut client = Client::connect(&path, &Hello::this_build(), HELLO_TIMEOUT).unwrap();
    assert_eq!(client.call(&call("suggest_changes")), Err(Gone::AfterTaken { script: None }));
}

#[test]
fn done_without_taken_and_repeated_taken_are_accepted() {
    let done = Done { text: "ok".into(), is_error: false };
    assert_eq!(
        one_call(&["{\"id\":1,\"done\":{\"text\":\"ok\",\"is_error\":false}}\n"]),
        Ok(Answer::Done { done: done.clone(), script: None })
    );
    assert_eq!(
        one_call(&[
            "{\"id\":1,\"taken\":true,\"script\":\"/F/a.fdx\"}\n",
            "{\"id\":1,\"taken\":true}\n",
            "{\"id\":1,\"done\":{\"text\":\"ok\",\"is_error\":false}}\n",
        ]),
        Ok(Answer::Done { done, script: Some("/F/a.fdx".into()) })
    );
}

#[test]
fn taken_names_the_script_and_a_hang_up_keeps_it() {
    let dir = scratch();
    let path = sock(&dir);
    let server = Server::bind_at(&path).unwrap();
    serve_one(server, Hello::this_build(), |conn| {
        let r = conn.next_request().unwrap().unwrap();
        conn.reply(r.id, &Reply::Taken { script: Some("/F/PURPLE LIST.fdx".into()) }).unwrap();
    });
    let mut client = Client::connect(&path, &Hello::this_build(), HELLO_TIMEOUT).unwrap();
    assert_eq!(
        client.call(&call("suggest_changes")),
        Err(Gone::AfterTaken { script: Some("/F/PURPLE LIST.fdx".into()) })
    );
}

#[test]
fn a_repliers_taken_and_the_conns_done_arrive_in_order() {
    let dir = scratch();
    let path = sock(&dir);
    let server = Server::bind_at(&path).unwrap();
    serve_one(server, Hello::this_build(), |conn| {
        for _ in 0..20 {
            let r = conn.next_request().unwrap().unwrap();
            // As the app does: `taken` from the UI thread, `done` back on
            // the connection's thread once the UI hands the result over.
            let replier = conn.replier();
            let id = r.id;
            thread::spawn(move || replier.reply(id, &Reply::Taken { script: Some("/s.fdx".into()) }).unwrap())
                .join()
                .unwrap();
            conn.reply(id, &Reply::Done(Done { text: format!("n{id}"), is_error: false })).unwrap();
        }
    });
    let mut client = Client::connect(&path, &Hello::this_build(), HELLO_TIMEOUT).unwrap();
    for n in 1..=20 {
        assert_eq!(
            client.call(&call("read_script")).unwrap(),
            Answer::Done { done: Done { text: format!("n{n}"), is_error: false }, script: Some("/s.fdx".into()) }
        );
    }
}

#[test]
fn close_removes_the_socket_but_keeps_the_lock() {
    let dir = scratch();
    let path = sock(&dir);
    let server = Server::bind_at(&path).unwrap();
    server.close();
    assert!(!path.exists());
    assert_eq!(Client::connect(&path, &Hello::this_build(), HELLO_TIMEOUT).unwrap_err(), NoLink::Absent);
    assert_eq!(Server::bind_at(&path).unwrap_err(), BindError::Owned);
    // A socket at the path that isn't the one bound here survives the drop.
    let other = UnixListener::bind(&path).unwrap();
    drop(server);
    assert!(path.exists());
    drop(other);
    drop(Server::bind_at(&path).unwrap());
}

/// A connector that stops reading never blocks the app's writes: a reply
/// that can't be written within `REPLY_TIMEOUT` fails (the app takes the
/// connector as gone) instead of waiting forever.
#[test]
fn a_connector_that_never_reads_cannot_block_a_reply() {
    let dir = scratch();
    let path = sock(&dir);
    let server = Server::bind_at(&path).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut conn = server.accept().unwrap();
        conn.handshake(&Hello::this_build()).unwrap();
        let replier = conn.replier();
        let big = Reply::Done(Done { text: "x".repeat(64 * 1024), is_error: false });
        let started = std::time::Instant::now();
        // Fill the socket's buffers: some write must fail, not block.
        let failed = (0..10_000).any(|i| replier.reply(i, &big).is_err());
        tx.send((failed, started.elapsed())).unwrap();
        drop(server);
    });
    // Hello, then never read again.
    let mut stream = UnixStream::connect(&path).unwrap();
    writeln!(stream, "{}", Hello::this_build().encode()).unwrap();
    let mut first = String::new();
    BufReader::new(stream.try_clone().unwrap()).read_line(&mut first).unwrap();
    let (failed, took) = rx.recv_timeout(Duration::from_secs(20)).expect("a reply blocked");
    assert!(failed);
    assert!(took < Duration::from_secs(10), "{took:?}");
    drop(stream);
}
