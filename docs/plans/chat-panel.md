# Plan: the chat panel (v0.4.0)

Status: **draft for review** (2026-11-15). Nothing built. This is the panel
design doc that [built-in-agent.md](built-in-agent.md) requires before any
panel code. It needs two reviewers (correctness, design) and the user's
answers to the open questions at the end before building starts.

Read with: [built-in-agent.md](built-in-agent.md) (decisions, architecture,
step order), [step0-review-ids-and-tools.md](step0-review-ids-and-tools.md)
(the host contract), [block-suggestions.md](block-suggestions.md) (v0.3.0:
block suggestions, which the agent's tools already propose).

## What v0.4.0 is

The writer chats with an agent in a column at the right of the window. The
agent reads the script through the shared tools and proposes changes into the
thread's own review; the writer decides them on the review card as today.
Everything the panel does can also be done with `scriptorium chat`,
`scriptorium threads` and `scriptorium accounts` on the same files.

Already decided (built-in-agent.md, user, 2026-11-14), not reopened here:
API keys only (no sign-in); the in-app agent only suggests; its own
collapsible column at the far right after the review card, Ctrl/Cmd+L;
threads in `<script>.threads/`; functional core, imperative shell; built on
gpui-kit.

## Starting point (checked 2026-11-15 on `c183d7e`, v0.3.0)

- `crates/agent` is done and frozen: `Thread`, both wire protocols,
  `ReplyParser` (SSE lines in, `Event`s out, then a `Turn`), `turn::Run` (the
  loop as a state machine: `Send` / `Wait` / `RunTools` / `Done`), `cancel`,
  `file::ThreadFile` (+ `tools_hash`, `check_tools`), `presets`.
- `crates/tools` is done: `catalogue()`, `call(ctx, name, args)` returning
  `Outcome::Text` or `Outcome::Append`, `apply(...)` for the host, the system
  prompt (`prompt::system`), block suggestions (`paragraph_ids`,
  `insert_after`, `[]` cuts) included.
- The file executor exists: `scriptorium_cli::commands::tool` (review file
  first, then session, under `SessionLock`). The MCP server uses it.
- **Not built:** `crates/providers` (accounts, keyring, HTTP, prices),
  `scriptorium chat`, `scriptorium accounts`, any transport, and the
  "replay transport" for tests. Nothing in the repo replays recorded
  provider traffic today; this plan defines it (§ Transports). Step 2 of
  built-in-agent.md is therefore a prerequisite of the panel and is part of
  v0.4.0 (pieces C1–C4 below).
- The app has no gpui-kit dependency yet. gpui-kit 0.9.1 pins the same
  `gpui-pre =0.3.12` as the app, so it can be added directly. Branch
  `v4/spike-gpui-kit` exists (no commits yet); its findings feed U0.

## gpui-kit: what we use, what we don't

Read from the published 0.9.1 crates (the fork at `../gpui-kit` has a broken
working tree).

**Depend on `gpui-base` only, not `gpui-kit`/`gpui-component`.** Measured
with `cargo tree` on top of our current gpui set (423 crates): `gpui-base`
adds 22 (`markdown`, `html5ever` and its `markup5ever` family, `lsp-types`,
`ropey`, `string_cache`…); `gpui-component` adds 75. `gpui-base` is the
unstyled behaviour layer, which matches "our own look": we keep `theme.rs`'s
colours.

From `gpui-base` 0.9.1:

| Need | Widget | Notes |
|---|---|---|
| Composer | `input::Textarea` + `TextareaState` | IME, selection, undo, `auto_grow(min, max)`, `placeholder`, `InputEvent::PressEnter { secondary, shift }`. Binds its keys in the `Input` context. |
| Key field (accounts) | `input::Input` with `.masked(true)` | |
| Assistant text | `text::TextView` + `TextViewState::markdown` | `push_str` appends streamed text (re-parses incrementally); `set_text` with an extension of the current text appends too, and is a no-op when equal, so the stored message can replace the streaming one without a flash. `selectable(true)` for copy. Optional stream fade (`TextViewMotion`); off by default for us (calm). |
| Pickers | `select::Select`, `popover` | Model picker, thread list. |
| Accounts sheet | `sheet::Sheet` | Or a plain overlay div if Sheet needs `Root` (U0 checks). |
| Init | `gpui_base::init(cx)` | Sets its `Theme` global, registers `Input` bindings and others. |

Not used: `gpui-component`'s `MessageScroller` (its only chat-specific
piece). The message list is GPUI's own `list()` with `ListAlignment::Bottom`
(variable-height rows, follows the tail) plus our "Jump to latest" button.
If that proves fiddly, copying `MessageScroller` (Apache-2.0) into
`agent_panel/` with attribution is allowed.

