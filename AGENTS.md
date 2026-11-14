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
  cli/    scriptorium-cli    `scriptorium` binary: headless access to everything
  app/    scriptorium-app    GPUI desktop app: script view, reviews, editing inside paragraphs
```

`core` modules: `model` (Script/Paragraph/Span), `doc` (editable Document:
commands, invertible ops, event log, undo/redo, paragraph-id rules), `spans`,
`rules` (Enter/Tab element flow), `outline`, `text`, `diff`.

Direction and open questions live in `docs/PLAN.md`. Read it before starting
new feature work: the roadmap is still being planned.

GPUI is a git dependency pinned to one Zed commit. Zed's `gpui*` crates are
Apache-2.0 and may be copied with attribution; Zed's `editor`, `ui`, `text`,
`rope` crates are GPL-3.0 — read them for ideas, never copy code from them.

## Rules

1. **Library crates are pure.** No file or network I/O, no threads, no clocks,
   no UI. Take and return values (strings, structs). The CLI and the app do I/O.
   This keeps every library crate compiling to `wasm32-unknown-unknown`.
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
   (`<script>.scriptorium.json`); never give the app a code path the CLI lacks.

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

Work on copies of scripts in a scratch directory: the app writes a session
file and an output `.fdx` next to the script.

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
