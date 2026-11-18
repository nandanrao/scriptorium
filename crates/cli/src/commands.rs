//! The `scriptorium` commands. Each returns the text it prints on stdout, or
//! an error message; things to tell the user besides (a review that can't be
//! read, a session that disagrees with a file) go to `notes`, which the
//! binary prints on stderr.
//!
//! Reviewing works on the same session as the app. Every command that
//! changes something opens the saved session (`<script>.scriptorium.json`),
//! applies one step through the same `scriptorium_review::Session` the app
//! uses, and saves it back (review files first, then the session). A session
//! that can't be replayed is never overwritten. Commands that only read never
//! write anything. A command that writes holds the script's [`SessionLock`]
//! from opening the session to saving it, so another program (the app,
//! another command) never writes it in between.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use scriptorium_agent::ToolCall;
use scriptorium_chat::exec::{Caller, FileScript, Script as _, run_call};
use scriptorium_core::outline::annotate;
use scriptorium_core::text::looks_same;
use scriptorium_core::{ElementKind, ParaId, Script};
use scriptorium_fdx::FdxFile;
use scriptorium_review::file::{Case, FORMAT_HEADER, append_block, insert_id, parse_typed_lines};
use scriptorium_review::matching::{Para, match_entries};
use scriptorium_review::sessionfile;
use scriptorium_review::{Entry, Line, Review, Session, Shape, Unplaced};
use scriptorium_tools::{Spot, block_entry, item_numbers, suggestion_rows};
use serde::Serialize;
use serde_json::Value;

use crate::fsio::{LockError, SessionLock, write_atomic};
use scriptorium_fsio::opened::{Opened, file_name, load, new_review_id, no_reviews, review_salt};
use scriptorium_fsio::ops;
pub use scriptorium_fsio::ops::{Choice, SuggestionRef, output_path};

pub const USAGE: &str = "\
scriptorium: screenplay tools

SCRIPTS
  scriptorium info <script.fdx>          summary: paragraphs, scenes, characters
  scriptorium dump <script.fdx> [--json] every paragraph with its id, type, scene and speaker
  scriptorium roundtrip <script.fdx>     check that a no-op save is byte-identical
  info and dump read the script as it is now in the saved session (with its
  decisions and edits); roundtrip reads the file. Like every command that
  opens a script, info and dump first find again a version lined up with it
  that changed outside Scriptorium (updating its links file) and say so.

APPS
  scriptorium apps <script.fdx> [--json]
        the apps in <script folder>/apps/: name, title, description, folder and
        any problems (what stops one opening). Exit 1 if any app has problems.
        With --json: [{name, title, description, folder, ok, problems,
        loaded, error}] (loaded and error are null here).