`Root`: in 0.9.1 production code only `TextView` tests and a button helper
call `Root::read/update`, so `Textarea` and `TextView` don't need it. If
`Select`/`Sheet` overlays do, wrapping the window's root view is a one-line
change in `main.rs` (U0 decides).

This supersedes built-in-agent.md's "our own small renderer on
`pulldown-cmark`": `TextView` brings streaming Markdown with selection, and
its parser (`markdown` crate) is part of the 22.

## Where the code lives

```
crates/providers      (new; pure core + `shell` feature, default on)
  core:   Account, AccountsFile (JSON model), price table, model capabilities
  shell:  config file, keyring (+ SCRIPTORIUM_<PROVIDER>_KEY env fallback),
          Transport trait, Http (ureq, worker thread), Replay (cassettes), model listing

crates/cli/src/chat/  (the shared chat host, shell; used by CLI and app)
  store     <script>.threads/: list, read, write (atomic), new ids, titles, per-thread lock
  host      ThreadHost: thread file + account + transport; one step of turn::Run's I/O
  exec      Executor trait; FileExecutor (wraps commands::tool)
  review    the thread's review: file name, Id: thread-<id>, Target::Own resolution
  seen      first-use disclosures already shown (per user)
  labels    tool-row labels and usage/cost text (pure; shared with the panel and CLI output)

crates/app/src/agent_panel/   (all panel UI)
  mod.rs        AgentPanel entity, init(), bindings(), ToggleAgentPanel action
  view_model.rs Thread + streaming buffer -> rows (pure, unit-tested)
  messages.rs   message list, streaming TextViews, tool rows, notices
  composer.rs   Textarea, Send/Stop, model chip, usage
  threads.rs    thread list popover
  accounts.rs   the accounts sheet
  driver.rs     the app's run loop (GPUI tasks, worker thread, channel)
  host.rs       the live executor: compiled as a child module of script_view (below)
```

**Hooks into the files v0.3.0 also touches (kept to a few lines each):**

- `main.rs`: `mod agent_panel;`, `gpui_base::init(cx)` via
  `agent_panel::init(cx)`, `b.extend(agent_panel::bindings())`.
- `workspace.rs`: one `on_action` for `ToggleAgentPanel` that forwards to the
  view (`with_view`), or says "Open a script to chat about it." when none is
  open.
- `script_view.rs`: a field `agent: Option<Entity<AgentPanel>>` (made on first
  toggle), `.children(agent_column)` after the card in `render`, and
  `#[path = "agent_panel/host.rs"] mod agent_host;`. Declaring the live
  executor as a child module of `script_view` gives it access to
  `ScriptView`'s private fields (Rust privacy is by module) while its file
  sits with the panel code, so `script_view.rs` gains no `pub(crate)`
  accessors and almost no lines.
- `menus.rs` (not contended): View › Agent panel (Ctrl/Cmd+L), Scriptorium ›
  AI accounts… (on Linux, in the File dropdown).

## Layout

```
┌ toolbar ───────────────────────────────────────────────────────────────────┐
├ sidebar 270 ┬ script (fits width) ─────┬ card 380 ─────┬ agent 360 ────────┤
│ Script      │                          │ (review views │ Dialogue pass  ▾ +│
│ Reviews     │                          │  only)        │───────────────────│
│  Dialogue…  │                          │               │ You: tighten the  │
│             │                          │               │ dialogue in 12    │
│             │                          │               │ ▸ Read scene 12   │
│             │                          │               │ ▸ Proposed 8      │
│             │                          │               │   changes →       │
│             │                          │               │   Dialogue pass   │
│             │                          │               │ Agent: I proposed │
│             │                          │               │ eight changes…    │
│             │                          │               │───────────────────│
│             │                          │               │ [composer       ] │
│             │                          │               │ Anthropic · opus ▾│
│             │                          │               │ 18.2k tok · $0.24 │
├ status bar ─┴──────────────────────────┴───────────────┴───────────────────┤
```

- A fixed column at the far right, after the card, inside `ScriptView`'s
  middle row (the status bar stays full width). Default 360 px; the left
  edge drags between 300 and 520 px. Open/closed and width are remembered
  per user (a small `agent-panel` file next to `notices-seen`).
- It belongs to the open script: threads are the script's. File › Open
  another script stops any run (the thread is saved at every step) and
  shows the new script's threads.
