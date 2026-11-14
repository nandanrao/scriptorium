# Plan: File menu, and a Mac build for the first outside tester

Status: steps 1–3 built (2026-11-14); step 4 (hand-off) next. The partner has an Apple Silicon Mac; the user forwards the zip. Written for the agent who builds
it. Read `AGENTS.md` and `docs/PLAN.md` first; this file assumes them.

## Why

The review MVP works on Linux (the user tried it on the PURPLE LIST and was
happy with it). The next tester is the user's partner. The PURPLE LIST is
**their** project, so they will use it on their own script and review files.
They use a Mac. Two things are missing for that:

1. A way to open files from the app. Today the app only takes paths on the
   command line, which a Finder-launched app never gets.
2. A Mac build they can download and run, without installing Rust or Xcode.

## Decisions (made by the user, 2026-11-14)

- **File › Open…** opens a script. **File › Add review…** adds a review file to
  the open script. Simple, but meant to last.
- **Option 2 for the Mac:** we ship a ready-made, downloadable app. The partner
  does not build it.
- **GitHub:** create a **private** repo with `gh` (authenticated as
  `nandanrao`, token has `repo` scope). Permission to create it is given.
  Suggested name `scriptorium`, private.
- **Name stays "Scriptorium"** for now (still a working name, `PLAN.md` open
  question 6). Bundle id: `org.scriptorium.app` (easy to change later).
- **Minimum macOS: 13 (Ventura).**
- **No File › Close.** Quitting is enough.
- Still unanswered: whether the partner has a GitHub account (to download
  releases from a private repo). If not, the user downloads the zip and sends
  it. Ask before step 4.

## Where things stand (code map)

Workspace `crates/`, GPUI pinned to Zed rev
`99c890d39d336052d9dae34c7914b55ee82abc5f` (a local checkout is in
`~/.cargo/git/checkouts/zed-*/99c890d/`; read its `crates/gpui/examples/` for
API usage). Rust 1.99, edition 2024.

- `core`: `Document` (`doc.rs`) is the event-sourced model: an append-only log
  of `Event::{Edit(Transaction), Undo, Redo, MergeUndo{depth}}`. A transaction is
  invertible ops, including `Op::SetMeta` (string metadata).
  `replace_text(sel, para, text, meta)` changes a paragraph and metadata in one
  undo step. `rebase()` makes the current state the starting point (no undo
  below it). `replay_onto(events)` replays a saved log.
- `fdx`: `FdxFile::parse`, `to_script`, `write_script` (splices text changes
  into the original bytes; refuses structural changes), `is_caps(kind)`.
- `review`: `Review::open(name, text, &script, is_caps, script_file_name)`,
  `Session { reviews }`. Decisions go through `Session::decide` into the
  `Document`, stored as metadata `review/<review id>/<entry>`. The review id is
  the first 12 hex digits of an FNV-1a hash of the review file's contents.
  `sessionfile.rs`: `SessionFile { version: 1, script_hash, events }`,
  `file_name_for("X.fdx") == "X.scriptorium.json"`, and
  `resume(&session, script, script_contents, saved_json) -> (Document, Resumed)`.
- `cli`: `src/main.rs` (info/dump/roundtrip) and `src/review_cmds.rs` (review,
  suggestions, decide, edit, undo, redo, suggest, progress, apply). Each
  command opens the script, review files and session the way the app does, and
  saves the session back. It refuses to overwrite a session it can't replay.
- `app`:
  - `main.rs`: arguments, key bindings, window.
  - `workspace.rs`: `Opened::load(script, reviews)` reads everything;
    `Workspace` is the window (sidebar, editor, card, status bar, autosave,
    apply).
  - `editor.rs`: `ScriptEditor` holds the `Document` and emits `EditorEvent`s.
  - `elements.rs`: paragraph painting and decorations.
  - `layout.rs`: page geometry. `theme.rs`: colours.

Today the `Workspace` is built once from an `Opened` and can't switch scripts
or add reviews. `Session` sits behind an `Rc` and is never changed after
startup. Changing that is the main work of step 1.

## Step 1: File › Open, File › Add review (Linux first)

### Behaviour

- **File › Open…** (`secondary-o`, i.e. Cmd+O on macOS, Ctrl+O elsewhere):
  native picker for one `.fdx` file. Then: commit any in-place rewrite, save the
  current session, and load the new script with the reviews attached to it last
  time (see "Remember reviews" below). Replace the window contents; one window
  only.