REVIEWING
  These work on the same saved session as the app (<script>.scriptorium.json
  next to the script), so an agent and a person see each other's decisions.
  The session remembers which review files are attached to the script; a
  command uses those when you don't name one. Review files named on a command
  that saves (decide, edit, undo, redo, apply) are attached too.
  Suggestions are numbered from 1 in script order, as `suggestions` lists them.
  <review> below is a review file, or --review <name or file> to pick one of
  the attached reviews; it can be left out when only one is attached.

  scriptorium reviews <script.fdx>
        the attached review files, with progress
  scriptorium reviews <script.fdx> add <review.txt>...
  scriptorium reviews <script.fdx> remove <review.txt>...
        attach or detach review files (detaching keeps the text as it is)
  scriptorium review <script.fdx> [<review.txt>...]
        summary of each review: suggestions, decisions, lines not found
  scriptorium suggestions <script.fdx> [<review>] [--pending] [--json]
        list the suggestions, with status and current text
  scriptorium decide <script.fdx> [<review>] <n> accept [<option>]
  scriptorium decide <script.fdx> [<review>] <n> keep | undecide
  scriptorium decide <script.fdx> [<review>] <n> rewrite <text>
  scriptorium decide <script.fdx> [<review>] <n> --rewrite-block '<TYPE: text lines>'
        decide suggestion n (option numbers start at 1; --rewrite-block
        replaces a block's lines, '' or '[cut]' cuts them); n can also be
        e<entry>, the suggestion's stable place in the review file
        (`entry` in `tool ... list_suggestions`)
  scriptorium edit <script.fdx> <paragraph id> <text> [-r <review.txt>]...
        replace one paragraph's text (ids from `dump`)
  scriptorium undo <script.fdx> [-r <review.txt>]...
  scriptorium redo <script.fdx> [-r <review.txt>]...
  scriptorium suggest <review.txt> <script.fdx> (--para <id> | --before <text>)
                      --after <text> [--after <text>]... [--why <text>]
  scriptorium suggest <review.txt> <script.fdx>
                      (--para <id> | --paras <a>-<b> | --paras <a>,<b>,... | --insert-after <id>)
                      --block '<TYPE: text>\n<TYPE: text>...' [--block ...]... [--why <text>]
        add a suggestion to a review file (created if missing); checks that
        the lines exist and that the change would be visible. --after gives
        one line's new text; each --block is one option replacing every
        target line (--paras a-b: from a to b in script order; a,b,c: exactly
        those), --block '' or '[cut]' cuts them; with --insert-after each
        --block is the lines to add after that line. At most 3 options and
        40 lines a side. Lines that go together (a cue and its dialogue, an
        exchange) belong in one suggestion
  scriptorium progress <script.fdx> [<review>] [-o <file>]
        the review file with every decision written in (STATUS/CHOSEN/MINE),
        to hand on or continue elsewhere; stdout unless -o
  scriptorium apply <script.fdx> [<review.txt>...] [--accept-all] [-o <out.fdx>]
        write the script with the session's decisions and edits
        (--accept-all: also take option 1 of every pending suggestion).
        Default output: \"<script> (scriptorium).fdx\" next to the script

EDITING STRUCTURE
  Each is one undoable step (`undo` reverses it), saved to the session; ids
  are from `dump`. TYPE is an element name; case, spaces and dashes don't matter (scene heading,
  scene-heading and SCENEHEADING all work): Scene Heading, Action,
  Character, Parenthetical, Dialogue, Transition, Shot, General. Lines inside
  a dual dialogue block take text edits only: these are refused there.
  With --json, each prints {\"new\":[ids],\"removed\":[ids],\"paras\":[{id,kind,text}]}.

  scriptorium insert <script.fdx> (--after <id> | --start | --end)
                     (--block '<TYPE: text>\\n<TYPE: text>...' | --guess '<plain text>'
                      | <TYPE> <text>) [--json]
        add paragraphs, one `TYPE: text` per line, in one step; --guess takes
        untyped screenplay text and guesses each line's type as the app does
        when pasting plain text (blank lines dropped)
  scriptorium remove <script.fdx> <id>[..<id>] [--json]
        remove a paragraph, or from the first to the second in script order
        (not a numeric range; refused if the second is above the first, and
        for the last remaining paragraph)
  scriptorium split <script.fdx> <id> (--at <text> | --offset <n>) [--json]
        split before the first occurrence of text, or after n characters;
        the original id keeps the original text
  scriptorium join <script.fdx> <first id> <second id> [--json]
        join the next paragraph onto the first
  scriptorium retype <script.fdx> <id> <TYPE> [--json]
  `apply` writes these into the .fdx as far as the writer supports them.

VERSIONS
  A version is another .fdx lined up with a script row by row (a
  translation), through a links file beside both (<version stem>.links.txt).
  scriptorium version new <script.fdx> --lang <code> [--from <code>] [-o <name.fdx>]
        make a version lined up with the script: scene headings and character
        names copied, other lines blank, every line linked. Default name
        \"<stem>.<code>.fdx\" next to the script. --from: the script's language.
        Run again, it finds what an earlier run made. Prints the version's path.
  scriptorium versions <script.fdx> [--json]
        the version lined up with the script: both files and languages, the
        links file, rows by status, rows new in the original
  scriptorium rows <script.fdx> [--scene <n|text>] [--needs] [--json]
        the rows as \"#<row> <status>  ¶ids text | ¶ids text\" (this script
        first), with the suggestions on the row and their status, and notes.
        --scene: a scene number (1 = the first) or part of its heading;
        --needs: only rows the version needs a suggestion for (none when
        called on the original). With --json: [{row, status, label, needs,
        note?, this: [{id, kind, text}], other: [...], suggestions: [{review,
        n, status}]}]; status is in_sync, this_changed, other_changed,
        both_changed, blank or one_sided, from the script called on.
  scriptorium links <script.fdx> sync|unsync <row>
        mark row <row> in sync (its lines' texts now become the links file's
        \"=\" stamps, so it reads in sync until either side changes), or not
        in sync (the original's stamps go: it reads changed in the original
        and needs a suggestion again). Refused for a row with lines the links file doesn't list
        yet (split off or typed in Scriptorium): Write .fdx first. The app's
        Edit › Mark in sync and Mark not in sync do the same; not undoable
  scriptorium links <script.fdx> join <row> [up|down]
        join a row left with nothing on one side (lines merged in Final
        Draft or here) with the row above or below it; without up or down,
        with the one that has lines on the side the row lacks
  Which file the app lines up on the left is a view setting of the app's
  window (kept in the session), not a command.
  These all re-find first: a file changed outside Scriptorium (Final Draft saved
  it) is found again and the links file updated, with a note. `apply` on
  the version Scriptorium made needs -o (it is written from the app).

AGENT TOOLS
  scriptorium tool <script.fdx> <name> ['<json args>'] [--review-file <review.txt>]
  scriptorium tool guide '{\"topic\": \"review\"}'     (guide takes no script)
        run one of the tools the MCP server offers agents (`tool --list`;
        the built-in agent's, plus attach_reviews, decide_suggestion,
        edit_paragraph, undo, redo, write_script, guide…). Arguments are a
        JSON object, e.g. '{\"scene\": 2}'. Suggestions go to --review-file
        (created and attached if missing), else to the only attached review.
        --review-file is a path from the script's folder: an attached review,
        or else a *.review.txt inside the folder (no .., no dot-folders, no
        links out). An existing file that doesn't start with SCRIPT REVIEW
        must be attached by the writer first (`reviews add`). Suggestions are
        only written into reviews inside the script's folder. Arguments the
        tool doesn't list are refused.
        The script is the first argument, never `script` in the JSON.
        The read-only tools never write, and show the script as written if
        the saved session can't be used.
  scriptorium tool [<script.fdx>] --list [--json]
        the tools with a one-line description each; --json prints their
        full specs (name, description, JSON schema of the arguments, the
        script left out: it is the first argument)

CONNECTING AI AGENTS (the app's Scriptorium › Connect an AI agent…)
  scriptorium connect claude-desktop|claude-code|codex
        add Scriptorium's MCP server (this program, started as `<path> mcp`)
        to that agent's settings, keeping everything else; the file's last
        version is kept as <file>.scriptorium-backup (and the first ever as
        <file>.scriptorium-original). Claude Code goes through
        its own `claude mcp add --scope user` when `claude` is found. Claude
        Desktop's file isn't written while Claude runs. Then checks that the
        agent can start this program.
        --remove       take it out again
        --print        print only Scriptorium's part of the settings (for
                       claude-code, the `claude mcp add` command); writes nothing
        --restart      (claude-desktop) quit Claude, write, and reopen it
        --mcpb         (claude-desktop) make a Desktop Extension and open it
                       with Claude instead of editing its settings file
  scriptorium connect --status
        whether each agent is connected, and its settings file
  scriptorium connect --print
        the setup command for other agents
  scriptorium mcp
        Scriptorium's MCP server on stdin and stdout (what agents start):
        each tool call goes to the open app when it's running (the script
        open there when `script` is left out), otherwise to the files
  scriptorium --version

