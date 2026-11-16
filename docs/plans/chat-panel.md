# Plan: the chat panel (v0.4.0)

Status: **revised after two design reviews** (2026-11-15). The coordinator
settled the reviewers' findings and the open questions while the user was
asleep; items marked (U) are listed under "Decided overnight" for the user to
revisit. Built for v0.4.0 (2026-11-15). This is the panel design doc that
[built-in-agent.md](built-in-agent.md) requires before any panel code.

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

## Decided overnight (coordinator, 2026-11-15; user may revisit)

Moved to the decisions list in [PLAN.md](../PLAN.md) ("Chat panel decisions",
decided 2026-11-15, user may revisit): no implicit context, the sidebar
collapse below about 1600 px, `scriptorium chat` writing the session file, and
the `keyring` crate with the env-var fallback.

Also settled (no user input needed): the chat host is its own crate; one
shared apply path; `review_path` in thread files; per-thread locking rules;
scope cuts (fixed width, no live model listing, no rename); disclosure and
whole-script rules; Cmd/Ctrl+. stops, Escape returns focus; runs continue with
the panel closed; agent proposals are not undo steps; proposals on the line
being rewritten are refused.

## Starting point (checked 2026-11-15 on `c183d7e`, v0.3.0)

- `crates/agent` is done and frozen: `Thread`, both wire protocols,
  `ReplyParser`, `turn::Run` (`Send` / `Wait` / `RunTools` / `Done`), `cancel`,
  `file::ThreadFile` (+ `tools_hash`, `check_tools`), `presets`. The one
  change this plan makes to it is an additive serde field (`review_path`).
- `crates/tools` is done: `catalogue()`, `call(ctx, name, args)` returning
  `Outcome::Text` or `Outcome::Append`, `apply(...)`, the system prompt, block
  suggestions included.
- The CLI's file executor (`scriptorium_cli::commands::tool`) can't serve
  chat as it is: it refuses `Target::Own` (`commands.rs:1105`), new reviews
  get random ids (`opened.rs:291`), and its `default_review` is "the only
  attached review". Chat gets its own apply path (§ Applying suggestions).
- **Not built:** `crates/providers` (being built now, C1/C2), `crates/chat`,
  `scriptorium chat`/`threads`/`accounts`, any transport, the replay
  transport.
- The app has no gpui-kit dependency yet. gpui-kit 0.9.1 pins the same
  `gpui-pre =0.3.12` as the app.

## gpui-kit: what we use

Read from the published 0.9.1 crates.

**Depend on `gpui-base` only.** Measured with `cargo tree` on top of our gpui
set (423 crates): `gpui-base` adds 22 (`markdown`, `html5ever` family,
`lsp-types`, `ropey`, `string_cache`…); `gpui-component` adds 75. `gpui-base`
is the unstyled behaviour layer, so we keep `theme.rs`'s look.

| Need | Widget | Notes |
|---|---|---|
| Composer | `input::Textarea` + `TextareaState` | IME, selection, undo, `auto_grow(min, max)`, `placeholder`, `InputEvent::PressEnter { secondary, shift }`. Binds its keys in the `Input` context (`input/base/state.rs`; Cmd/Ctrl+Z at :305, :325). |
| Key field | `input::Input` + `.masked(true)` | |
| Assistant text | `text::TextView` + `TextViewState::markdown` | `push_str` appends streamed text; `set_text` with an extension appends, with equal text is a no-op, so the stored message replaces the streaming one without a flash. `selectable(true)`. Stream fade off. |
| Pickers | `select::Select`, `popover` | Model picker, thread list. |
| Accounts sheet | `sheet::Sheet` | Or a plain overlay if Sheet needs `Root` (U0). |
| Init | `gpui_base::init(cx)` | Registers its `Input` bindings; **our bindings are added after it** so ours win where both match. |

The message list is GPUI's `list()` with `ListAlignment::Bottom` plus our
"Jump to latest" button; copying gpui-component's `MessageScroller`
(Apache-2.0) with attribution is the fallback. `Textarea`/`TextView` don't
need `Root`; if `Select`/`Sheet` do, U1 wraps the window root (one line in
`main.rs`). This supersedes built-in-agent.md's "own renderer on
`pulldown-cmark`".

**Known risk:** a spike could not focus a gpui-kit textarea by clicking on
headless Sway, which has no pointer. U0 retests click-to-focus with a
`zwlr_virtual_pointer_v1` client; keyboard focus (Ctrl+L) must work
regardless.

## Where the code lives

