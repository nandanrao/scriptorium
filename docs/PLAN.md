# Plan and decisions

Living notes on direction. Update this when a decision is made or reversed.

## Direction (2026-11-14)

- **Writing stays in Final Draft for now.** Scriptorium starts with what Final
  Draft doesn't do well: reviewing proposed changes and managing translations.
  Our own text editing comes later.
- **Plan the full roadmap before building much more.** The work so far is
  foundations plus a spike, not the start of a feature list.

## Decided

- **Agents are first-class users** (2026-11-14). Everything a person can do in
  the app, an agent can do from the CLI, on the same saved session. Every
  feature is designed for both from the start.
- Rust core, one crate per feature, pure library crates (no I/O), headless CLI.
- `.fdx` round-trips byte-for-byte; we splice changes, never reserialize.
- Undo/redo comes from the core's append-only event log, not from the UI.
- Licence: MIT OR Apache-2.0 (placeholder, open to change).
- **UI stack: GPUI native** (2026-11-14), pinned to a Zed commit. Decided by
  the spike below. A web build may later come from `gpui_web`.

## Spike: GPUI native editor (done, 2026-11-14: successful)

**Purpose: decide the UI stack (GPUI native vs Tauri/web), nothing more.**
It is deliberately small and may be thrown away.

What it must show, on Linux (Wayland) first:
- the PURPLE LIST rendered in screenplay layout, scrolling smoothly
- typing, including Spanish dead keys (es layout) and IME
- cursor movement, selecting across paragraphs, copy/cut/paste
- edits going through `scriptorium_core::Document` (undo/redo from the event log)

What it is not: a writing feature. The text-editing model it exercises
(`core::doc`) is reusable for review and translation edits either way.

Findings that already affect the stack decision:
- Current Zed GPUI (git) has screen-reader support (AccessKit) and a web backend
  (`gpui_web`). The crates.io release (0.2.2, Oct 2025) is far behind, so we pin
  a Zed commit.
- `gpui` is Apache-2.0; Zed's `editor`, `ui`, `text`, `rope` crates are GPL-3.0.
  Read them for ideas, don't copy code from them.
- Spellcheck is not provided by GPUI; we'd build it.

Results so far (2026-11-14, Linux/Sway/Wayland, Intel Arc, no Vulkan driver
installed — GPUI fell back to another renderer and still drew fine):
- Verified by driving the app: PURPLE LIST (2,921 paragraphs) renders with
  Final Draft's indents and spacing, italics, caps; accented typing; **Spanish
  dead keys compose correctly** (´+e → é, ~+n → ñ); Enter follows element
  rules; Tab cycles elements; up/down through soft-wrapped lines; an edit
  costs ~0.6–0.8 ms in a debug build.
- Fixed along the way: GPUI reports a cursor at a soft wrap as the end of the
  previous line; we place it at the start of the next. `KeyBinding::new` no
  longer accepts `secondary-` in this Zed version.
- Size: the whole editor (layout, input, IME, selection, mouse, clipboard,
  undo wiring) is ~1,300 lines in `crates/app`.
- Hand-tested by the user: "overall good". Typing, scrolling, selection and
  undo felt right.
- Known gaps, for when editing becomes a feature:
  - **Copy/paste loses element types and styles**: it goes through the
    clipboard as plain text and takes the type of the paragraph it lands in.
    Fix: put the paragraphs (kind + spans) on the clipboard as JSON metadata
    alongside the plain text (`ClipboardItem::new_string_with_json_metadata`),
    and add a core `paste_fragment` command that keeps the types when pasting
    whole paragraphs. Plain text stays for pasting into other apps.
  - Saving handles text edits only (no added/removed/retyped paragraphs).
  - Not yet tried: a real IME (fcitx5/ibus), macOS, Windows, spellcheck.

## Roadmap: AI features (draft, 2026-11-14)

All three need heavy design work before building. AI is called directly with
the writer's own API key for the major providers (Anthropic, OpenAI, Google),
instead of today's copy-through-a-chat step.

**Shared shape.** Each feature produces the same thing: *suggestions anchored
to the script*, worked through one at a time (accept, pick an option, rewrite,
keep mine, skip, undo). They differ in where suggestions come from and what
they anchor to. So the review queue and the anchor model are the core, and each
feature is a source of suggestions. Build that core once and keep it generic.

### 1. Language review (built, in `script-review`)

Spelling, grammar and naturalness suggestions, reviewed line by line.
- Port to Scriptorium and call the provider directly.
- Keep the plain-text review file as the interchange format, so the no-key
  path (paste into any chat) still works.