CHAT (the built-in agent; the same threads and accounts as the app's panel)
  As everywhere, the script comes first, then the subcommand.
  scriptorium chat <script.fdx> [--thread <id> | --new] [--account <id>] [--model <model>]
                   [--yes] [--allow-whole-script] [--record <cassette>] [--json] (<message> | -)
  scriptorium chat <script.fdx> --thread <id> --retry [same options]
        send a message on a new thread (the default) or thread <id>; `-`
        reads the message from stdin, to its end. --retry sends the thread
        again as it is (after an error, Ctrl+C or a round limit). --account
        and --model stick: the thread keeps them for later messages (change
        them with `threads set-model`). The reply streams to stdout; tool
        rows, notes and a closing `thread <id> (<title>) · <tokens> · <cost>`
        line go to stderr. The agent only suggests: its suggestions go to the
        thread's own review (`<title>.review.txt`, attached to the script).
        The first send about a script to a provider shows what gets sent and
        needs --yes (or \"y\" on a terminal); reading a long script whole
        needs --allow-whole-script (or \"y\"). Ctrl+C stops (the thread is
        saved at every step); twice quits at once.
        --json prints one object on stdout however the run ends:
          {outcome, thread, title, created, account, model, review_path,
           added, text, usage, usage_text, error, retry}
        outcome: finished | stopped | error | round_limit | withdrawn |
        declined | busy; error: {kind, message, retryable} or null; retry:
        the command to run to try again, or null; usage: {input_tokens,
        output_tokens, cache_read_tokens, cache_write_tokens, cost_usd,
        cost_complete}.
        Exit status: 0 finished, 1 other failure (error, round_limit,
        withdrawn), 2 wrong arguments, 3 the thread is busy in another
        program, 4 disclosure not accepted (nothing sent), 130 stopped.
        --record writes each request and reply to a cassette (no headers, so
        no keys), but cassettes DO contain snippets of the script's text, tool
        results and error bodies: keep them private like the script.
        With SCRIPTORIUM_REPLAY=<cassette> nothing is sent: replies come from
        the cassette, and exchanges left over fail the run.
  scriptorium threads <script.fdx> [--json]
        the script's threads (<script>.threads/), newest first
  scriptorium threads <script.fdx> show <id> [--json]
        the transcript, the thread's review file and its suggestion counts
  scriptorium threads <script.fdx> set-model <id> <account> <model> [--json]
  scriptorium threads <script.fdx> remove <id> [--json]
        (the review file, its decisions and the lock file stay)
  Running or changing a thread another program is running waits 2 s, then
  is refused (\"this conversation is running in another Scriptorium window
  or an AI agent\", exit status 3).

ACCOUNTS
  Accounts live in $XDG_CONFIG_HOME/scriptorium/accounts.json (macOS:
  Application Support); keys in the OS keyring, else from
  SCRIPTORIUM_<ACCOUNT>_KEY or SCRIPTORIUM_<PROVIDER>_KEY (a set variable
  wins). SCRIPTORIUM_KEYRING=off never touches the keyring (key set and key
  delete then refuse). Keys are read from stdin (one line), never from
  arguments, and never printed. An account without a model uses its
  provider's default model (custom endpoints need one).
  scriptorium accounts [list] [--json]
  scriptorium accounts add [<id>] --provider anthropic|openai|openrouter|custom
                    [--base-url <url> --protocol anthropic|openai_chat] [--name <name>]
                    [--model <model>] [--effort low|medium|high|xhigh|max]
                    [--max-tokens <n>] [--default] [--key-stdin] [--json]
  scriptorium accounts set <id> [--name <name>] [--model <model>] [--effort <e>]
                    [--max-tokens <n>] [--base-url <url>] [--json]
        change an account (--base-url: custom ones only; '' clears a setting)
  scriptorium accounts remove <id> [--json]      (also deletes its keyring entry)
  scriptorium accounts default <id> [--json]
  scriptorium accounts key set <id>              (the key on stdin)
  scriptorium accounts key delete <id>           (the account stays)
  scriptorium accounts key status <id> [--json]
  scriptorium accounts check <id> [--json]
        list the account's models: a cheap request (no tokens, no script
        text) that tells whether the key and address work
  add, set and check warn about an address that sends the key unencrypted.
";

/// Review commands, dispatched by [`run`].
const REVIEW_COMMANDS: &[&str] =
    &["review", "reviews", "suggestions", "decide", "edit", "undo", "redo", "suggest", "progress", "apply", "tool"];

/// `chat`, `threads` and `accounts` (feature `chat`).
const CHAT_COMMANDS: &[&str] = &["chat", "threads", "accounts"];

/// Exit statuses besides 0 (success). Every command fails with
/// [`EXIT_ERROR`]; `chat`, `threads` and `accounts` also use the others.
pub const EXIT_ERROR: u8 = 1;
/// Wrong arguments.
pub const EXIT_USAGE: u8 = 2;
/// The thread is running in another program.
pub const EXIT_BUSY: u8 = 3;
/// The first-use disclosure wasn't accepted: nothing was sent.
pub const EXIT_DECLINED: u8 = 4;
/// Stopped with Ctrl+C (the thread is saved).
pub const EXIT_STOPPED: u8 = 130;

/// A command that failed: the message for stderr, what to print on stdout
/// anyway (`chat --json` prints its result object whatever happened), and
/// the exit status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failed {
    pub message: String,
    pub stdout: String,
    pub code: u8,
}

impl Failed {
    pub fn new(code: u8, message: impl Into<String>) -> Self {
        Failed { message: message.into(), stdout: String::new(), code }
    }
}

impl From<String> for Failed {
    fn from(message: String) -> Self {
        Failed::new(EXIT_ERROR, message)
    }
}

/// Run the command line `args` (without the program name). Failures are
/// their message; [`run_command`] has the rest.
pub fn run(args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    run_command(args, notes).map_err(|f| f.message)
}

/// [`run`], with what a failure prints on stdout and its exit status.
pub fn run_command(args: &[&str], notes: &mut Vec<String>) -> Result<String, Failed> {
    match args {
        [cmd, rest @ ..] if CHAT_COMMANDS.contains(cmd) => chat_command(cmd, rest, notes),
        [cmd, rest @ ..] if crate::connect::COMMANDS.contains(cmd) => crate::connect::command(rest, notes),
        [cmd, rest @ ..] if crate::apps::COMMANDS.contains(cmd) => crate::apps::command(rest),
        _ => run_other(args, notes).map_err(Failed::from),
    }
}

#[cfg(feature = "chat")]
fn chat_command(cmd: &str, args: &[&str], notes: &mut Vec<String>) -> Result<String, Failed> {
    crate::chat::command(cmd, args, notes)
}

#[cfg(not(feature = "chat"))]
fn chat_command(cmd: &str, _args: &[&str], _notes: &mut Vec<String>) -> Result<String, Failed> {
    Err(Failed::new(EXIT_USAGE, format!("`{cmd}` isn't in this build (built without the `chat` feature)")))
}

fn run_other(args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    match args {
        ["info", path] => info(path, notes),
        ["dump", path] => dump(path, false, notes),
        ["dump", path, "--json"] => dump(path, true, notes),
        ["roundtrip", path] => load(path).and_then(|f| roundtrip(&f)),
        ["version", "new", rest @ ..] => crate::version_new::command(rest, notes),
        [cmd, rest @ ..] if crate::links::COMMANDS.contains(cmd) => crate::links::command(cmd, rest, notes),
        [cmd, rest @ ..] if crate::rows::COMMANDS.contains(cmd) => crate::rows::command(cmd, rest, notes),
        [cmd, rest @ ..] if crate::structure::COMMANDS.contains(cmd) => crate::structure::command(cmd, rest, notes),
        [cmd, rest @ ..] if REVIEW_COMMANDS.contains(cmd) => review_command(cmd, rest, notes),
        ["-h" | "--help" | "help"] => Ok(USAGE.to_string()),
        ["--version" | "-V"] => Ok(format!("scriptorium {}\n", env!("CARGO_PKG_VERSION"))),
        _ => Err(format!("unrecognized arguments\n\n{USAGE}")),
    }
}

fn review_command(cmd: &str, args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    match (cmd, args) {
        ("review", [script, reviews @ ..]) => {
            let o = Opened::open(script, reviews, notes)?;
            if o.session.reviews.is_empty() {
                return Err(no_reviews(script));
            }
            Ok(summary(&o))
        }
        ("reviews", [script]) => list_reviews(script, notes),
        ("reviews", [script, "add", files @ ..]) if !files.is_empty() => add_reviews(script, files, notes),
        ("reviews", [script, "remove", files @ ..]) if !files.is_empty() => remove_reviews(script, files),
        ("suggestions", [script, rest @ ..]) => suggestions(script, rest, notes),
        ("decide", [script, rest @ ..]) => decide_command(script, rest, notes),
        ("edit", [script, id, text, rest @ ..]) => {
            let para = id.parse::<u64>().map_err(|_| format!("paragraph id must be a number, got {id:?}"))?;
            edit(script, para, text, &review_flags(rest)?, notes)
        }
        ("undo" | "redo", [script, rest @ ..]) => undo_redo(script, cmd == "redo", &review_flags(rest)?, notes),
        ("suggest", [review, script, rest @ ..]) => suggest(review, script, rest, notes),
        ("progress", [script, rest @ ..]) => progress(script, rest, notes),
        ("apply", [script, rest @ ..]) => apply_command(script, rest, notes),
        ("tool", rest) => tool_command(rest, notes),
        _ => Err(format!("wrong arguments for `{cmd}`\n\n{USAGE}")),
    }
}

fn info(path: &str, notes: &mut Vec<String>) -> Result<String, String> {
    let o = Opened::open_read_only(path, notes)?;
    Ok(info_text(o.doc.script()))
}

fn info_text(script: &Script) -> String {
    let ctx = annotate(script);
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut speakers: BTreeMap<String, usize> = BTreeMap::new();
    for (p, c) in script.paragraphs.iter().zip(&ctx) {
        *kinds.entry(p.kind.name().to_string()).or_default() += 1;
        if !c.speaker.is_empty() && p.kind.name() == "Dialogue" {
            *speakers.entry(c.speaker.clone()).or_default() += 1;
        }
    }
    let words: usize = script.paragraphs.iter().map(|p| p.text().split_whitespace().count()).sum();
    let mut out = String::new();
    let _ = writeln!(out, "paragraphs  {}", script.paragraphs.len());
    let _ = writeln!(out, "scenes      {}", ctx.iter().filter(|c| c.heading).count());
    let _ = writeln!(out, "words       {words}");
    let _ = writeln!(out, "\nelements");
    for (k, n) in &kinds {
        let _ = writeln!(out, "  {n:>5}  {k}");
    }
    let mut top: Vec<_> = speakers.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let _ = writeln!(out, "\ncharacters (dialogue paragraphs)");
    for (name, n) in top.iter().take(15) {
        let _ = writeln!(out, "  {n:>5}  {name}");
    }
    out
}

#[derive(Serialize)]
struct Row<'a> {
    id: u64,
    kind: &'a str,
    scene: &'a str,
    speaker: &'a str,
    text: String,
}