```
crates/providers   (scriptorium-providers; pure core + `shell` feature)
  core:   Account, AccountsFile, price table, model capabilities, preset model lists,
          status -> AgentError mapping (shared by Http and Replay)
  shell:  config file, keyring + env fallback, Transport, Http (ureq), Replay (cassettes), recorder

crates/chat        (scriptorium-chat, new)
  core (pure, wasm-clean, no providers):
          labels   tool-row labels, usage/cost text
          naming   thread titles, the review file name for a thread
          model    thread list entries, disclosure keys
  shell (feature, default on; depends on providers, cli for SessionLock/write_atomic/Opened):
          store    <script>.threads/: list, read, write (atomic), ids, per-thread lock
          exec     ReviewStore trait + the one apply path; FileStore (CLI)
          host     the blocking run loop (CLI) and the step helpers both shells use
          seen     first-use disclosures already shown (per user)
          clock    Clock + IdSource traits (real, and deterministic for tests)

crates/cli         `scriptorium chat`, `threads`, `accounts` subcommands (call into chat)

crates/app/src/agent_panel/   (all panel UI; depends on chat)
  mod.rs        AgentPanel entity, init(), bindings(), ToggleAgentPanel
  view_model.rs Thread + streaming buffer -> rows (pure)
  messages.rs   message list, TextViews, tool rows, notices
  composer.rs   Textarea, Send/Stop, model chip, usage, disclosure, confirms
  threads.rs    thread list popover
  accounts.rs   accounts sheet
  driver.rs     the app's run loop (GPUI task, worker thread, channel)
  live.rs       LiveStore: chat::exec::ReviewStore over ScriptView's pub(crate) methods
```

Why a separate crate: if chat lived in `cli`, the MCP server and every CLI
build would pull in providers, ureq and keyring. The pure `chat` core lets
panel UI work (U2, U3) start before the shell is done.

**Hooks into the files v0.3.0 also touches**, all in U1 and only U1:

- `main.rs`: `mod agent_panel;`, `agent_panel::init(cx)` (calls
  `gpui_base::init`) **before** `cx.bind_keys(bindings())`, and
  `b.extend(agent_panel::bindings())`.
- `workspace.rs`: one `on_action` for `ToggleAgentPanel`, forwarding to the
  view or saying "Open a script to chat about it."; the Linux File dropdown
  remembers the composer as the focus to restore (see Undo).
- `script_view.rs`: a field `agent: Option<Entity<AgentPanel>>`, the column
  in `render` after the card, the sidebar collapse below ~1600 px (U), and
  four narrow `pub(crate)` methods (below).
- `menus.rs`: View › Agent panel (Ctrl/Cmd+L), Scriptorium › AI accounts…
  (Linux: File dropdown).

The `ScriptView` methods (signatures for U1; behaviour in § Applying):

```rust
/// What a tool call reads: the live document, session, script name, caps, and
/// the attached review with `id`, if any. None while a reload must happen first.
pub(crate) fn agent_ctx(&self, cx: &App) -> Option<AgentSnapshot>;
/// Under the session lock (zero wait): the review with `id` as on disk, reopened
/// if its file changed; attaching `path` if it isn't attached. Busy: Err(Busy).
pub(crate) fn agent_review(&mut self, id: &str, path: &Path, cx: &mut Context<Self>) -> Result<usize, AgentBusy>;
/// Still under the lock: write the review's new text, update the session, refresh.
pub(crate) fn agent_write_review(&mut self, r: usize, text: String, cx: &mut Context<Self>) -> Result<(), String>;
/// Show review `id` at suggestion number `n`, focus the card.
pub(crate) fn show_suggestion(&mut self, id: &str, n: usize, window: &mut Window, cx: &mut Context<Self>);
```

## Layout