- To work out: splitting a full script into chunks (token limits, cost, keeping
  context such as character voice across chunks).

### 2. Translation versions

A project holds the script in several languages, and versions of each. The
writer is multilingual and writes every version; AI only helps keep them in step.
Paragraphs are aligned across languages. When one changes, its counterparts are
marked stale, AI proposes an update to each, and the writer reviews it in the
same review UI: what changed in the source, the current target paragraph, and
the proposed update, ready to accept or rewrite.
- Depends on open questions 2 and 5 (source of truth, paragraph identity).
  This feature drives the data model, so design it first even if it ships later.
- Aligning two existing scripts needs matching (heuristic or AI) plus manual
  fixes. Added, split or merged paragraphs and scenes are the hard cases.
- To work out: is one language the master, or can either lead? Must versions
  stay 1:1 in structure? Are "versions" Final Draft revision sets (colored
  pages, e.g. PURPLE LIST) or our own snapshots? One `.fdx` per language?

### 3. Feedback review

Paste an email or a blob of notes from someone. AI splits it into individual
notes, anchors each one, and you work through the script note by note.
- Most notes aren't line edits ("Marta's arc feels rushed"). A note is a *task*
  that may produce suggestions, not a suggestion itself. Anchors vary: a line,
  a range, a scene, a character throughout, or the whole script.
- Per note: jump to the anchor, edit by hand or ask AI for a suggestion that
  addresses it, then mark it done, won't do, or later.
- Feedback usually refers to an older draft, so anchor by quoting (as `BEFORE`
  does today) rather than by position.
- To work out: how much editing happens in Scriptorium versus Final Draft
  (small line rewrites, as `E` does today, may be enough). Merging notes from
  several people. A report back to the giver of what was done with each note.

### Across all three

- API keys in the OS keyring, one per provider, plus model choice.
- Show an estimated cost before a run. Nothing is sent without the writer
  asking (scripts are unreleased and confidential).
- Prompts are versioned in the repo, and every provider's answer is parsed into
  the same suggestion format, so providers are interchangeable.

### How AI is accessed (draft, 2026-11-14)

Inspired by Zed's agent panel, but our own design: we're doing something
different, and Zed's agent crates are GPL-3.0 anyway.

- **Two ways in.**
  - A built-in agent using the writer's own API key. The three features run on
    it. For many writers this will be their first agent workflow, so it must
    work without Claude Code or any other tool installed.
  - External agents (Claude Code, Gemini CLI, Codex…) over the **Agent Client
    Protocol** (ACP, Apache-2.0, Rust crate available). Writers use the
    subscription they already have.
- **Screenplay tools are CLI commands.** Agents never edit `.fdx` XML. The
  `scriptorium` CLI gives them commands to read a scene, search, list
  characters, propose a change and add a note. Any agent with a shell can use
  them. An MCP wrapper can come later if it's needed.
- **Agent output becomes suggestions.** Proposed changes, and any file writes
  made through ACP, go into the review queue. The script changes only when the
  writer accepts.
