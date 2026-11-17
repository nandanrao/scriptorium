//! End to end (plan "agent connect", G3): `scriptorium mcp` with the real
//! app in the loop. The app runs in a headless compositor; the connector is
//! driven over stdio as an agent drives it, and follows the app as it opens
//! and quits:
//!
//! - `where_is_writer` names the script open in the app;
//! - `suggest_changes` shows on the app's card within a second;
//! - the app quits: the next calls go to the files;
//! - the app opens again: the suggestions made meanwhile are there.
//!
//! It needs a display, so `cargo test` skips it unless
//! `SCRIPTORIUM_E2E_WAYLAND` names the absolute path of a *headless*
//! compositor's socket — never the user's own (AGENTS.md, "Trying the app
//! without taking over the user's screen"). The chat scenarios' runner starts
//! one and runs this as S15: `crates/app/tests/scenarios/run.sh S15`.
//!
//! Also read from the environment: `SCRIPTORIUM_E2E_APP` (the app binary;
//! default `scriptorium-app` beside this build's CLI), `SCRIPTORIUM_E2E_CLI`
//! (the CLI; default this build's), `SCRIPTORIUM_E2E_OUT` (where the files
//! and the app's log go; default a temporary folder, removed after a pass),
//! `SCRIPTORIUM_E2E_SHOTS` (a path prefix for screenshots, taken with `grim`
//! when set) and `WAIT_SCALE` (every wait times this, for slow machines).
//! Keys go to the app with `wtype`.
//!
//! Nothing of the user's is touched: the app and the connector share a
//! private `HOME`, config, state and `XDG_RUNTIME_DIR` (so the agent socket
//! is private too), and the keyring is off.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const SCRIPT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Scene Heading"><Text>EXT. FARM HOUSE - NIGHT</Text></Paragraph>
    <Paragraph Type="Action"><Text>Laura smiles.</Text></Paragraph>
    <Paragraph Type="Character"><Text>CESAR</Text></Paragraph>
    <Paragraph Type="Dialogue"><Text>I am go to the town tomorrow.</Text></Paragraph>
    <Paragraph Type="Scene Heading"><Text>INT. KITCHEN - DAY</Text></Paragraph>
    <Paragraph Type="Action"><Text>Laura pours coffee for two.</Text></Paragraph>
    <Paragraph Type="Character"><Text>LAURA</Text></Paragraph>
    <Paragraph Type="Dialogue"><Text>You will miss the bus.</Text></Paragraph>
  </Content>
</FinalDraft>
"#;

/// What the connector answers `where_is_writer` with when the app is shut.
const NOT_OPEN: &str = "Scriptorium isn't open";

/// How long to wait, scaled by `WAIT_SCALE`.
fn secs(s: f64) -> Duration {
    let scale = std::env::var("WAIT_SCALE").ok().and_then(|v| v.parse::<f64>().ok()).unwrap_or(1.0);
    Duration::from_secs_f64(s * scale.max(1.0))
}

