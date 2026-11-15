# Scriptorium

*(working name)*

An open-source screenplay tool for Mac, Linux, Windows and the browser:
basic writing, reviewing proposed changes one at a time, and managing translations.
Built to be modified: a small Rust core, one crate per feature, and a headless
CLI, so you (or your coding agent) can add what you need. See [AGENTS.md](AGENTS.md).

**Made for agents as well as people.** Everything you can do in the app, an
agent can do from the command line (suggest changes, accept them, rewrite a
line, undo), working on the same saved session you see in the app.

**Status:** v0.3.0: reviewing, full editing and multi-line suggestions work; see the roadmap.

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
option, `Tab` next option, `R` keep mine, `E` rewrite the line in place (`Cmd+Return`
saves, `Esc` puts it back), `J`/`K` next/previous, `U` undo, `Shift+U` redo.
Click the script to edit text inside a line; `Esc` returns to the card.
`Ctrl+0`/`Ctrl+1`… switch views, `Ctrl+S` writes `MY SCRIPT (scriptorium).fdx`
(the original is never changed), `Ctrl+Q` quits. Work is saved as you go in
`MY SCRIPT.scriptorium.json` and picked up next time.

Editing: `Enter` splits a line, `Backspace`/`Delete` at an edge joins lines,
and `Tab` changes the line's type (also the way to fix a wrong guess after a
paste). Pasting from other apps guesses each line's type; a single pasted line
takes the type of the line it lands in. Copying to Final Draft gives plain
text. Dual dialogue lines can't be split, joined or removed yet.

From the command line (`scriptorium --help` for everything):

```
scriptorium info "MY SCRIPT.fdx"
scriptorium reviews "MY SCRIPT.fdx" add review.txt     # attach (the app sees it too)
scriptorium suggestions "MY SCRIPT.fdx" --pending --json
scriptorium decide "MY SCRIPT.fdx" 12 accept 2         # --review <name> if several are attached
scriptorium suggest review.txt "MY SCRIPT.fdx" --para 40 --after "New text." --why "tense"
scriptorium apply "MY SCRIPT.fdx"
scriptorium insert "MY SCRIPT.fdx" --after 12 --block 'Character: ANNA\nDialogue: Hello.'   # or --start / --end
scriptorium insert "MY SCRIPT.fdx" --end --guess 'INT. KITCHEN - DAY'   # guess each line's type
scriptorium remove "MY SCRIPT.fdx" 12..15              # first..last in script order
scriptorium split "MY SCRIPT.fdx" 12 --at "Then"       # or --offset 20
scriptorium join "MY SCRIPT.fdx" 12 13
scriptorium retype "MY SCRIPT.fdx" 12 Action
```

## Suggestions

A suggestion can change several lines at once: cut lines, add lines, or rework
a passage. In the app the script keeps its dots, and the card shows the lines
that go struck through in red and new lines in green (with a bar). In review
files `[cut]` means remove.

```
scriptorium suggest review.txt "MY SCRIPT.fdx" --paras 412..415 --block $'CHARACTER: MARIA\nDIALOGUE: What?'
scriptorium suggest review.txt "MY SCRIPT.fdx" --paras 412..415 --block '[cut]'
```

Sharing a folder with someone needs the same Scriptorium version on both sides.

Problems are logged to `~/Library/Logs/Scriptorium/scriptorium.log` (macOS) or
`~/.local/state/scriptorium/scriptorium.log` (Linux).

## Roadmap

Being planned; see [docs/PLAN.md](docs/PLAN.md). Direction so far: writing stays
in Final Draft at first, and Scriptorium starts with **review** and
**translation**. v0.3.0 is done: full editing and block (multi-line) suggestions. Next: see [docs/PLAN.md](docs/PLAN.md).

Done so far: the document model, Final Draft import/export with byte-identical
round trips, an event-sourced editing core (undo/redo, stable paragraph ids), a
GPUI spike that settled the UI stack, and a first version of reviewing (app and
CLI), ported from `script-review` with identical output.

## Licence

MIT OR Apache-2.0.