```
┌ toolbar ───────────────────────────────────────────────────────────────────┐
├ sidebar 270 ┬ script (fits width) ─────┬ card 380 ─────┬ agent 360 ────────┤
│ (rail below │                          │ (review views │ Dialogue pass  ▾ +│
│  ~1600 px)  │                          │  only)        │───────────────────│
│             │                          │               │ You: tighten the  │
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

- A fixed 360 px column at the far right, after the card, inside
  `ScriptView`'s middle row (status bar stays full width). Open/closed is
  remembered per user (an `agent-panel` line in the settings folder).
  Drag-resizing is deferred.
- It belongs to the open script. File › Open another script stops a run
  (the thread is saved at every step).
- Below ~1600 px window width the sidebar collapses while the panel is open
  (U); the editor's fit-to-width zoom absorbs the rest.

## Coexisting with the review card

- Separate columns, separate focus. The card's single-letter keys fire only
  with the card focused; the composer never sees them, and vice versa.
- Agent output never moves focus or changes the view. New suggestions update
  the sidebar and card through the normal refresh.
- A tool row "Proposed 8 changes → Dialogue pass" is a link:
  `show_suggestion(id, first added number)`, focus on the card, so A/R/E work
  at once.
- The thread's review is an ordinary attached review.
- With the panel closed, a run that ends in an error or needs the writer
  says so once in the status bar ("The agent stopped: Anthropic refused the
  key. Ctrl+L to see.").

## Message view

Rows come from a pure view model over the `Thread` plus the run's streaming
buffer:

| Thread content | Row |
|---|---|
| User text | Plain, wrapped, selectable. |
| Assistant `Text` | Markdown `TextView`. Links open only on click. |
| `Thinking` | Collapsed "Reasoning", expands; redacted: "Reasoning (hidden by the provider)". |
| `ToolCall` + `ToolResult` | One tool row. |
| `Notice` | Muted line ("Stopped before the tools ran."). |
| `Native` | Not shown. |

**Tool rows**: one line, expandable to readable arguments and the result
(first 40 lines, then "Show all"). Labels from `chat::labels` (shared with
CLI output):

| Tool | Running | Done |
|---|---|---|
| `script_info` | Looking at the script… | Looked at the script summary |
| `read_scene` | Reading scene 12… | Read scene 12 (INT. KITCHEN – NIGHT) |
| `read_script` | Reading the whole script… | Read the whole script (part 2 of 3) |
| `list_paragraphs` | Finding lines… | Found 14 lines (MARTA, scene 12) |
| `list_reviews` / `list_suggestions` | … | Listed reviews / Listed suggestions in Dialogue pass |
| `suggest_change` | Writing a suggestion… | Proposed a change to ¶812 → Dialogue pass |
| `suggest_changes` | Writing 8 suggestions… (3.2 kB) | Proposed 8 changes → Dialogue pass · 2 refused |

Errors show in `WARN` with the reason; cancelled calls read "Not run
(stopped)". `Event::ToolArgs { bytes_so_far }` shows a batch's progress.

**Streaming**: `Text`/`Thinking` deltas go into one `TextViewState` per block
index (`push_str`); `ToolCallStarted` adds a running row; repaints coalesce
to one `notify` per frame. The pushed turn replaces the streaming rows,
reusing the same `TextViewState`s. On `Input::Failed` the buffer is cleared
(turn.rs contract). The list follows the tail unless scrolled up.

## Composer

- `Textarea`, `auto_grow(1, 10)`, placeholder "Ask about the script, or ask
  for changes…".
- Enter or Cmd/Ctrl+Enter sends; Shift+Enter is a new line.
- While `Run::is_active()` the writer can type but not send; Send becomes
  Stop. The draft is never cleared except by a successful `push_user`.
- `Outcome::Withdrawn(text)`: the text returns to the composer with a notice.
- A blank message sends nothing.

## Thread list, model picker, usage

**Thread list** (header title, a popover): the script's threads, newest
first: title, model, last change, its review's suggestion count. Delete asks;
the review file, its decisions and the thread's `<id>.lock` stay (lock files
are never deleted, `crates/fsio/src/lib.rs`; the list skips `*.lock`). Unreadable or
newer-version files list greyed with the reason. A thread is created on its
first send. The list is read when it opens; there is no polling.

**Titles**: the first words of the first message (`chat::naming`, up to ~6
words / 40 chars). No rename in v0.4.0.

**Model picker** (chip: "Anthropic · claude-opus-5-5"): each account's
preset models (providers' table) plus a free-text model field; "Manage
accounts…". No live model listing in v0.4.0. A change applies from the next
run and is stored in the thread file (`scriptorium threads set-model`).

**Usage and cost**: `Thread::usage()`: "18.2k tokens · $0.24". Cost is
`Usage::cost` when reported, else the price table; unknown parts show "cost
unknown". A hover estimate of the next request is in only if trivial (the
last request's input count), else deferred.

## Accounts sheet

From "Manage accounts…" or Scriptorium › AI accounts….

- List: name, provider, key status ("in keychain", "from
  SCRIPTORIUM_ANTHROPIC_KEY", "missing"), default marker.
- Add: provider (presets + Custom: base URL, protocol), masked key, "Get a
  key" (`keys_url`), default model (preset list or free text), reasoning
  effort, output limit. "Check" sends one tiny request with the key (no
  script text) and reports success or the error.
- Remove (deletes the keyring entry), edit.
- No keyring available: the sheet says so and names the env var. Never a
  plain-text key file.
- CLI: `scriptorium accounts list|add|set|remove|check|default` and
  `scriptorium accounts key set|delete|status <id>` on the same file
  (`$XDG_CONFIG_HOME/scriptorium/accounts.json`; macOS Application Support).
  Keys from stdin (`--key-stdin`), never an argument.

## Cancel and errors

**Stop** (button, Cmd/Ctrl+.): while sending, the worker drops the
connection; bytes arrived → `Input::Reply(parser.abort())`, else
`Input::Stop(vec![])`. Waiting or at a confirm → `Input::Stop` with finished
results. Written suggestions stay.

**Runs continue while the panel is closed**; Stop is explicit; opening
another script or quitting stops them.

**Errors** are rows at the end of the thread:

| What | Row |
|---|---|
| `Action::Wait { secs }` | "Anthropic is busy. Retrying in 4 s (2 of 3)." + Stop |
| `Auth` | "Anthropic refused the key." [Manage accounts] |
| `RateLimit` | "Rate limited or out of credit." + provider message. [Retry] |
| `Overloaded` (retries used) | "Anthropic is overloaded. Try again in a while." [Retry] |
| `Offline` (retries used) | "Couldn't reach api.anthropic.com." [Retry] |
| `BadRequest` | Provider message (e.g. unknown model). [Choose model] |
| `Other` | Provider message. [Retry] |
| `Parse` | "The reply couldn't be read." Logged with the line. [Retry] |
| `File` | On open: thread greyed in the list with the reason; never overwritten. |
| `Outcome::RoundLimit` | "Stopped: the agent reached its limit of 25 tool rounds." [Continue] |
| `NotReady` | The reason (usually: type a message). |
| `Mismatch` | "Something went wrong running the tools." Logged as a bug. [Retry] |
| `ToolsCheck::Changed` on open | Banner: older tools, earlier reasoning won't be replayed. [New thread] [Continue] |
| Thread busy | "This thread is running in another program." (on open or send) |

[Retry]/[Continue] call `Run::start`. All errors also go to the log.

## First-use disclosure and whole-script reads

**Disclosure**: before the first send on a script to a provider host, a card
appears *above* the composer (the draft stays in the composer, untouched):

> **What gets sent.** Your messages, and the parts of *PURPLE LIST.fdx* the
> agent reads (scenes, search results, sometimes the whole script), go to
> Anthropic (api.anthropic.com) with your key. This conversation is saved in
> *PURPLE LIST.threads/* next to the script, and contains excerpts of it.
> [Send] [Not now]

Its Send sends the drafted message; Not now just dismisses (draft kept).
Remembered per user, keyed by provider host (from the account's base URL) and
script path (`chat::seen`, shared with the CLI). The CLI prints the same text
and needs `--yes` (or "y" on a terminal) the first time.

**Whole-script reads**: before `read_script` runs, if its size (`tools::
script_size`) exceeds about 20k tokens or $0.10 at the thread's model, the
panel shows an inline confirm: "The agent wants to read the whole script
(about 35k tokens, ≈ $0.18). [Send] [Not now]". The answer is remembered for
the thread (stored in the thread file as an additive field). Not now answers
the call with an error telling the model to use `read_scene`. CLI: asks on a
terminal, else refused unless `--allow-whole-script`. The flag overrides a
stored "Not now" (`whole_script: Some(false)`) for that run.

## How the panel drives the shared chat host

```
push_user ──▶ Thread ◀── chat::store (<script>.threads/<id>.json, <id>.lock)
turn::Run ─Send─▶ build_request ─▶ Transport (Http | Replay) ─lines─▶ ReplyParser ─Events─▶ UI
          ─RunTools─▶ tools::call(ctx) ─Append─▶ chat::exec::apply_append(store: &mut dyn ReviewStore)
          ─Wait─▶ timer (scaled by the test clock)
          ─Done─▶ outcome row