fn dump(path: &str, json: bool, notes: &mut Vec<String>) -> Result<String, String> {
    // JSON is read by programs (MCP parses it along with stderr): no notes.
    let mut dropped = vec![];
    let o = Opened::open_read_only(path, if json { &mut dropped } else { notes })?;
    let script = o.doc.script();
    let ctx = annotate(script);
    let rows: Vec<Row> = script
        .paragraphs
        .iter()
        .zip(&ctx)
        .map(|(p, c)| Row { id: p.id.0, kind: p.kind.name(), scene: &c.scene, speaker: &c.speaker, text: p.text() })
        .collect();
    if json {
        return Ok(serde_json::to_string_pretty(&rows).expect("rows serialize") + "\n");
    }
    let mut out = String::new();
    for r in rows {
        let _ = writeln!(out, "{:>5}  {:<14} {}", r.id, r.kind, r.text);
    }
    Ok(out)
}

fn roundtrip(f: &FdxFile) -> Result<String, String> {
    let out = f.write_script(&f.to_script()).map_err(|e| e.to_string())?;
    if out == f.raw() {
        Ok(format!("ok: no-op save is byte-identical ({} bytes, {} paragraphs)\n", out.len(), f.paragraphs().len()))
    } else {
        Err("no-op save changed the file".into())
    }
}

/// `-r <review.txt>` pairs.
fn review_flags<'a>(rest: &[&'a str]) -> Result<Vec<&'a str>, String> {
    let mut out = vec![];
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match *a {
            "-r" => out.push(*it.next().ok_or("-r needs a review file")?),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    Ok(out)
}

/// Takes `--review <name or file>` out of `args`.
fn take_review_flag<'a>(args: &[&'a str]) -> Result<(Option<&'a str>, Vec<&'a str>), String> {
    let (mut pick, mut rest) = (None, vec![]);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--review" => pick = Some(*it.next().ok_or("--review needs a review name or file")?),
            other => rest.push(other),
        }
    }
    Ok((pick, rest))
}