/// Polls `f` every 50 ms until it holds or `within` passes.
fn wait_until(within: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + within;
    loop {
        if f() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The places and programs one run uses.
struct Setup {
    /// The headless compositor's socket (absolute).
    wayland: PathBuf,
    app: PathBuf,
    cli: PathBuf,
    /// The scratch folder: the script, its review, config and state.
    work: PathBuf,
    /// The private runtime folder (short: socket paths are limited to 108
    /// bytes), where the agent socket goes.
    run: PathBuf,
    shots: Option<String>,
    /// The work folder was made here, so it goes after a pass.
    temporary: bool,
}

impl Setup {
    /// `None` (the test skips) unless a headless compositor is named.
    fn from_env() -> Option<Self> {
        let wayland = PathBuf::from(std::env::var_os("SCRIPTORIUM_E2E_WAYLAND")?);
        assert!(
            wayland.is_absolute(),
            "SCRIPTORIUM_E2E_WAYLAND must be the absolute path of a headless compositor's socket"
        );
        let cli = std::env::var_os("SCRIPTORIUM_E2E_CLI")
            .map_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_scriptorium")), PathBuf::from);
        let app = std::env::var_os("SCRIPTORIUM_E2E_APP").map_or_else(
            || Path::new(env!("CARGO_BIN_EXE_scriptorium")).with_file_name("scriptorium-app"),
            PathBuf::from,
        );
        assert!(app.is_file(), "no app at {} (build it, or set SCRIPTORIUM_E2E_APP)", app.display());
        let (work, temporary) = match std::env::var_os("SCRIPTORIUM_E2E_OUT") {
            Some(out) => (PathBuf::from(out), false),
            None => (std::env::temp_dir().join(format!("scriptorium-mcp-link-{}", std::process::id())), true),
        };
        let _ = std::fs::remove_dir_all(&work);
        for sub in ["config", "state"] {
            std::fs::create_dir_all(work.join(sub)).unwrap();
        }
        let run = PathBuf::from(format!("/tmp/scrl.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&run);
        std::fs::create_dir_all(&run).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(work.join("film.fdx"), SCRIPT).unwrap();
        let shots = std::env::var("SCRIPTORIUM_E2E_SHOTS").ok();
        Some(Self { wayland, app, cli, work, run, shots, temporary })
    }

    fn script(&self) -> PathBuf {
        self.work.join("film.fdx").canonicalize().unwrap()
    }

    fn socket(&self) -> PathBuf {
        self.run.join("scriptorium/agent.sock")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.work.join("state/scriptorium/scriptorium.log")).unwrap_or_default()
    }

    /// `cmd` with the private environment the app and the CLI share: no
    /// display of the user's, no config, state, keyring or runtime folder of
    /// theirs.
    fn private(&self, cmd: &mut Command) {
        cmd.env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("SWAYSOCK")
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .env_remove("SCRIPTORIUM_REPLAY")
            .env("HOME", &self.work)
            .env("XDG_CONFIG_HOME", self.work.join("config"))
            .env("XDG_STATE_HOME", self.work.join("state"))
            .env("XDG_RUNTIME_DIR", &self.run)
            .env("SCRIPTORIUM_KEYRING", "off");
    }

    /// The CLI in the work folder; its stdout, after checking it succeeded.
    fn cli(&self, args: &[&str]) -> String {
        let mut cmd = Command::new(&self.cli);
        self.private(&mut cmd);
        let out = cmd.args(args).current_dir(&self.work).stdin(Stdio::null()).output().unwrap();
        assert!(out.status.success(), "scriptorium {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A key chord into the compositor (only ever the headless one). Each
    /// `wtype` run drops its first key, so it leads with a Shift.
    fn keys(&self, args: &[&str]) {
        let ok = Command::new("wtype")
            .env("WAYLAND_DISPLAY", &self.wayland)
            .args(["-k", "Shift_L", "-s", "120"])
            .args(args)
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "wtype {args:?} failed (is wtype installed?)");
    }

    /// A screenshot, when asked for. The app answers on its socket before
    /// its first frame is up, and a blank output compresses to a few KB, so
    /// it is taken again (for a few seconds at most) until it has something
    /// in it.
    fn shot(&self, name: &str) {
        let Some(prefix) = &self.shots else { return };
        let file = format!("{prefix}{name}.png");
        wait_until(secs(5.0), || {
            let took = Command::new("grim").env("WAYLAND_DISPLAY", &self.wayland).arg(&file).status();
            took.is_ok_and(|s| s.success()) && std::fs::metadata(&file).is_ok_and(|m| m.len() > 16 * 1024)
        });
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.run);
    }
}

/// The app, in a process group of its own (with its D-Bus session, when
/// `dbus-run-session` is there), so stopping it stops nothing else.
struct App {
    child: Child,
}

impl App {
    fn start(s: &Setup) -> Self {
        let dbus =
            Command::new("sh").args(["-c", "command -v dbus-run-session"]).output().is_ok_and(|o| o.status.success());
        let mut cmd = if dbus {
            let mut c = Command::new("dbus-run-session");
            c.arg("--").arg(&s.app);
            c
        } else {
            Command::new(&s.app)
        };
        s.private(&mut cmd);
        use std::os::unix::process::CommandExt;
        let log = std::fs::File::create(s.work.join("app.log")).unwrap();
        let child = cmd
            .arg("film.fdx")
            .current_dir(&s.work)
            .env("WAYLAND_DISPLAY", &s.wayland)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .process_group(0)
            .spawn()
            .unwrap();
        Self { child }
    }

    fn exited(&mut self) -> Option<i32> {
        self.child.try_wait().unwrap().map(|s| s.code().unwrap_or(-1))
    }

    fn kill(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = Command::new("kill").args(["-TERM", "--", &format!("-{}", self.child.id())]).status();
            if !wait_until(Duration::from_secs(5), || self.child.try_wait().ok().flatten().is_some()) {
                let _ = Command::new("kill").args(["-KILL", "--", &format!("-{}", self.child.id())]).status();
                let _ = self.child.wait();
            }
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.kill();
    }
}

/// `scriptorium mcp` as an agent starts it: in `/`, with almost no
/// environment, the same home and runtime folder as the app.
struct Connector {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Connector {
    fn start(s: &Setup) -> Self {
        let mut child = Command::new(&s.cli)
            .arg("mcp")
            .current_dir("/")
            .env_clear()
            .env("HOME", &s.work)
            .env("XDG_RUNTIME_DIR", &s.run)
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(s.work.join("connector.err")).unwrap())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        // Lines are read on a thread, so a connector that stops answering
        // fails the test instead of hanging it.
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in stdout.lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self { child, stdin, lines, next_id: 1 }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
        let line = self.lines.recv_timeout(secs(30.0)).unwrap_or_else(|_| panic!("no answer to {msg}"));
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }

    /// A tool call's text and whether it was an error.
    fn call(&mut self, name: &str, args: Value) -> (String, bool) {
        let r = self.request("tools/call", json!({ "name": name, "arguments": args }));
        let result = &r["result"];
        (result["content"][0]["text"].as_str().unwrap_or("").to_string(), result["isError"] == true)
    }

    fn ok(&mut self, name: &str, args: Value) -> String {
        let (text, err) = self.call(name, args);
        assert!(!err, "{name} failed: {text}");
        text
    }

    fn json(&mut self, name: &str, args: Value) -> Value {
        let text = self.ok(name, args);
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: not JSON ({e}): {text}"))
    }
}

impl Drop for Connector {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The checks made, printed as they go; the test fails at the end with
/// every one that failed, so one failure doesn't hide the rest.
#[derive(Default)]
struct Checks(Vec<String>);

impl Checks {
    fn check(&mut self, what: &str, ok: bool, detail: impl std::fmt::Display) {
        if ok {
            eprintln!("  ok   {what}");
        } else {
            eprintln!("  FAIL {what}: {detail}");
            self.0.push(format!("{what}: {detail}"));
        }
    }
}

/// `where_is_writer`'s "Showing:" line (what the card shows), for a short
/// failure message.
fn showing(place: &str) -> &str {
    place.lines().find(|l| l.starts_with("Showing:")).unwrap_or(place)
}

/// The statuses of the suggestions `list_suggestions` lists, in order.
fn statuses(list: &Value) -> Vec<String> {
    list.as_array().map_or(vec![], |a| a.iter().map(|s| s["status"].as_str().unwrap_or("?").to_string()).collect())
}

/// Starts the app and waits until it serves agents.
fn open_app(s: &Setup, c: &mut Checks) -> App {
    let mut app = App::start(s);
    let up = wait_until(secs(30.0), || s.socket().exists() && s.log().contains("serving AI agents"));
    c.check("the app starts and serves agents", up, format!("exit {:?}; log:\n{}", app.exited(), s.log()));
    assert!(up, "the app never served agents; see {}", s.work.display());
    app
}

/// Ctrl+Q, then waits for the app to be gone.
fn quit_app(s: &Setup, app: &mut App, c: &mut Checks) {
    s.keys(&["-k", "Escape"]);
    s.keys(&["-M", "ctrl", "-k", "q", "-m", "ctrl"]);
    let gone = wait_until(secs(15.0), || app.exited().is_some());
    c.check("Ctrl+Q quits the app", gone, "still running");
    if !gone {
        app.kill();
    }
    c.check("it quit cleanly", app.exited() == Some(0), format!("exit {:?}", app.exited()));
    c.check("its agent socket is gone", !s.socket().exists(), s.socket().display());
}

#[test]
fn the_connector_follows_the_app_as_it_opens_and_quits() {
    let Some(s) = Setup::from_env() else {
        eprintln!(
            "skipped: needs a headless compositor (SCRIPTORIUM_E2E_WAYLAND); \
             run crates/app/tests/scenarios/run.sh S15"
        );
        return;
    };
    let mut c = Checks::default();
    let script = s.script();
    let script_str = script.to_string_lossy().into_owned();

    // A review the writer has finished: the app opens on it, with no card.
    s.cli(&["suggest", "notes.review.txt", "film.fdx", "--para", "1", "--after", "Laura grins.", "--why", "sharper"]);
    s.cli(&["reviews", "film.fdx", "add", "notes.review.txt"]);
    s.cli(&["decide", "film.fdx", "1", "keep"]);

    let mut app = open_app(&s, &mut c);
    let mut agent = Connector::start(&s);
    let init = agent.request(
        "initialize",
        json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "g3", "version": "0" } }),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "scriptorium", "{init}");

    // where_is_writer names the open script.
    let place = agent.ok("where_is_writer", json!({}));
    c.check(
        "where_is_writer names the script open in the app",
        place.contains(&format!("Open in Scriptorium: {script_str}")),
        &place,
    );
    c.check("and the review showing", place.contains("Showing: the review notes review"), &place);
    s.shot("01-open");

    // suggest_changes, with no script: the app's open one. It shows on the
    // card within a second.
    let rows = agent.json("list_paragraphs", json!({}));
    let id_of = |rows: &Value, text: &str| {
        rows["paragraphs"].as_array().unwrap().iter().find(|p| p["text"] == text).unwrap()["id"].as_u64().unwrap()
    };
    let dialogue = id_of(&rows, "I am go to the town tomorrow.");
    let added = agent.json(
        "suggest_changes",
        json!({ "review_file": "notes.review.txt", "suggestions": [
            { "paragraph_id": dialogue, "options": ["I am going to town tomorrow."], "why": "grammar" },
        ] }),
    );
    c.check("suggest_changes through the app adds one", added["added"] == 1, &added);
    let mut last = String::new();
    let on_card = wait_until(secs(1.0), || {
        last = agent.ok("where_is_writer", json!({}));
        last.contains("the card shows suggestion #2")
    });
    s.shot("02-suggested");
    c.check("the new suggestion is on the card within a second", on_card, showing(&last));
    let live = agent.json("list_suggestions", json!({}));
    c.check("the app lists it", statuses(&live) == ["kept", "pending"], &live);
    let review = std::fs::read_to_string(s.work.join("notes.review.txt")).unwrap_or_default();
    c.check("the app wrote it to the review file", review.contains("I am going to town tomorrow."), &review);

    // Quit: the next calls go to the files.
    quit_app(&s, &mut app, &mut c);
    let place = agent.ok("where_is_writer", json!({}));
    c.check("with the app shut, where_is_writer says so", place.contains(NOT_OPEN), &place);
    let (text, err) = agent.call("list_suggestions", json!({}));
    c.check("a call with no script says to name one", err && text.contains(NOT_OPEN), &text);
    let coffee = id_of(&agent.json("list_paragraphs", json!({ "script": script_str })), "Laura pours coffee for two.");
    let added = agent.json(
        "suggest_changes",
        json!({ "script": script_str, "review_file": "notes.review.txt", "suggestions": [
            { "paragraph_id": coffee, "options": ["Laura pours coffee for one."], "why": "she's alone" },
        ] }),
    );
    c.check("suggest_changes on the files adds one", added["added"] == 1, &added);
    let listed = s.cli(&["suggestions", "film.fdx", "--json"]);
    let on_files: Value = serde_json::from_str(&listed).unwrap_or(Value::Null);
    c.check("the files hold all three", statuses(&on_files) == ["kept", "pending", "pending"], &listed);

    // Open again: the same connector reaches the app, and the suggestions
    // made while it was shut are there.
    let mut app = open_app(&s, &mut c);
    let place = agent.ok("where_is_writer", json!({}));
    c.check("after reopening, the next call reaches the app", place.contains("Open in Scriptorium"), &place);
    c.check("the card shows the first one left", place.contains("the card shows suggestion #2"), showing(&place));
    let live = agent.json("list_suggestions", json!({}));
    c.check("the app lists all three", statuses(&live) == ["kept", "pending", "pending"], &live);
    s.shot("03-reopened");
    quit_app(&s, &mut app, &mut c);

    let log = s.log();
    c.check("the app never panicked", !log.contains("PANIC"), &log);
    drop(agent);
    assert!(c.0.is_empty(), "{} check(s) failed (files in {}):\n{}", c.0.len(), s.work.display(), c.0.join("\n"));
    if s.temporary {
        let _ = std::fs::remove_dir_all(&s.work);
    }
}