- **Several threads at once**, each producing its own suggestion set (e.g.
  dialogue polish, Spanish consistency, one person's notes). Suggestions on the
  same paragraph from different threads are shown together, or the older one is
  marked stale.
- **Prompts are files in the project**, so they're versioned with it.
- **Order:** suggestion/review core → built-in agent → CLI tools → ACP and
  several threads.

### Modules: agent-built web apps on the script (draft, 2026-11-14)

Agents (the built-in one, or any coding agent) build small web apps on top of
the project: visualizations, a storyboard, a character bible, a scene
breakdown. They're shown in a webview inside the app, saved and organized in
the project, and update live as the script changes. The writer can ask the
in-app agent to build a new module. This is the modular idea: features can be
added by building modules, not only by changing Scriptorium.

- **A module is a folder** in the project: a manifest (name, permissions),
  HTML/JS, and its own data. Versioned with the project (Git).
- **Local backend.** Scriptorium serves modules and an API to them (custom URL
  scheme or localhost):
  - read the script model (scenes, paragraphs, characters, languages);
  - subscribe to changes, fed by the core's event log, for live updates;
  - read and write the module's own data;
  - propose script changes, which go into the review queue.
- **Rule: modules never edit the script directly.** They read freely, write
  their own data, and propose changes like any agent.
- **A small module SDK** (JS library + docs + CLI to scaffold and test), so any
  coding agent can build one from the docs alone.
- **Sandboxed.** Module code is agent-written and sees an unreleased script: no
  network access by default, permissions declared in the manifest.

Consequences and risks:
- **Stable scene and paragraph ids become essential** (open question 5). A
  storyboard frame must stay attached to its scene through rewrites and new
  drafts from Final Draft.
- **Webview inside a GPUI window is unproven.** `wry` can embed a child webview
  on macOS, Windows and X11, but child webviews on Linux Wayland are limited.
  Test early; a separate window is the fallback. Using Tauri for this is
  acceptable (either alongside the GPUI UI or by switching), though a separate
  window only needs `wry` itself, which is what Tauri uses underneath.
- **Module data is JSON files** (decided 2026-11-14): they diff and merge well
  in Git. If performance becomes a problem, add a thin index (e.g. SQLite) on
  top as a rebuildable cache; the flat files stay the source of truth.
- **Scope:** this is a platform. Design the module API early, because it shapes
  the core (ids, change events, storage), but build it after the review core.

### Next test: a webview in the GPUI app on Wayland (not started)

Decision so far: use `wry` directly (no full Tauri) if it works.

Goal: find out whether a module's web page can be shown *inside* the GPUI
window on Linux Wayland, and what the fallback is if not.

Known before starting (2026-11-14):
- GPUI's `Window` implements `raw_window_handle::HasWindowHandle` (Wayland,
  X11 and macOS backends), so it can be handed to `wry`.
- Current `wry` is 0.57.2. On Linux it renders with WebKitGTK
  (`webkit2gtk-4.1`, installed here) and needs GTK initialized and its event
  loop pumped.
- To verify: `wry`'s docs say child webviews (`build_as_child`) work on X11
  only, not Wayland. On Wayland, `wry` wants a GTK container (`build_gtk`),
  which GPUI doesn't have.

The test: a throwaway crate (`crates/webview-spike`, outside the workspace
lints because it may need `unsafe`) that opens a GPUI window and tries, in
order:
1. **Child webview on native Wayland**: `build_as_child(window)`, GTK pumped
   from a GPUI timer. Expected to fail; confirm how.
2. **Child webview under X11** (GPUI forced onto XWayland): same code. Check
   it draws, resizes and moves with a GPUI panel, takes keyboard focus, and how
   it looks with fractional scaling (XWayland can blur).
3. **Separate window on Wayland**: a GTK window with `build_gtk`, GTK on its
   own thread, or a helper process using `tao` + `wry`. A separate process
   also isolates agent-written module code from the app.

For each: does the page load from a custom URL scheme (`scriptorium://`), and
can the app push a change to the page (live update) and receive a message
back (a proposed change)?

Outcome decides: modules in a panel inside the window (if 1 or 2 is good
enough), or in their own window (3). Only if neither is acceptable, revisit
Tauri for the whole UI.

## Review window (mockup, 2026-11-14)

Mockup: https://claude.ai/artifact/QZyn6MBK2oLhZ77zUK4o5Z (private).

Decided with the user:
- **A review is a view of the script.** The left sidebar picks what you look
  at: the plain script, or one of the loaded reviews. The selected review
  expands in the sidebar into its queue of changes (grouped by scene).
- **Right panel is the suggestion card only** (options, why, accept / keep
  mine / rewrite / skip), same keys as `script-review`.
- **Decisions change the script**, and every view shows the result.
- **Review file format stays exactly as today** (`.review.txt`). How reviews
  are produced is out of scope for now.
- **Feedback notes are parked; multilingual comes later** (translation sync
  is out of the mockup and the MVP).
- **Direct editing inside paragraphs is on** (2026-11-14), since the editor
  spike worked. Adding, joining and retyping paragraphs stays off until the
  `.fdx` writer can save them. Final Draft editing the same file is handled by
  working on separate versions for now.

Open, shown as "to decide" in the mockup:
- One undo history across all reviews (mocked) or one per review.
- Two reviews on one line: a suggestion is found by its `BEFORE` text; if the
  line changed since, the card says so (mocked).
- Script view read-only for now.
- "Apply to .fdx" writes all accepted changes at once; decisions autosave as
  events.

### Review MVP (built, 2026-11-14)

`scriptorium-app SCRIPT.fdx [REVIEW.txt…]`, and the same through the CLI.

- `crates/review`: the review-file parser, matching (BEFORE → paragraph),
  decisions, the saved session. A port of `script-review/src/core`. Accepting
  every suggestion of the PURPLE LIST review files gives a `.fdx` **byte-identical**
  to the JS tool's (checked for both review files, 507 suggestions).