fn list_reviews(script: &str, notes: &mut Vec<String>) -> Result<String, String> {
    let o = Opened::open(script, &[], notes)?;
    let mut out = String::new();
    if o.session.reviews.is_empty() {
        let _ = writeln!(out, "no review files attached (add one: scriptorium reviews {script:?} add <review.txt>)");
    }
    for (r, review) in o.session.reviews.iter().enumerate() {
        let p = o.session.progress(&o.doc, r);
        let _ = writeln!(
            out,
            "{}  {} of {} decided ({} accepted, {} kept, {} rewritten, {} pending)",
            review.path,
            p.done(),
            p.total,
            p.accepted,
            p.rejected,
            p.rewritten,
            p.pending
        );
        if p.removed > 0 {
            let _ = writeln!(out, "  {} on lines removed since (can't be decided)", p.removed);
        }
    }
    Ok(out)
}

/// Attach review files (paths from the current folder) to `script`, saving
/// the session; the result lists each with its suggestion count.
pub fn add_reviews(script: &str, files: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let _lock = lock(script)?;
    let mut o = Opened::open(script, files, notes)?;
    ops::attach(&mut o, script)
}

/// Detach review files (as attached, or paths from the current folder).
pub fn remove_reviews(script: &str, files: &[&str]) -> Result<String, String> {
    let _lock = lock(script)?;
    ops::detach(script, files)
}

fn summary(o: &Opened) -> String {
    let mut out = String::new();
    for (r, review) in o.session.reviews.iter().enumerate() {
        let p = o.session.progress(&o.doc, r);
        let fuzzy = review.items.iter().filter(|i| i.fuzzy).count();
        let _ = writeln!(out, "{}", review.name);
        let _ = writeln!(out, "  suggestions   {} ({fuzzy} matched approximately)", p.total);
        let _ = writeln!(
            out,
            "  decided       {} accepted, {} kept, {} rewritten, {} pending",
            p.accepted, p.rejected, p.rewritten, p.pending
        );
        if p.removed > 0 {
            let _ = writeln!(out, "  line removed  {} (their lines were removed since; can't be decided)", p.removed);
        }
        if !review.invisible.is_empty() {
            let _ = writeln!(
                out,
                "  skipped       {} that change nothing visible (e.g. only letter case in a character name)",
                review.invisible.len()
            );
        }
        if !review.unmatched.is_empty() {
            let _ = writeln!(out, "  not found     {}", review.unmatched.len());
            for u in review.unmatched.iter().take(10) {
                let _ = writeln!(out, "    block {}: {}", u.entry + 1, u.before);
            }
        }
        for w in &review.warnings {
            let _ = writeln!(out, "  warning: {w}");
        }
    }
    out
}

/// `TYPE: text` for each `{kind, text}` line of `lines`, indented by `pad`.
fn typed_text(out: &mut String, lines: &Value, pad: &str) {
    for l in lines.as_array().into_iter().flatten() {
        let kind = l["kind"].as_str().unwrap_or_default().to_uppercase();
        let _ = writeln!(out, "{pad}{kind}: {}", l["text"].as_str().unwrap_or_default());
    }
}

fn suggestions(script: &str, args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let (pick, rest) = take_review_flag(args)?;
    let (mut pending_only, mut json, mut file) = (false, false, None);
    for f in rest {
        match f {
            "--pending" => pending_only = true,
            "--json" => json = true,
            other if other.starts_with('-') || file.is_some() => {
                return Err(format!("unexpected argument {other:?}"));
            }
            review => file = Some(review),
        }
    }
    let (o, r) = Opened::open_one(script, file, pick, true, notes)?;
    let rows = suggestion_rows(&o.doc, &o.session, r, pending_only);
    if json {
        return Ok(serde_json::to_string_pretty(&rows).expect("rows serialize") + "\n");
    }
    let mut out = String::new();
    for r in rows {
        // A removed line says so in its status; any other structural change
        // is named in the "line changed" flag.
        let changed = match r.line {
            Some(l) if r.status != l => format!(" (line changed since: {l})"),
            Some(_) => String::new(),
            None if r.outdated => " (line changed since)".into(),
            None => String::new(),
        };
        let flags = changed + if r.fuzzy { " (matched approximately)" } else { "" };
        let block = r.shape != "line";
        let place = match r.paragraph_ids.as_slice() {
            [a, .., b] if block => format!("¶{a}–¶{b}"),
            _ => format!("¶{}", r.paragraph),
        };
        let all_cuts = r
            .options
            .as_array()
            .is_some_and(|o| !o.is_empty() && o.iter().all(|o| o.as_array().is_some_and(Vec::is_empty)));
        let shape = match (block, all_cuts) {
            (false, _) => String::new(),
            (true, true) => " cut".to_string(),
            (true, false) => format!(" {}", r.shape),
        };
        let _ = writeln!(out, "#{} {place}{shape} {}{flags}", r.n, r.status);
        if block {
            if matches!(r.line, Some("line removed" | "line cut")) {
                let _ = writeln!(out, "  was:");
                typed_text(&mut out, &r.before, "    ");
            } else {
                let _ = writeln!(out, "  now:");
                typed_text(&mut out, &r.current, "    ");
            }
            for (k, opt) in r.options.as_array().into_iter().flatten().enumerate() {
                let _ = writeln!(out, "  {}:", k + 1);
                if opt.as_array().is_some_and(Vec::is_empty) {
                    let _ = writeln!(out, "    [cut]");
                }
                typed_text(&mut out, opt, "    ");
            }
        } else {
            let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
            if matches!(r.line, Some("line removed" | "line cut")) {
                let _ = writeln!(out, "  was: {}", text(&r.before));
            } else {
                let _ = writeln!(out, "  now: {}", text(&r.current));
            }
            for (k, opt) in r.options.as_array().into_iter().flatten().enumerate() {
                let _ = writeln!(out, "  {}: {}", k + 1, text(opt));
            }
        }
        if !r.why.is_empty() {
            let _ = writeln!(out, "  why: {}", r.why);
        }
    }
    Ok(out)
}