- **File › Add review…** (`secondary-shift-o`): picker for one or more `.txt`
  files. Open each as a `Review` against the *original* script (from the `.fdx`,
  as now), add it to the session, and switch the view to the first one added.
  - If the file carries saved decisions (STATUS/CHOSEN/MINE), apply them as
    **one undoable step**: one transaction, or several merged with
    `merge_undo_since`. Unlike at startup, don't `rebase`, because the document
    already has history.
  - Adding a file that's already open (same review id) does nothing and says so.
- **Start with no arguments**: an empty window with "Open a script…" (a button
  that runs File › Open), one line on what the app does, and the shortcut. The
  sidebar and card are hidden. Add review is disabled until a script is open.
- **Drag and drop** onto the window: `.fdx` → Open, `.txt` → Add review.
  GPUI has `ExternalPaths` and `div().on_drop::<ExternalPaths>(…)` (see
  `examples/drag_drop.rs` in the checkout).
- **Errors as notices** in the status bar, saying what to do: "Not a Final
  Draft script (no <Content>)", "This review is for "X.fdx", the open script is
  "Y.fdx"" (a warning, not a refusal), "3 suggestions didn't match a line
  (`scriptorium review` lists them)". No modal dialogs.
- **Quit** stays as is (`secondary-q` saves the session and quits). On macOS the
  app menu needs a Quit item bound to the same action.

### Menus

