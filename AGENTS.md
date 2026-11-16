# Working on Scriptorium

Guidance for coding agents (and people) changing this codebase. Read this before
editing. `CLAUDE.md` points here.

## What this is

An open-source screenplay tool: reviewing proposed changes (from an AI or a
collaborator), managing translations, and later basic writing. A Rust core with
a thin native UI on top built with GPUI (Zed's UI framework).

**Agents are users too.** Anything a person can do in the app, a coding agent
can do from the `scriptorium` CLI: create suggestions, list them, accept, keep,
rewrite, edit a line, undo, write the script. The CLI and the app share one
saved session, so they see each other's work. Design every feature for both
from the start, not as a CLI added afterwards.

## Layout

```
crates/
  core/   scriptorium-core   document model, outline (scenes/speakers), text, diff
  fdx/    scriptorium-fdx    Final Draft import + byte-preserving export
  review/ scriptorium-review review files, matching suggestions to lines, decisions, saved sessions
  agent/  scriptorium-agent  the built-in agent minus I/O: threads, wire protocols, reply parser, turn driver, thread files (frozen)
  tools/  scriptorium-tools  the screenplay tools agents call (catalogue, call, apply) and the system prompt
  providers/ scriptorium-providers  AI accounts, model and price tables, request building; shell: keyring keys, accounts file, HTTP (features `shell`, `keyring`)
  chat/   scriptorium-chat   chat threads and the host run loop shared by the CLI and the panel: `<script>.threads/`, locks, shared apply path (feature `shell`; core builds without it)
  fsio/   scriptorium-fsio   atomic writes, the session lock, `opened` (feature): the script opened as the CLI and chat host open it
  cli/    scriptorium-cli    `scriptorium` binary: headless access to everything, including `chat`, `threads`, `accounts` (feature `chat`, default)
  mcp/    scriptorium-mcp    MCP server for the Claude plugin; depends on cli with default-features = false (no providers, keyring or HTTP)
  app/    scriptorium-app    GPUI desktop app: script view, reviews, editing inside paragraphs, the agent panel (Ctrl/Cmd+L) and the accounts sheet
```

`core` modules: `model` (Script/Paragraph/Span), `doc` (editable Document:
commands, invertible ops, event log, undo/redo, paragraph-id rules), `spans`,
`rules` (Enter/Tab element flow), `outline`, `text`, `diff`.

Direction and open questions live in `docs/PLAN.md`. Read it before starting
new feature work: the roadmap is still being planned.

GPUI comes from crates.io as `gpui-pre`: weekly unpatched snapshots of Zed's
GPUI, published for gpui-kit, pinned exactly. Upgrade it when gpui-kit
(longbridge/gpui-kit) upgrades, in a commit of its own; when Zed publishes
official weekly GPUI releases, revisit. Zed's `gpui*` crates are Apache-2.0 and
may be copied with attribution; Zed's `editor`, `ui`, `text`, `rope` crates are
GPL-3.0 — read them for ideas, never copy code from them.

## Rules

1. **Functional core, imperative shell.** Logic is pure: no file or network
   I/O, no threads, no clocks, no UI; it takes and returns values (strings,
   structs). I/O lives in a thin shell around it. The split is by module, not
   by crate: a crate may contain its own shell (e.g. `providers` keeps keys,
   settings and HTTP in `shell` modules over a pure core) as long as the core
   modules never call into the shell and can be tested without it. Crates that
   are pure throughout (`core`, `fdx`, `review`, `agent`, `tools`) keep
   compiling to `wasm32-unknown-unknown`; a crate with a shell puts it behind
   a default cargo feature so its core still does. (Clarified by the user,
   2026-11-14.)
2. **Never reserialize a user's file.** Exporters splice changes into the
   original bytes. A save with no edits must be byte-identical; every format
   crate has a test for that.
3. **Paragraphs are referred to by `ParaId`,** not by index or text, in anything
   that outlives one editing session (review proposals, translations).
4. **Each feature is its own crate** with a small public API and its own tests.
   Depend on `scriptorium-core`, not on sibling feature crates, unless there's a
   real reason.
5. **Everything is reachable from the CLI.** If you add a capability, add a
   subcommand so agents can use it and it can be tested without the GUI. The
   app and the CLI go through the same library calls and the same session file
   (`<script>.scriptorium.json`, which also lists the review files attached to
   the script); never give the app a code path the CLI lacks. Anything that
   writes the session or a review file holds `scriptorium_fsio::SessionLock`
   (`<script>.scriptorium.lock`) from reading the files to writing them.
6. **A model never picks an arbitrary file.** Tool calls are checked against
   the tools offered (`scriptorium_tools::check_args`): an argument that
   wasn't offered is refused. A review file a model names (the CLI `tool`'s
   and MCP's `review_file`; the built-in agent isn't offered one) is either
   a review already attached, wherever it is, or a `*.review.txt` inside the
   script's folder (`scriptorium_fsio::review_in_folder`: no `..`, no links
   out) that, if it exists, starts with `SCRIPT REVIEW`. Only the writer's
   own commands (File › Add review, `reviews add`, MCP `attach_reviews`)
   attach files from anywhere.

## How agent sessions work here (required)

The user asked for this on 2026-11-14; it applies to every coding-agent
session on this repo.