/// `decide <script> [<review>] [--review <r>] <n> <decision...>`.
fn decide_command(script: &str, args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let (pick, rest) = take_review_flag(args)?;
    // `<review.txt> <n> ...` or just `<n> ...`.
    let (file, n, decision) = match rest.as_slice() {
        [n, decision @ ..] if SuggestionRef::parse(n).is_some() => (None, *n, decision),
        [file, n, decision @ ..] => (Some(*file), *n, decision),
        _ => return Err(format!("decide which suggestion?\n\n{USAGE}")),
    };
    let n = SuggestionRef::parse(n).ok_or(format!("no suggestion {n}: give its number, or e<entry>"))?;
    let choice = match decision {
        ["accept"] => Choice::Accept(None),
        ["accept", k] => Choice::Accept(Some(k.parse::<usize>().map_err(|_| format!("no option {k}"))?)),
        ["keep"] => Choice::Keep,
        ["undecide"] => Choice::Undecide,
        ["rewrite", text] => Choice::Rewrite(text.to_string()),
        ["--rewrite-block" | "rewrite-block", block] => Choice::RewriteBlock(block_lines(block)?),
        _ => {
            return Err(format!(
                "decide what? accept [<option>] | keep | undecide | rewrite <text> | --rewrite-block <lines>\n\n{USAGE}"
            ));
        }
    };
    decide_in(script, file, pick, true, n, choice, notes)
}

/// Decide one suggestion of `review` (an attached review's name or file, or
/// a review file, absolute or from the script's folder; never from the
/// current folder; `None`: the only attached review), and save. Returns the suggestion's new status and line.
pub fn decide(
    script: &str,
    review: Option<&str>,
    n: SuggestionRef,
    choice: Choice,
    notes: &mut Vec<String>,
) -> Result<String, String> {
    decide_in(script, None, review, false, n, choice, notes)
}

fn decide_in(
    script: &str,
    file: Option<&str>,
    pick: Option<&str>,
    from_cwd: bool,
    n: SuggestionRef,
    choice: Choice,
    notes: &mut Vec<String>,
) -> Result<String, String> {
    let _lock = lock(script)?;
    let (mut o, r) = Opened::open_one(script, file, pick, from_cwd, notes)?;
    ops::decide(&mut o, r, n, choice)
}

/// Replace paragraph `para`'s whole text, and save. `reviews` (paths from the
/// current folder) are attached too.
pub fn edit(script: &str, para: u64, text: &str, reviews: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let _lock = lock(script)?;
    let mut o = Opened::open(script, reviews, notes)?;
    ops::edit(&mut o, para, text)
}

/// Undo (or redo) the last change to the script, and save. `reviews` (paths
/// from the current folder) are attached too.
pub fn undo_redo(script: &str, redo: bool, reviews: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let _lock = lock(script)?;
    let mut o = Opened::open(script, reviews, notes)?;
    ops::undo_redo(&mut o, redo)
}

/// `--block` text as lines: one `TYPE: text` per line (any case); `''` or
/// `[cut]` is no lines (a cut).
pub fn block_lines(block: &str) -> Result<Vec<Line>, String> {
    if block.trim().is_empty() {
        return Ok(vec![]);
    }
    let lines = parse_typed_lines(block, Case::Any)?;
    Ok(lines.into_iter().map(|(k, t)| Line::typed(k, t)).collect())
}

/// `--paras`: `a-b` or `a..b` (from the first to the last, in script
/// order), or `a,b,c` (exactly these).
fn paras_spot(spec: &str) -> Result<Spot, String> {
    let num = |s: &str| s.trim().parse::<u64>().map_err(|_| format!("--paras: {s:?} isn't a paragraph id"));
    if let Some((a, b)) = spec.split_once("..").or_else(|| spec.split_once('-')) {
        return Ok(Spot::Range(num(a)?, num(b)?));
    }
    let ids: Vec<u64> = spec.split(',').map(num).collect::<Result<_, _>>()?;
    Ok(Spot::List(ids))
}