- One menu definition in a new `app/src/menus.rs`:
  - **Scriptorium:** About (can wait), Quit.
  - **File:** Open…, Add review…, separator, Write .fdx (`secondary-s`).
  - **Edit:** Undo, Redo (route to the review undo when the card has focus,
    else to the editor's).
  - **View:** Script, Next/Previous view.
- **macOS:** `cx.set_menus(vec![Menu { name, items: vec![MenuItem::action(…)…] }])`.
  See `examples/set_menus.rs`. Menu actions must be global actions handled with
  `cx.on_action` (or dispatched to the focused window), not only `.on_action`
  on a div, because the menu fires with no element focused.
- **Linux:** `set_menus` stores the menus but nothing shows them
  (`gpui_linux/src/linux/platform.rs:1047`). Add a **File** button at the left
  of the toolbar that opens a small dropdown listing the same items, built from
  the same definition. Zed renders its own menu on Linux for the same reason.
  Keep it simple: an absolutely positioned div closed by click-away or Esc. GPUI
  has `deferred`/`anchored` for popovers (see `examples/popover.rs`).

### File pickers

- `cx.prompt_for_paths(PathPromptOptions { files: true, directories: false,
  multiple, prompt: Some("Open".into()) })` returns a
  `oneshot::Receiver<Result<Option<Vec<PathBuf>>>>`. Await it in `cx.spawn` and
  update the workspace when it resolves. `None` means cancelled.
- macOS: native NSOpenPanel. Linux: xdg-desktop-portal (the user's machine has
  `xdg-desktop-portal-gtk` and `-wlr`). On `Err`, show "Couldn't open a file
  picker (no desktop portal?). Pass the path on the command line instead."
- There's no file-type filter in `PathPromptOptions`. Check the extension after
  picking and explain if it's wrong.

### Remember reviews per script (session file version 2)

Without this, File › Open on a script would come up with no reviews, and the
user would add them again every time.

- `SessionFile` gains `reviews: Vec<String>`: paths of the attached review
  files, **relative to the script's folder** when inside it, else absolute.
  Bump `VERSION` to 2. Read version 1 files as `reviews: []`. Don't reject them:
  the user already has a v1 session on the PURPLE LIST.
- On open: load each listed review. A missing or unreadable file gets a notice
  and is dropped from the list. A file whose contents changed gets a new review
  id: its old decisions no longer show as decisions (known gap in
  `PLAN.md`), so say "X changed since last time; its earlier decisions are kept
  in the text but no longer marked".
- Command-line review paths still work: they're added to the remembered list.
- Keep `sessionfile` pure: the struct and functions take and return strings and
  paths; the app and CLI do the file I/O.

### Restructuring the window

- Split today's `Workspace` into:
  - **`Workspace`**: the window shell (toolbar, menus, empty state, notices,
    drop target). Holds `Option<Entity<ScriptView>>`.
  - **`ScriptView`**: everything about one open script (editor, session, view,
    queue, card, autosave). Mostly today's `Workspace`, constructed from an
    `Opened`.
  - Open = save the old `ScriptView`'s session, build a new one, and swap.
- Add review = `ScriptView::add_review(review)`:
  - Push to `session.reviews` (make `Session` owned or `Rc` + rebuild; it's
    small).
  - Rebuild the per-review caches (`rows`, `row_of_item`, `item_by_para`) and
    grow `cur`.
  - Apply its saved decisions as above, refresh, schedule autosave.
- Watch for the GPUI borrow patterns already hit in `workspace.rs`. Render
  helpers return concrete types (`Div`, `Stateful<Div>`), because `impl
  IntoElement` captures `cx` in edition 2024. Never hold `self.editor.read(cx)`
  across a call that needs `cx` mutably.

### CLI parity (required: agents are first-class users)

- `scriptorium reviews <script.fdx>`: list the attached review files, with a
  progress line for each.
- `scriptorium reviews <script.fdx> add <review.txt>...` and
  `... remove <review.txt>`: change the list in the session file. Remove doesn't
  touch the text.
- Commands that take review files (`suggestions`, `decide`, `edit`, `undo`,
  `redo`, `progress`, `apply`) use the session's attached reviews when none are
  given, so an agent can work with just the script path. For `suggestions` and
  `decide` with several attached reviews, add `--review <name or file>` to pick
  one.
- Update the CLI usage text, `README.md` and `AGENTS.md`.

### Tests and checks

- `review` crate: session v2 round trip; v1 file read as v2 with no reviews;
  relative path handling; adding a review with saved decisions makes exactly
  one undo step.
- CLI: `reviews add`, then `suggestions` with no review argument; then open the
  app and see the same reviews.
- App, in the **headless Sway** (recipe in `AGENTS.md`; never the user's
  display):
  - start with no arguments
  - Ctrl+O. The portal dialog won't appear headless, so also test through
    drag-drop or a debug path. If the picker can't be driven headless, say so
    and ask the user to try the dialogs.
  - add a review, quit, reopen: the reviews come back.
- `cargo fmt --all`, `cargo clippy --all-targets` with no warnings,
  `cargo test`, and the wasm check:
  `cargo check -p scriptorium-review -p scriptorium-core -p scriptorium-fdx --target wasm32-unknown-unknown`.

## Step 2: First commit, private repo, CI

Nothing has been committed yet (`git status` shows everything untracked, branch
`main`).

- Add to `.gitignore`: `*.scriptorium.json`, `*.json.old`, `*(scriptorium).fdx`
  (scripts and review files are already ignored). **Never commit scripts,
  review files or session files**; they're the user's partner's unreleased work.
- First commit of the current state, attribution lines as instructed in the
  session. Then step 1 in its own commit(s).
- Create the repo:
  `gh repo create scriptorium --private --source . --remote origin --push`.
  It must be **private**.
- `.github/workflows/ci.yml`:
  - **Linux** (`ubuntu-latest`): install GPUI's Linux build deps (wayland, xkb,
    fontconfig, vulkan headers; check Zed's `script/linux` for the current
    list), then fmt check, clippy `-D warnings`, test, and the wasm check.
  - **macOS** (`macos-14`, Apple Silicon, has Xcode): `cargo build --release -p
    scriptorium-app -p scriptorium-cli` and `cargo test`. This is the first time
    the code compiles on macOS; expect a few fixes (e.g. `gpui_platform`
    features: `wayland`/`x11` are Linux-only and should be gated behind
    `[target.'cfg(target_os = "linux")'.dependencies]` if they break the Mac
    build).
  - Cache `~/.cargo` and `target` (GPUI builds are slow).

## Step 3: The Mac app

- **Bundle:** `script/bundle-mac.sh` builds `Scriptorium.app`:
  - `Contents/MacOS/scriptorium-app`, plus `Contents/MacOS/scriptorium` (the
    CLI, so agents on the Mac can use it)
  - `Contents/Info.plist`: `CFBundleIdentifier org.scriptorium.app`,
    `CFBundleName Scriptorium`, `CFBundleExecutable scriptorium-app`,
    `LSMinimumSystemVersion 13.0`, `NSHighResolutionCapable true`, version from
    `Cargo.toml`
  - `Contents/Resources/Scriptorium.icns`: a plain placeholder icon is fine
- **Universal binary:** build `aarch64-apple-darwin` and `x86_64-apple-darwin`
  (`MACOSX_DEPLOYMENT_TARGET=13.0`), then `lipo -create`. If x86_64 is hard on
  the runner, ship arm64 only and say so; most Macs on macOS 13+ are Apple
  Silicon. Ask the user which Mac the partner has if it matters.