- **The main session orchestrates; subagents build.** All building (writing
  code, tests, fixtures, docs for a feature) is done by subagents, so the main
  session's context stays clean for decisions and integration.
- **Many small agents, not one big one.** Give each builder one narrow,
  well-bounded piece (one crate, one module, one test suite) with a written
  brief: the goal, the files it may touch, the decisions already made, and the
  checks it must pass. Run independent pieces in parallel.
- **Every piece of work is reviewed by several agents.** Before work is
  accepted, at least two reviewer agents that did not build it read it
  independently, in fresh contexts: one for correctness (bugs, edge cases,
  tests that would miss them), one for design (does it fit this file's rules,
  the plan in `docs/`, and the decisions already made; is there a simpler
  shape). Give reviewers the code and the brief, not the builder's
  conclusions. Design decisions get the same treatment before they are built:
  have reviewers critique the plan, not just the code.
- **Pick the model for the job.** Mechanical work (bulk renames, API-migration
  fixes, following a precise fix list, boilerplate, test scaffolding) goes to
  a cheaper, faster model such as Sonnet. Design, tricky correctness work and
  reviews use the strongest model. (Asked by the user, 2026-11-14.)
- **The orchestrator decides.** It weighs the reviews, sends fixes back to a
  builder (or a new one), and only then integrates, runs the checks below
  and commits. Disagreements that change the product go to the user.
- Concurrent sessions: tell other sessions working in the repo which paths
  you're touching, and commit with pathspecs (`git commit -- <paths>`).

## Checks (run before you finish)

```
cargo fmt --all
cargo clippy --all-targets   # must be warning-free
cargo test
```

Tests that use the real script (`../2026_11_05_PURPLE LIST.fdx`, kept out of the
repo) skip themselves when it's missing. Point `SCRIPTORIUM_REAL_FDX` at another
`.fdx` to use that instead. Never commit scripts.

## Trying the app without taking over the user's screen

Never launch the app on the user's own display to test it: it pops up over
their work and steals focus, and keys you send go to whatever they're using.
Run a private headless Sway instead and drive that:

```
# once: a compositor with no visible output
env -u WAYLAND_DISPLAY -u SWAYSOCK WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
  sway -c /path/to/minimal-config &      # e.g. `output HEADLESS-1 resolution 1500x1000`
# it creates a new $XDG_RUNTIME_DIR/wayland-N; use that for everything below
WAYLAND_DISPLAY=wayland-N target/debug/scriptorium-app SCRIPT.fdx REVIEW.txt &
WAYLAND_DISPLAY=wayland-N wtype -k Shift_L a   # each wtype run drops its first key: lead with Shift
WAYLAND_DISPLAY=wayland-N grim shot.png
```

Every command that talks to a display (the app, wtype, grim) must set
`WAYLAND_DISPLAY=wayland-N` for the private compositor on the same command
line. Never rely on the inherited environment: it points at the user's session.

Work on copies of scripts in a scratch directory: the app writes a session
file and an output `.fdx` next to the script.

More for the headless session:
- Run the app under `dbus-run-session` with `DISPLAY` unset, so a file picker
  (xdg-desktop-portal) opens on the headless output, never on the user's
  desktop. It does appear there, but typing a path into it with `wtype` is
  unreliable; instead set `SCRIPTORIUM_TEST_PICK=/path/a:/path/b` and the app's
  pickers return those paths without opening a dialog.
- Headless Sway has no pointer, and `swaymsg seat - cursor` does nothing
  without one. Clicking needs a `zwlr_virtual_pointer_v1` client (a ~40-line
  `wayland-client` program does it).
- Stop the headless Sway through its own socket (`SWAYSOCK=… swaymsg exit`),
  never with a `pkill` that could match the user's Sway.
- The app logs to `$XDG_STATE_HOME/scriptorium/scriptorium.log`; point
  `XDG_STATE_HOME` at the scratch directory.
- The chat panel's end-to-end scenarios (S1–S14 in `docs/plans/chat-panel.md`)
  run with one command, all of the above done for you:
  `crates/app/tests/scenarios/run.sh [S5 S11 …]`. It prints PASS/FAIL per
  scenario and the screenshots to look at; run it before a release and after
  changing the panel, the chat crates or the CLI's `chat`.

App layout: `main.rs` (arguments, key bindings, log + panic hook), `menus.rs`
(the one menu definition: macOS menu bar, Linux File dropdown), `workspace.rs`
(the window shell: toolbar, empty state, notices, pickers, drops),
`script_view.rs` (one open script: sidebar, editor, review card, autosave),
`agent_panel/` (the chat column: shell, key bindings, its parts as child entities; the live-store helpers are on ScriptView),
`editor.rs`, `elements.rs`, `layout.rs`, `theme.rs`, `log.rs`.

## Adding a file format

1. `cargo new --lib crates/<format>`, name it `scriptorium-<format>`, use the
   workspace package fields and `[lints] workspace = true` (copy `crates/fdx/Cargo.toml`).
2. Expose `parse(&str) -> Result<_, _>`, `to_script() -> Script`, and a writer.
3. Tests: hand-written fixtures in `tests/`, a no-op round-trip test, and edits
   that round-trip.
4. Wire it into the CLI.

## Conventions

- Edition 2024, `rustfmt.toml` sets width 120.
- Errors: a crate-level enum implementing `Display` + `std::error::Error`.
- Comments explain *why* and the contract, not what the next line does.