fn suggest(review: &str, script: &str, rest: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let (mut para, mut before, mut after, mut why) = (None, None, vec![], String::new());
    let (mut paras, mut insert_after, mut blocks) = (None, None, vec![]);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().map(|v| v.to_string()).ok_or(format!("{a} needs a value"));
        match *a {
            "--para" => para = Some(val()?),
            "--before" => before = Some(val()?),
            "--paras" => paras = Some(val()?),
            "--insert-after" => insert_after = Some(val()?),
            "--after" => after.push(val()?),
            "--block" => blocks.push(val()?),
            "--why" => why = val()?,
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    let targets = [para.is_some(), before.is_some(), paras.is_some(), insert_after.is_some()];
    if targets.iter().filter(|t| **t).count() != 1 {
        return Err("give exactly one of --para, --before, --paras or --insert-after".into());
    }
    let block = !blocks.is_empty() || paras.is_some() || insert_after.is_some();
    if block && !after.is_empty() {
        return Err("--after is for one line (--para or --before); give the lines with --block".into());
    }
    if block && before.is_some() {
        return Err("--before names one line for --after; for --block give --para, --paras or --insert-after".into());
    }
    if block && blocks.is_empty() {
        return Err("give at least one --block (each one option; '' or '[cut]' cuts the lines)".into());
    }
    if !block && after.is_empty() {
        return Err("give at least one --after".into());
    }
    let options: Vec<Vec<Line>> = blocks.iter().map(|b| block_lines(b)).collect::<Result<_, _>>()?;
    let id = |s: &str, flag: &str| s.parse::<u64>().map_err(|_| format!("{flag} must be a number, got {s:?}"));
    // Matched and numbered against the live document (the saved session), as
    // every other command sees the script; the session is only saved if the
    // review file is attached (to record its new text).
    let _lock = lock(script)?;
    let mut o = Opened::open(script, &[], notes)?;
    let fdx = &o.fdx;
    let caps = |k: &ElementKind| fdx.is_caps(k.name());
    let entry = if block {
        let spot = match (&para, &paras, &insert_after) {
            (Some(p), _, _) => Spot::Para(id(p, "--para")?),
            (_, Some(spec), _) => paras_spot(spec)?,
            (_, _, Some(a)) => Spot::InsertAfter(id(a, "--insert-after")?),
            _ => unreachable!("one target checked above"),
        };
        block_entry(&o.doc, &caps, &spot, options, &why)?
    } else {
        let p = match (para, before) {
            (Some(p), None) => {
                let id = ParaId(id(&p, "--para")?);
                o.doc.paragraph(id).ok_or(format!("no paragraph {} (see `scriptorium dump`)", id.0))?.clone()
            }
            (None, Some(b)) => {
                let texts: Vec<String> = o.doc.paragraphs().iter().map(|p| p.text()).collect();
                let paras: Vec<Para> = o
                    .doc
                    .paragraphs()
                    .iter()
                    .zip(&texts)
                    .map(|(p, t)| Para { id: p.id, text: t, editable: p.kind.is_editable(), caps: caps(&p.kind) })
                    .collect();
                let probe = Entry::single(&b, &after);
                match match_entries(&paras, &[probe]).pop().flatten() {
                    Some(m) => o.doc.paragraphs()[m.index].clone(),
                    None => return Err(format!("no line in the script matches {b:?}")),
                }
            }
            _ => unreachable!("one target checked above"),
        };
        let text = p.text();
        if !p.kind.is_editable() {
            return Err(format!("paragraph {} ({}) isn't reviewable", p.id.0, p.kind.name()));
        }
        if after.iter().all(|a| looks_same(a, &text, caps(&p.kind))) {
            return Err("that change wouldn't be visible in Final Draft".into());
        }
        Entry { para: vec![p.id], why, ..Entry::single(text.trim(), &after) }
    };
    let first = entry.para[0].0;
    let ids: Vec<u64> = entry.para.iter().map(|p| p.0).collect();
    let path = Path::new(review);
    let stored = path.canonicalize().ok().map(|at| sessionfile::stored_path(&o.dir, &at));
    let attached = stored.and_then(|st| o.session.reviews.iter().position(|r| r.path == st));
    // Places `ROW:` suggestions (rows of a pair of versions), from the pair
    // loaded when the script was opened.
    let rows = o.row_resolver();
    // The attached review, or the file opened on its own (not attached here).
    let (mut lone, r) = match attached {
        Some(r) => (None, r),
        None => {
            let t = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(_) => {
                    let header = format!("{FORMAT_HEADER}\nScript: {}\n\n", file_name(Path::new(script)));
                    let id = new_review_id(&append_block(&header, &entry, ""), &review_salt(path));
                    insert_id(&header, &id)
                }
            };
            let original = o.fdx.to_script();
            let review = Review::open_live(review, &t, &original, &o.doc, caps, "", rows.as_deref());
            (Some(Session::new(vec![review])), 0)
        }
    };
    let session = lone.as_mut().unwrap_or(&mut o.session);
    // Spliced onto the file (never reserialized), on exactly these paragraphs.
    let many = session.append_entries(&o.doc, r, vec![entry], true, caps, rows.as_deref())?;
    let result = many.results[0].clone();
    let Some(i) = result.item else {
        return Err(match result.unplaced {
            Some(Unplaced::Taken { .. }) => {
                format!("not added: another suggestion in {review} already covers ¶{first}")
            }
            Some(Unplaced::Invisible) => "that change wouldn't be visible in Final Draft".into(),
            Some(Unplaced::Invalid(why)) => format!("not added: {why}"),
            Some(Unplaced::Mismatch) if block => {
                format!("not added: ¶{first}… changed meanwhile; read them again (`scriptorium dump`)")
            }
            _ => format!("not added: ¶{first} can't take a suggestion"),
        });
    };
    // Numbered as `suggestions` and `decide` will, by position in the script.
    let n = item_numbers(&o.doc, &session.reviews[r].items)[i];
    let item = &session.reviews[r].items[i];
    let place = match (block, item.shape(0), ids.as_slice()) {
        (false, ..) => format!("¶{first}"),
        (true, Shape::Add { after, .. }, _) => format!("e{}, after ¶{}", item.entry, after.0),
        (true, _, [one]) => format!("e{}, ¶{one}", item.entry),
        (true, _, [a, .., b]) => format!("e{}, ¶{a}–¶{b}", item.entry),
        (true, _, []) => format!("e{}", item.entry),
    };
    let is_lone = lone.is_some();
    o.write_review(path, &many.file_text).map_err(|e| format!("{review}: {e}"))?;
    if !is_lone {
        o.save()?;
    }
    Ok(format!("added suggestion #{n} ({place}) to {review}\n"))
}

fn progress(script: &str, args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let (pick, rest) = take_review_flag(args)?;
    let (file, rest) = match rest.as_slice() {
        [f, rest @ ..] if !f.starts_with('-') => (Some(*f), rest.to_vec()),
        rest => (None, rest.to_vec()),
    };
    // Only `-o` can write the review file and the session.
    let _lock = if rest.is_empty() { None } else { Some(lock(script)?) };
    let (mut o, r) = Opened::open_one(script, file, pick, true, notes)?;
    let out = o.session.save_progress(&o.doc, r);
    match rest.as_slice() {
        [] => Ok(out),
        ["-o", file] => {
            let own = o.review_path(r);
            let over_itself = scriptorium_fsio::same_file(&own, Path::new(file));
            if over_itself {
                // Through the session, which records the file's new text, so
                // its STATUS lines aren't taken for a change made elsewhere.
                // Checked before writing, so a refusal leaves the file alone.
                let mut review = o.session.reviews[r].clone();
                review.wrote_progress(out.clone())?;
                o.write_review(Path::new(file), &out)?;
                o.session.reviews[r] = review;
                o.save()?;
            } else {
                write_atomic(Path::new(file), out.as_bytes()).map_err(|e| format!("{file}: {e}"))?;
            }
            Ok(format!("wrote {file}\n"))
        }
        _ => Err(format!("unexpected arguments {rest:?}")),
    }
}

fn apply_command(script: &str, rest: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let (mut reviews, mut accept_all, mut out) = (vec![], false, None);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match *a {
            "--accept-all" => accept_all = true,
            "-o" => out = Some(Path::new(*it.next().ok_or("-o needs a file name")?)),
            r => reviews.push(r),
        }
    }
    apply(script, &reviews, accept_all, out, notes)
}

/// Write the script with the session's decisions and edits to `output`
/// (default "<script> (scriptorium).fdx" next to it, replaced if it exists);
/// never over the script itself. `accept_all` also takes option 1 of every
/// pending suggestion, in this output only. `reviews` (paths from the
/// current folder) are attached first.
pub fn apply(
    script: &str,
    reviews: &[&str],
    accept_all: bool,
    output: Option<&Path>,
    notes: &mut Vec<String>,
) -> Result<String, String> {
    let _lock = lock(script)?;
    let mut o = Opened::open(script, reviews, notes)?;
    ops::write(&mut o, script, accept_all, output)
}

