# Scriptorium

*(working name)*

An open-source screenplay tool for Mac, Linux, Windows and the browser:
basic writing, reviewing proposed changes one at a time, and managing translations.
Built to be modified: a small Rust core, one crate per feature, and a headless
CLI, so you (or your coding agent) can add what you need. See [AGENTS.md](AGENTS.md).

**Made for agents as well as people.** Everything you can do in the app, an
agent can do from the command line (suggest changes, accept them, rewrite a
line, undo), working on the same saved session you see in the app.

**Status:** reviewing works (first version); roadmap being planned.

## Try it

Start the app, then **File › Open…** (`Ctrl+O`, `Cmd+O` on a Mac) a Final
Draft script and **File › Add review…** (`Ctrl+Shift+O`) one or more review
files (the `SCRIPT REVIEW v1` format from `script-review`). Dropping files on
the window works too. Paths can also go on the command line:

```
cargo run --release -p scriptorium-app -- "MY SCRIPT.fdx" "MY SCRIPT.review.txt"
```

The script remembers its reviews: next time, opening the script brings them
back.

The sidebar switches between the plain script and each review; a review lists
its changes. Keys on the review card: `A`/`Enter` accept, `1` `2` `3` pick an
option, `Tab` next option, `R` keep mine, `E` rewrite the line in place (`Enter`
saves, `Esc` puts it back), `J`/`K` next/previous, `U` undo, `Shift+U` redo.
Click the script to edit text inside a line; `Esc` returns to the card.
`Ctrl+0`/`Ctrl+1`… switch views, `Ctrl+S` writes `MY SCRIPT (scriptorium).fdx`
(the original is never changed), `Ctrl+Q` quits. Work is saved as you go in
`MY SCRIPT.scriptorium.json` and picked up next time.

From the command line (`scriptorium --help` for everything):

```
scriptorium info "MY SCRIPT.fdx"
scriptorium reviews "MY SCRIPT.fdx" add review.txt     # attach (the app sees it too)
scriptorium suggestions "MY SCRIPT.fdx" --pending --json
scriptorium decide "MY SCRIPT.fdx" 12 accept 2         # --review <name> if several are attached
scriptorium suggest review.txt "MY SCRIPT.fdx" --para 40 --after "New text." --why "tense"
scriptorium apply "MY SCRIPT.fdx"
```

Problems are logged to `~/Library/Logs/Scriptorium/scriptorium.log` (macOS) or
`~/.local/state/scriptorium/scriptorium.log` (Linux).

## Roadmap

Being planned; see [docs/PLAN.md](docs/PLAN.md). Direction so far: writing stays
in Final Draft at first, and Scriptorium starts with **review** and
**translation**. Our own text editing comes later.

Done so far: the document model, Final Draft import/export with byte-identical
round trips, an event-sourced editing core (undo/redo, stable paragraph ids), a
GPUI spike that settled the UI stack, and a first version of reviewing (app and
CLI), ported from `script-review` with identical output.

## Licence

MIT OR Apache-2.0.