```

**The app's driver** (`driver.rs`): one GPUI task per run, holding weak
handles to the panel and the `ScriptView`. Every entity update is made from
the task's async context, one at a time, never `view.update` nested inside
`panel.update` (GPUI panics on a double borrow).
- `Send`: a worker `std::thread` runs the blocking `Transport::send`, parses
  with `ReplyParser` there and sends `Event`s and the `Turn` (or error) over a
  channel; a stop flag is checked between lines.
- `RunTools`: per call, build the context from `ScriptView::agent_ctx`, run
  `tools::call`, and for `Append` run the shared apply path with `LiveStore`.
- `Wait`: timer raced with Stop.
- After every step the thread file is written atomically.

**The CLI** (`scriptorium chat <script> [--thread id|--new] [--account a]
[--model m] [--yes] [--allow-whole-script] [--retry] "message"`): the same
steps, blocking (`chat::host`); text to stdout, tool rows (same labels) to
stderr. Ctrl+C exits; the thread was saved at the last step, and the next
start answers stranded calls as cancelled.

**Thread commands**: `scriptorium threads <script>` (list), `threads show
<id>` (transcript, the review path, its suggestion counts: pending / decided),
`threads remove <id>`, `threads set-model <id> <account> <model>`, `--json`
on each.

### Applying suggestions: one path, two stores

`chat::exec` owns the whole append path, so the CLI and the app create, name,
re-check and write the thread's review identically:

```rust
pub trait ReviewStore {
    /// The attached review whose id is `id`, re-read if its file changed on
    /// disk since it was read; attaching the file at `path` if it isn't attached.
    fn review(&mut self, id: &str, path: &Path) -> Result<usize, StoreError>;
    /// Creates `path` with `text` (a new, empty review with `Id: <id>`) and attaches it.
    fn create(&mut self, id: &str, path: &Path, text: &str) -> Result<usize, StoreError>;
    fn session(&self) -> &Session;
    fn doc(&self) -> &Document;
    /// Writes review `r`'s new file text and updates the session.
    fn write(&mut self, r: usize, text: String) -> Result<(), StoreError>;
}
```

`apply_append(store, thread, append)`:
1. Target resolution. `Target::Own` means the thread's review: if the thread
   has a `review_path` and the file exists, `store.review("thread-<id>",
   path)` (re-attaching it after a detach, never a second file with the same
   id, which `Session::add` refuses: `review/src/session.rs:997`). Otherwise
   `chat::naming` picks `<title>.review.txt` (" 2", " 3"… only for name
   collisions with *other* files), `store.create(...)`, and the thread
   records `review_path`. `Target::Review` and `Target::File` resolve as the
   host contract says.
2. `tools::apply` against the resolved review and `store.doc()`.
3. `store.write` when `file_text` is `Some` (review file first; the session
   is the store's business).

Read tools get the same context in both: `default_review` = the thread's
review when attached, never "the only attached review"; tool results use the
same wording ("this conversation's review"). S11 tests the parity.

- `FileStore` (CLI): over `scriptorium_cli::Opened`, holding `SessionLock`
  for the call; writes the review, then saves the session (it may attach,
  (U)).
- `LiveStore` (app, `agent_panel/live.rs`) over the `ScriptView` methods:
  - **Lock with zero wait and retry**, as `save_session` does
    (`script_view.rs:1041`): never `LOCK_WAIT` on the UI thread. Busy →
    retry every 200 ms from the async task, up to 10 s, then the tool result
    "The script's files are busy in another program; try again."
    (`add_reviews` can't be reused: it commits the rewrite,
    `script_view.rs:512`, and waits 5 s for the lock on the UI thread, :524,
    which also blocks for 5 s if this process already holds it.)
  - **Honour `Plan::Wait`**: if another program changed the files while the
    writer is rewriting a line, the app can't reload yet (reload clears
    `rewriting`, :1290), so the call waits like Busy.
  - **Refuse proposals on the line being rewritten**: a validation failure
    "The writer is editing this line right now; propose it later." (that
    item fails; the rest of a batch goes on).
  - Change detection needs nothing extra: the watcher's `review_changed`
    compares the held text with the disk (`script_view.rs:1105`, `sessionfile/sync.rs:70`), and the
    app wrote both.
  - **Never writes the session file**: the attach is saved by the app's
    autosave.
  - No-lock fallback (`lock_session` returning `Some(None)`, :1086, when the
    OS can't lock): proceed without the lock, as the app does for its own
    saves, with the same logged warning.

`ThreadFile` gains `review_path: Option<String>` (relative to the script's
folder, `#[serde(default, skip_serializing_if = "Option::is_none")]`) and the
whole-script answer; both additive, version stays 1.