- **One event log for everything.** A decision is a `Document` transaction:
  the text change plus a metadata op (`review/<review id>/<entry>` →
  `accepted:k` / `rejected` / `rewritten`). Typing, decisions, rewrites and
  undo are all in one log; undo is one history across all reviews and edits.
  A rewrite (E, typing, Enter) is merged into a single undo step.
- **Saved session**: `<script>.scriptorium.json` holds the log, replayed over
  the script on open. Review ids are hashes of the review files' contents, so a
  session survives adding, removing or reordering review files. If the script
  itself changed, the session is set aside, not applied.
- Outputs: `<script> (scriptorium).fdx` (never the original), and
  `scriptorium progress` writes a review file with STATUS/CHOSEN/MINE.
- CLI for agents: `suggestions`, `decide`, `suggest`, `edit`, `undo`/`redo`,
  `progress`, `apply`, `review`.

Known gaps:
- The app and the CLI both write the session file; if both run at once, the
  last writer wins. The app should notice the file changing and reload.
- If a review file changes (edited, or progress written into it), its id
  changes and its decisions no longer show as decisions (the text changes stay).
- Paragraph ids are positions in the `.fdx`, which is fine while paragraphs
  can't be added or removed. Open question 5 still stands.
- No dark theme; review keys are single letters only while the card has focus.
- Not tried yet: macOS, Windows, a real IME in the review flow.

### Next: File menu and a Mac build (agreed 2026-11-14, not started)

The first outside tester is the user's partner, on a Mac; the PURPLE LIST is
their project. Plan with all context: [plans/file-menu-and-mac-release.md](plans/file-menu-and-mac-release.md).
In short:
- File › Open and File › Add review, with native pickers. On Linux, an
  in-window File button, since GPUI has no Linux menu bar.
- Reviews remembered per script in the session file.
- CLI parity for both.
- First commit, a private GitHub repo (created with `gh`), CI on Linux and
  macOS.
- A downloadable, ad-hoc-signed `Scriptorium.app` (macOS 13+), released from tags.
- Name stays Scriptorium (bundle id `org.scriptorium.app`). No File › Close.

## Proposed: Git for versions (2026-11-14, not yet decided)

A project is a folder that is a Git repository, managed by Scriptorium. Git
holds the saved versions; the event log still holds fine-grained undo/redo.

- **Contents:** one `.fdx` per language (`script.en.fdx`, `script.es.fdx`),
  review files, feedback notes, translation sync records. Final Draft keeps
  editing the `.fdx` files in place.
- **Versions:** a saved version is a commit; a named draft (a revision set, a
  draft sent to someone) is a tag. History, comparing and restoring come free.
- **Translations:** each translation records the source commit it was last
  synced to. The diff from that commit to now gives the stale paragraphs and
  the input for AI's proposed updates.
- **Feedback:** notes refer to the draft the giver read (a tag). Quotes are
  found in that draft and followed forward through the diffs.
- **Fits splicing:** because `.fdx` changes are spliced, never reserialized,
  diffs stay small and readable.
- Rust libraries: `gitoxide` (pure Rust) or `git2` (libgit2).

Caveats:
- **Git stays hidden.** No "commit", "branch" or "merge" in the UI: "Save
  version", "History", "Compare with draft from Nov 5". Scriptorium saves a
  version at meaningful moments (new draft brought in from Final Draft, review
  finished, translation synced).
- **Translations are files, not branches.** They live side by side.
- **No `.fdx` merges, at least at first.** A line-based merge of XML can corrupt
  a script. Keep one line of history per person; combine several people's work
  through the review queue, not Git merges.
- **To check:** whether Final Draft rewrites metadata on every save, which would
  make diffs noisy. Test with the PURPLE LIST file.
- Git doesn't give stable paragraph identity (open question 5); cross-language
  alignment still needs it.

## Open questions (to settle in roadmap planning)

1. ~~UI stack~~: decided, GPUI native.
2. Source of truth: `.fdx` primary, or our own project folder exporting to FDX.
   Decides where stable paragraph ids live. The Git proposal above leans toward
   a project folder that holds the `.fdx` files as they are.
3. Translation workflow: languages, direction, who translates (people, AI, both).
4. Review workflow beyond today's tool: multiple reviewers? (AI access is now
   drafted under the roadmap.)
5. Paragraph identity rules (`core::doc` module docs has a first version).
6. Name.