- **Signing:** ad-hoc only, `codesign --force --deep -s - Scriptorium.app`
  (needed for arm64 to run at all). No notarization: on first launch the partner
  right-clicks › Open (or runs `xattr -dr com.apple.quarantine
  Scriptorium.app`). Developer ID signing and notarization ($99/year Apple
  account) are for later.
- **Release workflow:** on a `v*` tag, build, bundle, `ditto -c -k --keepParent`
  into `Scriptorium-<version>-mac.zip`, and attach it to a GitHub Release
  (`gh release create` or `softprops/action-gh-release`).
- **Opening files from Finder:** a Finder-launched app gets no arguments, so
  File › Open from step 1 is how files get in. (Handling "Open With" and drops
  on the Dock icon uses `cx.on_open_urls`; that can come later.)
- **Logs:** a Finder-launched app's stderr goes nowhere. Write warnings, errors
  and panics (via a panic hook) to `~/Library/Logs/Scriptorium/scriptorium.log`
  on macOS and `$XDG_STATE_HOME/scriptorium/` (default
  `~/.local/state/scriptorium/`) on Linux. Keep it simple: append, one line per
  event, no logging crate needed. Show the log path in the About item or the
  testing note.
- **Writing next to the script:** the app writes `<script>.scriptorium.json`
  and `<script> (scriptorium).fdx` into the script's folder. That works for an
  unsandboxed app, including in Documents (macOS may ask once for access to
  Documents/Desktop/iCloud Drive: expected). If it fails, the notice must say
  where and why.
- **Things that may differ on macOS:**
  - Cmd shortcuts: the `sec!` macro in `main.rs` already maps to `cmd-` on
    macOS. Review keys are single letters.
  - `ctrl-tab` view cycling: fine on macOS, but consider `cmd-shift-]` and
    `cmd-shift-[` as well.
  - Fonts: Courier Prime is bundled via `add_fonts`; the UI font
    `.SystemUIFont` maps to SF.
  - Title bar: GPUI's default is fine.
  - Dead keys / IME: macOS uses `insertText` and marked text. The editor
    implements `EntityInputHandler`, so Option+e then e should work. Check it.

## Step 4: Hand-off to the partner

- Ask the user whether the partner has a GitHub account (private repo
  downloads need one), or whether the user forwards the zip.
- `docs/testing-on-mac.md`, one page, written for a writer, not a developer:
  1. download and unzip
  2. move to Applications; first launch: right-click › Open
  3. File › Open their `.fdx`; File › Add review their `.review.txt`
  4. the keys (table from the README, with Cmd)
  5. what gets written and where; the original is never changed
  6. what to report, and where the log file is
- Mac checklist for the partner:
  - the menu bar and Cmd shortcuts
  - accented typing (Option+e then e → é; ñ)
  - Retina sharpness
  - trackpad scrolling
  - file pickers
  - a script in iCloud Drive
  - quitting and reopening resumes

## Known pitfalls (from building the MVP)

- **GUI testing:** never on the user's display. Use the headless Sway in
  `AGENTS.md`. Each `wtype` run drops its first key, so lead with `-k Shift_L`.
  Work on copies of scripts in the scratchpad.
- **The user's shell is zsh with `noclobber`:** `>` onto an existing file fails
  ("file exists"). Use `>|` or the Write tool.
- `pkill -f scriptorium-app` also matches the shell running it, which kills
  your own command (exit 144). Use `pkill -x scriptorium-app` or a pid file.
- Undo granularity: a multi-part action (a rewrite, adding a review with saved
  decisions) should be one undo step via `Document::undo_depth()` +
  `merge_undo_since(depth)`. Don't add new event kinds without making `replay`
  handle them.
- `ScriptEditor::run` doesn't scroll (the caller decides what to show);
  typing does. Keep it that way, or undo jumps to the top of the script.
- The JS tool's output is the reference for review behaviour. If you touch
  matching or writing, rerun the comparison: accept all with
  `scriptorium apply --accept-all` vs `script-review` core (a small node script
  importing `script-review/src/core/review.js`). The results must be
  byte-identical.

## Out of scope here

Recent files, multiple windows, File › Close, Windows builds, notarization,
dark theme, structural edits, translation, feedback notes, AI calls. All are
tracked in `PLAN.md`.