### Per-thread locking

- A thread is locked (`fs4`, `<id>.lock` beside it) for the length of a run.
- **App, on send**: take the lock (zero wait), **re-read the thread file**
  (the CLI may have added turns), then `push_user`. Busy → "running in
  another program".
- **The app tracks its own runs**: `fs4` locks are per open file, so a second
  lock attempt from the same process would fail against itself; the panel
  keeps a set of thread ids it is running and never probes those.
- **CLI**: acquires with a short wait (2 s) so a probe from the app doesn't
  cause a false refusal; still busy → "this thread is running in another
  program".
- Opening a thread in the panel probes once (lock and release) to show it
  read-only if busy; no polling.
- No-lock fallback (the OS can't lock): runs go ahead, with the warning
  logged; two programs on one thread then risk last-writer-wins on the thread
  file, the same exposure the session file has in that mode.
- Lock files are never deleted.

### Transports and the replay transport

```rust
pub trait Transport: Send + Sync {
    /// POST `req`, calling `line` for each line of the streamed body until it
    /// returns Flow::Stop or the body ends.
    fn send(&self, req: &Request, line: &mut dyn FnMut(&str) -> Flow) -> Result<(), AgentError>;
}
```

- One status→`AgentError` mapping in providers core (status, `retry-after`,
  body → kind/retryable/message), used by both `Http` and `Replay`.
- `Http`: `ureq`, the request from `protocol::build_request`.
- `Replay`: `SCRIPTORIUM_REPLAY=<cassette>` in the CLI and the app. JSON
  Lines, one exchange per request:

  ```json
  {"expect": {"path": "/messages", "model": "claude-opus-5-5", "last_user": "tighten"},
   "status": 200, "delay_ms": 20, "lines": ["event: message_start", "data: {...}"]}
  {"expect": {"results": [{"id": "toolu_1", "contains": "Added 8"}]}, "status": 529, "retry_after": 1, "body": "{...}"}
  {"offline": "connection reset"}
  ```

  `expect` can match the last user text and **tool result content** (by call
  id, substring), so a test checks what the model was told. A mismatch or
  running out of exchanges is a loud error naming the exchange.
- **Determinism**: `SCRIPTORIUM_TEST_CLOCK=<RFC 3339 start>` fixes the clock
  and makes thread ids sequential (`t0001`…), and so review ids
  (`thread-t0001`); `SCRIPTORIUM_TEST_TIME_SCALE=0.01` scales retry waits.
  Both go through `chat::clock`.
- `scriptorium chat --record <cassette>` writes bodies (no headers, so no
  keys) from a live run, for the per-provider smoke tests and new fixtures.

## Undo integration

Anything the agent changes in the document goes through the editor's
`Document` as a transaction in the one event log. In v0.4.0 the in-app agent
changes nothing in the document (it only suggests), so:

- **Agent proposals are not undo steps**: they are appended review lines,
  like CLI/MCP suggestions. Undo after a run undoes the writer's last step.
- **Decisions on the thread's suggestions are ordinary transactions**, seen
  by undo, the session file and `scriptorium undo`.
- **Tools read the live document** at each call; `apply` re-checks each
  proposal against the live line; the line being rewritten is refused.
- **A reload during a run** replaces the session; the next call reads the new
  one; `apply`'s id check catches a stale `Target::Review` index.
- **Attaching the thread's review** is not an undo step.
- **Composer undo**: Cmd/Ctrl+Z in the composer is handled by gpui-base's
  `Input` context binding (deeper than `Workspace`), so keys never reach the
  script. Only the Edit menu needs forwarding: the panel handles
  `UndoAny`/`RedoAny` while the composer is focused and re-dispatches
  `gpui_base::input::Undo`/`Redo` to it (the state's `undo` is
  `pub(super)`). On Linux the File dropdown moves focus to itself before
  dispatching (`workspace.rs:224-264`); U1 makes the panel check "the
  composer had focus when the menu opened" (the focus the dropdown restores)
  rather than "is focused now". The macOS menu bar dispatches without moving
  focus.

## Keyboard and focus

| Key | Where | Does |
|---|---|---|
| Ctrl/Cmd+L | `Workspace` | Closed: open, focus composer. Open, focus elsewhere: focus composer. Open, composer focused: close, focus back to card or editor. |
| Enter, Cmd/Ctrl+Enter | composer | Send (not while a run is active). |
| Shift+Enter | composer | New line. |
| Escape | composer | Close an open picker; else focus back to the script (card in a review view, else editor). Panel stays open. |
| Cmd/Ctrl+. | panel | Stop. |
| Cmd/Ctrl+N | panel | New thread. |
| Tab / Shift+Tab | panel | Move focus (composer → Send/Stop → model chip → thread title); not indent. |
| Cmd/Ctrl+Z, Shift+Z | composer | Composer undo/redo (gpui-base). |

- Contexts: `AgentPanel` on the column, `AgentComposer` around the textarea.
  Overrides of gpui-base keys (Escape, Tab, Enter) bind in
  `AgentComposer > Input` and are registered after `gpui_base::init`; U0
  verifies the precedence.
- Focus never moves to the panel on its own.
- Keymap tests in the style of `main.rs`'s `action_for`.

## Headless-Sway test plan

Never on the user's display (AGENTS.md recipe). Every display command sets
`WAYLAND_DISPLAY=wayland-N`; the app runs under `dbus-run-session` with
`DISPLAY` unset; `XDG_STATE_HOME` and `XDG_CONFIG_HOME` point at scratch (so
accounts and seen-disclosures never touch the user's). The private D-Bus
session has no Secret Service, so keys come from
`SCRIPTORIUM_ANTHROPIC_KEY=test`; `SCRIPTORIUM_REPLAY`, `SCRIPTORIUM_TEST_CLOCK`
and `SCRIPTORIUM_TEST_TIME_SCALE` make runs offline and deterministic. Copies
of fixture scripts only; stop Sway with `SWAYSOCK=… swaymsg exit`.

Keyboard-driven, plus a small `zwlr_virtual_pointer_v1` helper (built in U0)
for click checks. `grim` shots at named points; the orchestrator reads them.
Cassettes in `crates/app/tests/cassettes/` (shared with CLI tests).

1. **Toggle**: Ctrl+L opens with the composer focused, again closes and
   returns focus; at 1500×1000 the sidebar is a rail, at 1700×1000 it stays;
   script and review views.
2. **Click focus**: clicking the composer focuses it (virtual pointer).
3. **Disclosure**: type a draft, Enter shows the card above it, the draft is
   intact; Send sends it; a second send doesn't show the card.
4. **Streaming**: `delay_ms: 80`; shots mid-stream and at the end; final text
   equals the thread file's.
5. **Tool rows → review**: `read_scene` + `suggest_changes`; the review file
   has `Id: thread-t0001`; the sidebar lists it; the row link shows the first
   new suggestion with the card focused; A accepts.
6. **Detach and re-attach**: detach the thread's review, ask for one more
   change: the same file is re-attached, no "… 2" file.
7. **Stop**: long `delay_ms`; Cmd/Ctrl+. mid-stream; partial text stays with
   "Stopped"; next send works.
8. **Retry and errors**: 529 then success (countdown, scaled time); 401 auth
   row; 500 → Other; malformed line → Parse; offline after retries → [Retry]
   succeeds; an unreadable thread file is greyed.
9. **Whole-script confirm**: under the threshold no confirm; over it, Not now
   → the next exchange's `expect` checks the refusal text; a second
   `read_script` in the thread doesn't ask again.
10. **Accounts sheet**: add a Custom account via `wtype`; config in scratch
    has it, no key; the sheet shows the env-var notice.
11. **Parity**: the same cassette through `scriptorium chat` and the app
    gives identical thread JSON and review file (deterministic clock/ids).
12. **CLI alongside**: with the app open, `scriptorium chat` on a new thread:
    the app's session watcher shows the new review; the thread list shows the
    thread when next opened. Running the CLI on a thread the app is running is
    refused after its 2 s wait; the panel refuses a thread the CLI is running.
13. **Undo**: type in the composer, Ctrl+Z removes only the typed text; Edit ›
    Undo (Linux File dropdown path) with the composer focused undoes composer
    text, with the card focused undoes the accept; agent suggestions remain.
14. **Rewrite in progress**: start a rewrite (E), let a cassette propose on
    that line: that item is refused with the "editing this line" reason.

Below the GUI (in `cargo test`): `view_model`, `chat::labels`, `chat::naming`,
`chat::store` (atomic writes, locks, own-run tracking, lock files skipped),
`apply_append` with an in-memory `ReviewStore` (own review creation,
re-attach, collisions), the status→`AgentError` mapping, CLI cassette tests
(including Ctrl+C then `--retry`), keymap tests. The Sway scenarios are a
script run before release, not CI.

## Task breakdown

Each piece: narrow brief, owned files, two fresh reviewers. **D** =
design-heavy (strongest model to build and review), **M** = mechanical
(Sonnet builds, strongest reviews).

| # | Piece | Owns | After | Kind |
|---|---|---|---|---|
| C1 | providers core: accounts model, price table, preset models, status→AgentError mapping; **adds every new workspace dependency** (ureq, keyring, gpui-base, crates/chat entry) once | `crates/providers` (core), root `Cargo.toml` | – | D |
| C2 | providers shell: config file, keyring + env fallback, `Transport`, `Http`, `Replay` + cassettes (`expect` on results), recorder | `crates/providers` (shell) | C1 | D |
| C3a | chat store + lock: thread files, ids, `clock`, per-thread lock, own-run tracking, `review_path` and whole-script fields in `ThreadFile` | `crates/chat/src/{store,clock}.rs`, `crates/agent/src/file.rs` (additive fields) | C1 | D |
| C3b | chat core: `labels`, `naming`, `model` (pure) | `crates/chat/src/core/` | C1 | M |
| C3c | chat exec + loop: `ReviewStore`, `apply_append`, `FileStore`, blocking `host`, `seen` | `crates/chat/src/{exec,host,seen}.rs` | C2, C3a, C3b | D |
| C4 | CLI: `chat`, `threads [show/remove/set-model]`, `accounts`; cassette tests | `crates/cli/src/commands.rs`, `crates/cli/tests` | C3c | M |
| U0 | spike: gpui-base in the app, Textarea + streaming TextView, Select/Sheet vs Root, `AgentComposer > Input` precedence, click-to-focus with a virtual-pointer helper | throwaway branch; the helper kept in `crates/app/tests/tools/` | C1 | D |
| U1 | **the only task touching** `main.rs`, `workspace.rs`, `script_view.rs`; plus `menus.rs` (panel + AI accounts items): toggle, empty fixed column, persistence, sidebar rail, the four `pub(crate)` methods, Linux-menu focus memory, keymap tests | those files, `agent_panel/mod.rs` | U0, v0.3.0 merged | D |
| U2 | view model | `agent_panel/view_model.rs` | C3b | M |
| U3 | message view: list, TextViews, tool rows, reasoning rows, jump to latest | `agent_panel/messages.rs` | U1, U2 | M |
| U4 | driver: Run loop, worker thread, channel, Stop, Wait, error rows, runs with panel closed | `agent_panel/driver.rs` | C2, C3c, U3 | D |
| U5 | live store: `LiveStore` over U1's methods; zero-wait retry, `Plan::Wait`, rewriting-line refusal | `agent_panel/live.rs` | U1, C3c | D |
| U6 | composer (sole owner of `composer.rs`): textarea, Send/Stop, model chip (preset + free text), usage, disclosure card, whole-script confirm | `agent_panel/composer.rs` | U4, U5 | M |
| U7 | thread list (open, delete, busy probe) | `agent_panel/threads.rs` | U4 | M |
| U8 | accounts sheet (list, add, remove, check; no live model listing) | `agent_panel/accounts.rs` | C2, U1 | M |
| U9 | cassettes + the Sway scenario script; run S1–S14 | `crates/app/tests/` | all | M; orchestrator reads shots |

Order: C1 → (C2 ‖ C3a ‖ C3b ‖ U0) → C3c → (C4 ‖ U1 ‖ U2) → (U3 ‖ U5 ‖ U8)
→ U4 → (U6 ‖ U7) → U9. A live smoke test per provider with the user's keys
(`--record`) before release, not in CI.

### Needs a live smoke test before release

- **OpenAI, reasoning off with an output limit.** From GPT-5.4 on, the
  providers crate sends `reasoning_effort: "none"` so Chat Completions
  accepts tools, and it always sends `max_completion_tokens` for models in
  its table (64k unless the account sets one). A community report says
  GPT-5.4 ignores `none` when `max_completion_tokens` is set: it reasons,
  spends the whole budget and returns an empty reply with
  `finish_reason: "length"`
  (<https://community.openai.com/t/gpt-5-4-ignores-reasoning-effort-none-when-max-completion-tokens-is-used/1378362>).
  Not confirmed by OpenAI. Check with `gpt-6-sol`, `gpt-5.6-terra` and
  `gpt-6-luna`: a tool call goes through and the reply has no reasoning
  tokens. If it fails, drop the limit for these models or move OpenAI to the
  Responses API.
- **OpenAI, GPT-6 Astra.** Warned as "may not support tools": its model page
  doesn't say. One tool call settles it.

## Deferred past v0.4.0

Drag-resizing the panel; live model listing; renaming threads; implicit
"what the writer is looking at" context (U); a "withdraw this batch" action;
a hover estimate of the next request (unless trivial); a `.gitignore` entry
for `<script>.threads/` (thread files contain excerpts; the disclosure says
so, and the writer decides).

## Open questions

None blocking. The (U) items above await the user's confirmation.