/// The session lock of `script`, for a command that writes (see the module
/// docs); a script that isn't there fails as opening it would.
/// The script's session lock; None (with a note on stderr) when the file
/// system has no locks, as the app goes on. Busy past the wait is an error.
pub(crate) fn lock(script: &str) -> Result<Option<SessionLock>, String> {
    // A regular .fdx first: never a lock beside a dotfile or a folder.
    let at = scriptorium_fsio::check_script(Path::new(script))?;
    match SessionLock::for_script(&at) {
        Ok(l) => Ok(Some(l)),
        Err(e @ LockError::NoLocks) => {
            eprintln!("note: {e}");
            Ok(None)
        }
        Err(e) => Err(e.to_string()),
    }
}

/// `tool --list [--json]`: each tool's name and the first line of its
/// description, or the full specs as JSON: the tools outside agents are
/// offered ([`scriptorium_tools::Offer::Agent`]), as MCP lists them.
fn tool_list(json: bool) -> String {
    let mut specs = scriptorium_tools::catalogue(scriptorium_tools::Offer::Agent);
    // The script is the command's first argument, not a tool argument.
    for spec in &mut specs {
        if let Some(props) = spec.schema.get_mut("properties").and_then(Value::as_object_mut) {
            props.remove("script");
        }
        if let Some(required) = spec.schema.get_mut("required").and_then(Value::as_array_mut) {
            required.retain(|r| r != "script");
        }
    }
    if json {
        return serde_json::to_string_pretty(&specs).expect("specs serialize") + "\n";
    }
    let width = specs.iter().map(|s| s.name.len()).max().unwrap_or(0);
    let mut out = String::new();
    for s in &specs {
        let line = s.description.lines().next().unwrap_or("");
        let first = match line.find(". ") {
            Some(end) => &line[..=end],
            None => line,
        };
        let _ = writeln!(out, "{:<width$}  {}", s.name, first.trim());
    }
    out
}

/// `scriptorium tool ...`: parses the command line and runs [`tool`], or
/// lists the tools.
fn tool_command(args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    if args.contains(&"--list") {
        // The script, if given, isn't needed: every script has the same tools.
        let json = args.contains(&"--json");
        let rest: Vec<&str> = args.iter().copied().filter(|a| !matches!(*a, "--list" | "--json")).collect();
        if rest.len() > 1 || rest.first().is_some_and(|a| a.starts_with('-')) {
            return Err(format!("unexpected arguments with --list: {rest:?}"));
        }
        return Ok(tool_list(json));
    }
    // `guide` and `where_is_writer` need no script: `tool guide '{"topic": "review"}'`.
    let (script, rest) = match args {
        ["guide" | "where_is_writer", ..] => (None, args),
        [script, rest @ ..] => (Some(*script), rest),
        [] => return Err(format!("wrong arguments for `tool`\n\n{USAGE}")),
    };
    let (mut name, mut json, mut review_file) = (None, None, None);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match *a {
            "--review-file" => review_file = Some(*it.next().ok_or("--review-file needs a file")?),
            other if name.is_none() => name = Some(other),
            other if json.is_none() => json = Some(other),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    let name = name.ok_or(format!("which tool?\n\n{USAGE}"))?;
    let args: Value = match json {
        Some(j) => serde_json::from_str(j).map_err(|e| format!("the tool arguments aren't JSON: {e}"))?,
        None => Value::Object(Default::default()),
    };
    tool(script, name, args, review_file, notes)
}

/// Runs one tool on `script` as an outside agent would
/// ([`scriptorium_chat::exec::run_call`] with [`Caller::Agent`] over the
/// script's files, waiting for the session lock up to
/// [`scriptorium_fsio::LOCK_WAIT`]): the executor MCP and the app use.
/// `args` is the tool's JSON arguments, without `script` (it is the command
/// line's); `review_file` (the `--review-file` flag, or in `args`) is where
/// suggestions go, relative to the script's folder. Notes from opening the
/// script go to `notes`. Returns the tool result, ending in a newline.
fn tool(
    script: Option<&str>,
    name: &str,
    mut args: Value,
    review_file: Option<&str>,
    notes: &mut Vec<String>,
) -> Result<String, String> {
    let map = args.as_object_mut().ok_or("the tool arguments must be a JSON object")?;
    if map.contains_key("script") {
        return Err("give the script as the first argument, not as `script` in the tool arguments".into());
    }
    if let Some(f) = review_file {
        map.insert("review_file".into(), Value::String(f.into()));
    }
    // `guide` and `where_is_writer` take no script, even when one is given.
    let Some(script) = script.filter(|_| !matches!(name, "guide" | "where_is_writer")) else {
        let call = ToolCall {
            id: "cli".into(),
            name: name.into(),
            input: args,
            input_error: None,
            raw_input: None,
            extra: None,
        };
        let done = run_call(&mut NoScript, Caller::Agent, &call);
        return if done.result.is_error { Err(done.result.content) } else { Ok(done.result.content + "\n") };
    };
    map.insert("script".into(), Value::String(script.into()));
    let mut files = FileScript::new(Path::new(script))?.with_lock_wait(scriptorium_fsio::LOCK_WAIT);
    let call =
        ToolCall { id: "cli".into(), name: name.into(), input: args, input_error: None, raw_input: None, extra: None };
    let done = run_call(&mut files, Caller::Agent, &call);
    for n in files.take_notes() {
        // A note the error already says (a links file that can't be read)
        // isn't said again.
        let said = done.result.is_error && !n.is_empty() && done.result.content.contains(n.as_str());
        if !said && !notes.contains(&n) {
            notes.push(n);
        }
    }
    if done.result.is_error { Err(done.result.content) } else { Ok(done.result.content + "\n") }
}

/// The script of `tool guide`: never opened (`guide` reads none).
struct NoScript;

impl scriptorium_chat::exec::Script for NoScript {
    fn open(
        &mut self,
        _writes: bool,
    ) -> Result<Box<dyn scriptorium_chat::exec::ReviewStore + '_>, scriptorium_chat::exec::ReviewError> {
        Err(scriptorium_chat::exec::ReviewError::Failed("no script given".into()))
    }
}