- The editor fits the page to its width (`layout::fit_width`), so opening
  the panel shrinks the page rather than overflowing. At 1500 px with the
  card showing, the page zooms to about 0.58. See open question 6.
- Header: the thread's title (opens the thread list), `+` new thread.
  Body: messages. Footer: composer, model chip, usage, Send/Stop.

## Coexisting with the review card

- Two separate columns with separate focus. The card keeps its single-letter
  keys (they only fire with the card focused); typing in the composer never
  reaches them, and the card never takes keys meant for the composer.
- Agent output never moves focus or changes the view. When a run adds
  suggestions, the sidebar's counts and the card update the way they do
  for a CLI change today (refresh after the session changes), and the
  tool row says where they went.
- A tool row "Proposed 8 changes → Dialogue pass" is a link: it shows that
  review (`set_view(View::Review(r))`) and goes to the first suggestion the
  call added (`Applied::numbers`). Focus goes to the card, so A/R/E work at
  once.
- The thread's review is an ordinary attached review: named after the
  thread, decided, undone, written to .fdx, detached like any other.
- When the panel is closed and a run ends with an error or needs the writer
  (read_script confirm, disclosure), the status bar says so once ("The
  agent stopped: Anthropic refused the key. Ctrl+L to see.") using
  `ScriptView::set_notice`.

## Message view

Rows come from a pure view model (`view_model.rs`) over the `Thread` plus the
current run's streaming buffer, so the mapping is unit-tested without GPUI:

| Thread content | Row |
|---|---|
| User text | The writer's text, plain, wrapped, selectable. |
| Assistant `Text` | Markdown `TextView` (paragraphs, emphasis, lists, quotes, code). Links open in the browser only on click. |
| `Thinking` | Collapsed "Reasoning" row, expands to the text; redacted thinking shows "Reasoning (hidden by the provider)". |
| `ToolCall` + its `ToolResult` | One tool row (below). |
| `Notice` (cancelled, round limit) | A muted line ("Stopped before the tools ran."). |
| `Native` | Not shown. |

**Tool rows.** One line, expandable to the arguments (as readable fields, not
raw JSON) and the result text (first 40 lines, then "Show all"). Labels come
from `chat::labels`, shared with CLI output:

| Tool | Running | Done |
|---|---|---|
| `script_info` | Looking at the script… | Looked at the script summary |
| `read_scene` | Reading scene 12… | Read scene 12 (INT. KITCHEN – NIGHT) |
| `read_script` | Reading the whole script… | Read the whole script (part 2 of 3) |
| `list_paragraphs` | Finding lines… | Found 14 lines (MARTA, scene 12) |
| `list_reviews` / `list_suggestions` | … | Listed reviews / Listed suggestions in Dialogue pass |
| `suggest_change` | Writing a suggestion… | Proposed a change to ¶812 → Dialogue pass |
| `suggest_changes` | Writing 8 suggestions… (3.2 kB) | Proposed 8 changes → Dialogue pass · 2 refused |

An error result shows the row in `WARN` with the reason; calls answered as
cancelled read "Not run (stopped)". While arguments stream
(`Event::ToolArgs { bytes_so_far }`) the row shows the size so far, so a
200-suggestion batch visibly progresses.

**Streaming.** The driver feeds `Event::Text`/`Thinking` deltas into one
`TextViewState` per block index (`push_str`); `ToolCallStarted` adds a
running tool row. Repaints are coalesced to one `cx.notify()` per frame. When
the turn is pushed, the streaming rows are replaced by the stored message's
rows, reusing the same `TextViewState` entities (equal text: no re-render).
On `Input::Failed` the buffer is cleared, as `turn.rs` requires (nothing that
streamed before a failure is stored). Scrolling follows the tail unless the
writer has scrolled up; then a "Jump to latest" button appears.

## Composer

- `Textarea`, `auto_grow(1, 10)`, placeholder "Ask about the script, or ask
  for changes…".
- Enter sends; Shift+Enter is a new line; Cmd/Ctrl+Enter also sends (it is
  "commit" elsewhere in the app).
- Locked while `Run::is_active()` (turn.rs: never `push_user` mid-run). The
  writer can still type; Enter does nothing until the run ends, and the
  Send button is replaced by Stop.
- On `Outcome::Withdrawn(text)` (a refusal) the text goes back into the
  composer, with a notice that the model declined it.
- A blank message sends nothing (`push_user` returns false).

## Thread list, model picker, usage

**Thread list** (header title opens it; a popover): the script's threads,
newest first: title, model, last change ("2 h ago"), suggestions count of its
review. Actions per thread: open, delete (asks; the review file and its
decisions stay). A thread file that can't be read, or from a newer version,
is listed greyed with the reason (`AgentError::File`). A thread is created on
its first send, not on `+` (no empty files).

**Model picker** (chip under the composer: "Anthropic · claude-opus-5-5"):
accounts and their models (from providers' model listing, cached per
account; presets' `default_model` first), plus "Manage accounts…". A change
applies from the next run (a `Run` is fixed to one origin) and is stored in
the thread file. Replay of provider-specific reasoning is already
origin-gated in the agent crate, so switching mid-thread is safe.

**Usage and cost** (under the chip): the thread's total from
`Thread::usage()`: "18.2k tokens · $0.24". Cost is `Usage::cost` when the
provider reports one (OpenRouter), else providers' price table × tokens
(input, cached, cache writes, output). Unknown parts show "cost unknown"
rather than a wrong number (`Usage::known`). Hovering shows the breakdown and
the last request's input size ("next message sends about 14k tokens of
context"), which is the cost estimate PLAN.md asks for before each send.

## Accounts sheet

Opened from "Manage accounts…" or Scriptorium › AI accounts…; a sheet over
the panel side of the window.

- Accounts list: name, provider, key status ("in keychain", "from
  SCRIPTORIUM_ANTHROPIC_KEY", "missing"), default marker.
- Add: provider (presets + Custom: base URL and protocol), key (masked
  `Input`), "Get a key" (opens the preset's `keys_url`), "Check" (lists the
  account's models: a request that sends the key but no script text),
  default model, reasoning effort and output limit (per-account settings,
  built-in-agent.md item 6).
- Remove (deletes the keyring entry). Edit name/model/effort.
- No keyring available (headless, some Linux setups): the sheet says so and
  names the env var; never a plain-text key file.
- Everything here is `scriptorium accounts list|add|remove|check|default|models`
  on the same config file (`$XDG_CONFIG_HOME/scriptorium/accounts.json`,
  `~/Library/Application Support/Scriptorium/accounts.json`). The CLI reads a
  new key from stdin (`--key-stdin`), never from an argument.

## Cancel and errors

**Stop** (button, or Cmd/Ctrl+. in the panel): while sending, the worker
drops the connection; if bytes arrived the driver reports
`Input::Reply(parser.abort())` (what the writer saw is kept), else
`Input::Stop(vec![])`. While waiting to retry, or at a read_script
confirmation, `Input::Stop` with the results that finished. Calls without a
result are answered as cancelled by the agent crate, with its notice row.
Suggestions already written stay in the review (they are proposals; the
writer can decide or ignore them).

**Errors** are rows at the end of the thread, not dialogs:

| What | Row |
|---|---|
| `Action::Wait { secs }` | "Anthropic is busy. Retrying in 4 s (2 of 3)." with Stop. |
| `Auth` | "Anthropic refused the key." [Manage accounts] |
| `RateLimit` | "Rate limited or out of credit." + the provider's message. [Retry] |
| `Offline` (retries used) | "Couldn't reach api.anthropic.com." [Retry] |
| `BadRequest` | The provider's message (e.g. unknown model). [Choose model] |
| `Outcome::RoundLimit` | "Stopped: the agent reached its limit of 25 tool rounds." [Continue] |
| `NotReady` | The reason; usually means "type a message". |
| `Mismatch` | "Something went wrong running the tools." Logged as a bug. [Retry] |
| tools changed (`ToolsCheck::Changed` on open) | Banner: "This thread started with an older set of tools; the model's earlier reasoning won't be replayed." [New thread] [Continue] |

[Retry] and [Continue] call `Run::start` (the one way to send again). Errors
also go to the log file.

## First-use disclosure

Before the first send on a script to an account, the composer is replaced by a
card:

> **What gets sent.** Your messages, and the parts of *PURPLE LIST.fdx* the
> agent reads (scenes, search results, sometimes the whole script), go to
> Anthropic (api.anthropic.com) with your key. Nothing is sent until you
> press Send. This conversation is saved in *PURPLE LIST.threads/* next to
> the script, and contains excerpts of it.
> [Send] [Cancel]

Remembered per user, keyed by account and script (`chat::seen`, a file like
`notices-seen` in the same config folder, so the CLI shares it). The CLI
prints the same text and needs `--yes` (or a "y" on a terminal) the first
time.

**Whole-script reads.** `read_script` is the one tool that sends a lot.
Before running it the panel shows an inline confirm with its size from
`tools::script_size` and the estimated cost: "The agent wants to read the
whole script (about 35k tokens, ≈ $0.18). [Send] [Not now]". "Not now"
answers the call with an error result telling the model to work from
`read_scene`. CLI: asks on a terminal, else refused unless
`--allow-whole-script`.

## How the panel drives the shared chat host

One loop shape, two shells (the CLI's blocking one, the app's async one),
over shared parts:

```
panel/CLI ──push_user──▶ Thread ◀── chat::store (read/write <script>.threads/<id>.json, per-thread lock)
             turn::Run ──Action::Send──▶ build_request ──▶ Transport (Http | Replay) ──lines──▶ ReplyParser ──Events──▶ UI
                       ──Action::RunTools──▶ Executor: tools::call(ctx) → Outcome::Append → tools::apply → write review
                       ──Action::Wait──▶ timer
                       ──Action::Done──▶ outcome row
```

**The app's driver** (`driver.rs`): a GPUI task per run.
- `Send`: `build_request`, then a worker `std::thread` runs the blocking
  `Transport::send`, parsing lines with `ReplyParser` there and sending
  `Event`s and the final `Turn` (or error) back over a channel. A stop flag
  is checked between lines; dropping the connection unblocks a read.
- `RunTools`: on the UI thread, `view.update(cx, |v, cx| v.agent_call(...))`
  for each call; the tools are pure and fast. `read_script` first awaits the
  confirm.
- `Wait`: `background_executor().timer(secs)` raced with Stop.
- After every step the thread file is written (atomically), so a crash
  loses at most one step; reopening runs `Run::start`, which answers
  stranded calls as cancelled.

**The CLI** (`scriptorium chat <script> [--thread id|--new] [--account a]
[--model m] [--yes] [--allow-whole-script] [--retry] "message"`): the same
steps, blocking; text streams to stdout, tool rows (same labels) to stderr.
Ctrl+C just exits: the thread was saved at the last step and the next start
cancels stranded calls, so no signal-handling dependency is needed.

**Executors.** `chat::exec::Executor` has two implementations:
- `FileExecutor` (CLI): `commands::tool(script, name, args, Some(thread review path))`.
- The live executor (`agent_panel/host.rs`, a child of `script_view`):
  builds `tools::Ctx` from the editor's live `Document` and the view's
  `Session` at the moment of each call (`default_review` = the thread's
  review if attached), runs `tools::call`, and for `Outcome::Append`:
  1. resolves the target: `Target::Own` creates `<title>.review.txt` with
     `Id: thread-<id>` in the script's folder and attaches it the way
     `add_reviews` does (in memory; listed in `attached_since_sync`);
  2. holds `SessionLock`, compares `Review::text()` with the file on disk
     and reopens it if another program changed it (host contract), then
     `tools::apply`;
  3. writes `Applied::file_text` (when `Some`) with `write_atomic`, records
     the new text as seen so the 1 s watcher doesn't treat its own write
     as an outside change, refreshes rows;
  4. **never writes the session file**: the attach and any new state are
     saved by the app's existing autosave, which already owns that file.
  The thread file stores `review_id` (`thread-<id>`); the review is found
  among attached reviews by that id.

**The thread review** is named from the thread title, made safe for file
names, with " 2", " 3"… on collision (`chat::review`, shared, so the CLI
names it the same way).

**Two programs, one thread.** A per-thread lock (`<id>.lock`, the same `fs4`
lock `SessionLock` uses) is held for the length of a run. A thread running
elsewhere shows read-only in the panel ("Running in another program") and the
CLI refuses to run a thread the app is running. The panel re-reads the thread
list and the open thread when their files change (checked every second,
like `disk.rs`).

**The CLI while the app is open.** `scriptorium chat` attaching a new thread
review writes the session file, as the MCP server's suggesting tools do today;
the app reloads and merges as it does for them. See open question 2: this
meets "the in-app agent never writes the session file", but not a stricter
"no agent writes it while the app has it open".

### Transports, and the replay transport for tests

`providers::Transport`:

```rust
pub trait Transport: Send + Sync {
    /// POST `req`, calling `line` for each line of the streamed body until it
    /// returns `Flow::Stop` or the body ends. HTTP errors become AgentError::Api.
    fn send(&self, req: &Request, line: &mut dyn FnMut(&str) -> Flow) -> Result<(), AgentError>;
}
```

- `Http`: `ureq`, the request's url/headers/body as built by
  `protocol::build_request`, status and `retry-after` mapped to
  `AgentError::Api`.
- `Replay`: a cassette, chosen by `SCRIPTORIUM_REPLAY=<file>` in both the
  CLI and the app (a test hook like `SCRIPTORIUM_TEST_PICK`). No keys and no
  network are needed. A cassette is JSON Lines, one exchange per request:

  ```json
  {"expect": {"path": "/messages", "model": "claude-opus-5-5", "last_user": "tighten"},
   "status": 200, "delay_ms": 20, "lines": ["event: message_start", "data: {...}", "..."]}
  {"expect": {"tool_results": ["toolu_1"]}, "status": 529, "retry_after": 1, "body": "{...}"}
  {"offline": "connection reset"}
  ```

  `expect` fields are checked (a mismatch is a loud error naming the
  exchange, so a test can't pass by accident); `delay_ms` spaces lines out
  so streaming and Stop can be seen and driven headless; running out of
  exchanges is an error.
- `scriptorium chat --record <file>` writes a cassette from a live run
  (bodies only; no headers, so no keys), for the per-provider smoke tests
  and to make new fixtures.

## Undo integration

The rule: anything the agent changes in the document goes through the
editor's `Document` as a transaction in the one event log. In v0.4.0 the
in-app agent changes nothing in the document (it only suggests, by
decision), so:

- **Agent proposals are not undo steps.** They are lines appended to the
  thread's review file, not document events, like suggestions the CLI or MCP
  adds today. Edit › Undo after a run undoes the writer's last step, not the
  agent's proposals. (Open question 9: a "withdraw this batch" action later.)
- **Decisions on the thread's suggestions are ordinary transactions**
  (accept, keep mine, rewrite, block replace), so undo, redo, the session
  file, `scriptorium undo` and the CLI all see them, exactly as for any
  review.
- **Tools read the live document** at the moment of each call, including
  decisions and edits made while the run is going; `apply` re-checks each
  proposal against the live line, so a line the writer changed meanwhile is
  refused with a reason rather than mis-placed. Open question 10 covers a
  line the writer is rewriting at that moment.
- **A reload during a run** (the CLI wrote meanwhile) replaces the view's
  session; the next tool call builds its `Ctx` from the new one, and
  `apply`'s id check catches a stale `Target::Review` index.
- **Attaching the thread's review** is not an undo step (as with Add review).
- **Undo inside the composer** is the textarea's own: the panel handles
  `UndoAny`/`RedoAny` while the composer is focused (the action bubbles from
  the focused element, so the panel sees it before `Workspace` does) and
  forwards it to the textarea. So Edit › Undo, or Cmd/Ctrl+Z, while typing a
  message never undoes a script edit.

## Keyboard and focus

| Key | Where | Does |
|---|---|---|
| Ctrl/Cmd+L | anywhere (`Workspace`) | Closed: open and focus the composer. Open, focus elsewhere: focus the composer. Open, composer focused: close, focus back to what had it (card or editor). |
| Enter, Cmd/Ctrl+Enter | composer | Send (nothing while a run is active). |
| Shift+Enter | composer | New line. |
| Escape | composer | Close an open picker; else focus back to the script (card in a review view, else the editor). The panel stays open. |
| Cmd/Ctrl+. | panel | Stop the run. |
| Cmd/Ctrl+N | panel | New thread. |
| Tab / Shift+Tab | panel | Move focus (composer → Send/Stop → model chip → thread title); not indent. |
| Cmd/Ctrl+Z, Shift+Z | composer | The composer's own undo/redo. |

- New contexts: `AgentPanel` on the column, `AgentComposer` around the
  textarea. `gpui-base` binds Escape, Tab, Enter in its own `Input` context,
  which is deeper than ours; our overrides bind in `AgentComposer > Input`
  so they win (U0 verifies GPUI's precedence on this snapshot).
- Focus never moves to the panel on its own; replies don't steal focus.
- Keymap tests in the style of `main.rs`'s `action_for`: Ctrl+L in every
  context; Enter/Escape/Tab in the composer vs the editor vs the card; the
  card's single letters unaffected.

## Headless-Sway test plan

Never on the user's display (AGENTS.md recipe). Every command carries
`WAYLAND_DISPLAY=wayland-N`; the app runs under `dbus-run-session` with
`DISPLAY` unset; `XDG_STATE_HOME` **and `XDG_CONFIG_HOME`** point at the
scratch directory, so the test's accounts file and seen-notices never touch
the user's. The private D-Bus session has no Secret Service, so the keyring
is absent and the env-var path is used (`SCRIPTORIUM_ANTHROPIC_KEY=test`);
with `SCRIPTORIUM_REPLAY` set, nothing goes over the network. Work on copies
of the fixture scripts; stop Sway with `SWAYSOCK=… swaymsg exit`.

Scenarios are keyboard-driven (headless Sway has no pointer; a
`zwlr_virtual_pointer_v1` helper is only needed for the drag-resize check,
S12). Each takes `grim` shots at named points; the orchestrator reads them.
Cassettes live in `crates/app/tests/cassettes/`.

1. **Toggle:** Ctrl+L opens the column with the composer focused, again
   closes it and focus returns; shots at 1500×1000 and 1280×800, script view
   and review view (with card).
2. **Disclosure:** first send shows the card; Send sends; a second send
   doesn't show it; `notices-seen`-style file in scratch has the key.
3. **Streaming:** a cassette with `delay_ms: 80`; shots mid-stream and at the
   end; the final text equals the thread file's.
4. **Tool rows → review:** a `read_scene` then `suggest_changes` cassette;
   `<title>.review.txt` exists with `Id: thread-…`, the sidebar lists it,
   the row's link (keyboard: focus the row, Enter) shows the review at the
   first new suggestion; A accepts it.
5. **Stop:** long `delay_ms`; Cmd/Ctrl+. mid-stream; the partial text stays
   with "Stopped"; the thread file is sendable (`Run::start` next time).
6. **Retry and errors:** 529 then success (countdown visible, then the
   reply); 401 shows the auth row; offline after retries shows [Retry],
   which succeeds on the next exchange.
7. **Whole-script confirm:** `read_script` call; Not now gives the model the
   refusal (next exchange's `expect` checks it).
8. **Accounts sheet:** add a Custom account with `wtype` into the masked
   field; the config file in scratch has it and no key; the sheet shows "no
   keychain; use SCRIPTORIUM_…_KEY".
9. **Undo:** after S4, type in the composer, Ctrl+Z removes only the typed
   text; Edit › Undo with the card focused undoes the accept; the agent's
   suggestions remain.
10. **CLI alongside:** with the app open, `scriptorium chat` (same cassette
    format) on a new thread; within ~1 s the app lists the thread and the
    new review, and the session merges without losing app edits. Then run
    the CLI on the thread the app is running: refused.
11. **Parity:** the same cassette through `scriptorium chat` and through the
    app gives the same thread JSON (ignoring timestamps and ids) and the
    same review file.
12. **Resize:** drag the column edge (virtual pointer), width remembered
    after restart.

Below the GUI: unit tests for `view_model` and `chat::labels`, `chat::store`
(atomic writes, locks, titles, collisions), the CLI against cassettes
(including Ctrl+C mid-run then `--retry`), and the keymap tests above. These
run in `cargo test`; the Sway scenarios are a script run before release, not
CI.

## Task breakdown

Prerequisites (built-in-agent.md step 2, now part of v0.4.0) then the panel.
Each piece: a narrow brief, the files it may touch, two fresh reviewers
(correctness, design). Hooks into `script_view.rs`/`workspace.rs`/`main.rs`
are one piece (U1), after v0.3.0 lands, so other pieces never touch them.

| # | Piece | Owns | After | Model |
|---|---|---|---|---|
| C1 | providers core: accounts model, price table, model capabilities | `crates/providers` (core) | – | strongest |
| C2 | providers shell: config file, keyring + env fallback, `Transport`, `Http` (ureq, stop), `Replay` + cassette format, `--record` writer, model listing | `crates/providers` (shell) | C1 | strongest |
| C3 | chat host: `store` (atomic, ids, titles, per-thread lock), `exec` (`Executor`, `FileExecutor`), `review` naming, `seen`, `labels`, the blocking loop | `crates/cli/src/chat/` | C2 | strongest |
| C4 | CLI: `chat`, `threads [show/delete/--usage]`, `accounts …`; cassette tests | `crates/cli/src/commands.rs`, `crates/cli/tests` | C3 | Sonnet; strongest review |
| U0 | spike: `gpui-base` in the app (init, Textarea, streaming TextView, Select/Sheet with or without Root, `AgentComposer > Input` precedence, dep count) | throwaway branch (or `v4/spike-gpui-kit`) | – | strongest |
| U1 | hooks + empty column: toggle, width, persistence, menus, keymap tests | `main.rs`, `workspace.rs`, `script_view.rs`, `menus.rs`, `agent_panel/mod.rs` | U0, v0.3.0 merged | Sonnet + headless S1 |
| U2 | view model (pure): rows, streaming buffer, labels use, notices | `agent_panel/view_model.rs` | C3 | Sonnet; strongest review |
| U3 | message view: list, TextViews, tool rows, reasoning rows, jump to latest | `agent_panel/messages.rs` | U1, U2 | Sonnet + headless S3 |
| U4 | driver + composer: Run loop, worker thread, channel, Stop, Wait, errors, Withdrawn | `agent_panel/{driver,composer}.rs` | C2, U3 | strongest |
| U5 | live executor: Ctx, apply, Own review, lock, seen-write, links to the review | `agent_panel/host.rs` | U1, C3 | strongest |
| U6 | thread list, model picker, usage/cost, file watching, per-thread lock UI | `agent_panel/{threads,composer}.rs` | U4 | Sonnet |
| U7 | accounts sheet | `agent_panel/accounts.rs` | C2, U1 | Sonnet |
| U8 | disclosure + whole-script confirm | `agent_panel/*`, `chat::seen` | U4, U5 | Sonnet |
| U9 | cassettes + the Sway scenario script; run S1–S12 | `crates/app/tests/` | all | Sonnet; orchestrator reads shots |

Order: C1 → C2 → C3 → C4 ‖ (U0 → U1) → U2 → U3 ‖ U5 ‖ U7 → U4 → U6 ‖ U8 →
U9. A live smoke test per provider with the user's keys (`--record`) before
release, not in CI.

## Open questions

1. **Where the chat host lives.** This plan puts it in `scriptorium-cli`'s
   lib (`chat/`), which the app already depends on for the shared shell.
   Rule 4 ("each feature is its own crate") argues for a
   `scriptorium-chat` crate instead. Which?
2. **`scriptorium chat` while the app is open.** Attaching a new thread
   review writes the session file (the CLI/MCP path the app already reloads
   from). If "the agent never writes the session file while the app has it
   open" should hold for the CLI too, the app could hold a shared `fs4` lock
   on a `<script>.scriptorium.open` file while open, and the CLI would then
   write only the review and thread files and leave attaching to the app
   (which sees the new thread file and attaches its review). Is the stricter
   rule wanted?
3. **Thread review after the writer detaches it.** `ThreadFile` stores only
   `review_id`. Should the next suggestion re-attach the same file (needs
   the path: a `review_path` field, an additive change to the frozen agent
   crate's file format) or start a new review?
4. **Thread titles.** First words of the first message (free, local), or a
   title the model writes (one more request, sends text)? And may a thread
   be renamed (its review file keeps its first name)?
5. **Stop key.** Cmd/Ctrl+. is proposed; Zed and others use different keys.
   Is Escape-to-stop wanted instead (then Escape no longer returns focus to
   the script)?
6. **Narrow windows.** With the sidebar, card and panel open at 1500 px, the
   page zooms to about 0.58. Collapse the sidebar to a rail when the panel
   opens below some width, let the card and panel share a column, or accept
   the zoom for v0.4.0 and measure (proposed)?
7. **Implicit context.** Should a message carry what the writer is looking
   at (current scene, current suggestion), so "make this shorter" works? It
   would be sent to the provider, so the disclosure must mention it.
8. **Disclosure scope and the CLI.** Once per account per script (proposed),
   per account, or per script? Must agents using the CLI pass `--yes` the
   first time (proposed), or is the CLI exempt?
9. **Agent proposals are not undoable** (they are proposals, decisions are
   the steps). Confirm, and whether a "withdraw this batch" action is
   wanted later.
10. **A line the writer is rewriting** when the agent proposes on it: allow
    (the proposal's `before` is the half-typed text), or have the live
    executor refuse proposals on the line being rewritten?
11. **Keyring dependency.** `keyring` on Linux uses the Secret Service over
    D-Bus (extra crates). Accept, or ship v0.4.0 with env-var keys plus the
    macOS Keychain only?
12. **Model listing** sends the key (no script text) to the provider when the
    picker opens or Check is pressed. Acceptable under "nothing is sent
    without the writer asking", or only on Check?
13. **Runs while the panel is closed.** Proposed: closing the column only
    hides it and the run continues; Stop is explicit. Opening another script
    stops it. Agreed?
14. **Thread files and version control.** They contain script excerpts and
    travel with the project; should Scriptorium write a `.gitignore` entry or
    leave it to the writer?
15. **Read the whole script.** Always confirm (proposed), or only above a size
    or cost threshold, and remembered per thread?
