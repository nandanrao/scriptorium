# Plan: connecting AI agents to the app

Status: planned, not built. First review round (architecture and setup,
2026-11-17) folded in; hand test 0 decides Claude Desktop's route before
Phase E. Release target **v0.6.0**, built on a new branch
`v6/agent-connect` from `v5/versions` (after v0.5.0).

**The goal** (the user, 2026-11-17): a writer who uses Claude Desktop, and
has never opened Terminal, installs Scriptorium, clicks one button, and
Claude can work on the script open in the app, live. Claude Code, Codex and
other agents work the same way. Updating the app updates everything.

## Decisions (made with the user, 2026-11-17)

1. **Everything is in Scriptorium.app.** No plugin zip, no copies of
   binaries elsewhere, so no version skew. Agent configs point at paths
   inside the bundle.
2. **The running app is the server.** While the app is open, agent tool
   calls run inside it, on the live open document: changes show at once,
   and the agent can ask what is open and where the writer is (selection,
   scene, visible lines). The app runs them through the in-app chat's tool
   executor (`crates/chat/src/exec`, `Script`/`ReviewStore`, the app's
   `LiveStore`). MCP and the in-app chat end up with **one** set of tool
   definitions and one executor.
3. **Transport:** a small stdio connector inside the bundle, launched by the
   agent. It talks to the running app over a Unix domain socket in the
   user's Application Support folder (user-only permissions; no TCP port,
   no token). If the app isn't running, it works on the files directly, as
   `scriptorium-mcp` does today (safe under the existing locks; the app
   shows the changes when opened).
4. **Guidance lives in the server:** server `instructions`, tool
   descriptions and MCP prompts, so every agent gets it. Plugin skills are
   no longer needed.
5. **Scriptorium › Connect an AI agent…** with a button each for Claude
   Desktop, Claude Code and Codex that writes that agent's config (merging,
   never clobbering other entries, with a backup), plus "Copy setup
   command" for other agents. It tells the user to restart Claude Desktop.
6. **The `scriptorium` command is installed only from a menu item**,
   Scriptorium › "Install command-line tool…", like VS Code's "Install
   'code' command in PATH". There is no first-launch prompt and no silent
   install (changed by the user, 2026-11-17).
   - If a folder the user can write is already on their PATH, the item puts
     a symlink there.
   - Otherwise it asks for the admin password once and makes the symlink in
     `/usr/local/bin` (step F1 says why not `/etc/paths.d`).
   - Agent configs use full paths, so nobody needs the command for agents
     to work.
7. **Later, separate:** Developer ID signing and notarization, then
   auto-update (Sparkle). Last phase, notes only.

Answered by the user (2026-11-17), after the first review round:

8. **No Claude Code skills.** `plugin/` and the plugin zip go. Guidance
   reaches agents only through the server: the `guide` tool, instructions,
   and tool descriptions (prompts as an extra).
9. **An Apple Developer account: yes, before wider use.** The agent
   connection is built and shipped unsigned (ad-hoc) now; signing,
   notarization and auto-update stay the last phase (H).
10. **Agent undo undoes the last step, whoever made it,** as the file-based
    `undo` does today. It is refused while the writer is rewriting a line,
    and the tool description says it can undo the writer's typing.
11. **Only the open script goes through the app;** calls for any other
    script go to the files. **The plain CLI does not go through the app**
    (only `scriptorium mcp` uses the link); AGENTS.md rule 5 is unchanged.
12. **Release:** v0.6.0, on a new branch `v6/agent-connect` from
    `v5/versions`.

## Recommendations this plan takes (the user may override)

- **The connector is the CLI: `scriptorium mcp`**, not `scriptorium-app
  --mcp` and not a third binary. Why:
  - The bundle ships the `scriptorium` CLI anyway (for Terminal users and
    decision 6's menu item). It is already there (`script/bundle-mac.sh` copies it into
    `Contents/MacOS`). So "one binary" saves nothing over adding a
    subcommand to it, and `scriptorium-mcp` goes away. The bundle has two
    executables, as today.
  - The app binary links AppKit, Metal and CoreText (GPUI). Every agent
    start would load them for a process that only pipes JSON. The cost is
    likely tens of milliseconds; *to verify* by measuring both on the Mac.
    It would also risk a Dock icon or app-style Gatekeeper treatment if
    anything touched `NSApplication` before `--mcp` was handled.
  - The CLI already has the file-based tools the fallback needs.
  - The "setup command" for other agents is the same command people type:
    `scriptorium mcp`.
- **Claude Desktop: test a manifest-only `.mcpb` first, and fall back to
  the config file.**
  - The extension would hold no binary: just a manifest whose `command`
    is the absolute path into `Scriptorium.app`.
  - With no copy, there is no version skew. Installing it needs no restart
    of Claude, and Claude gives it an on/off switch.
  - The spec assumes a self-contained bundle, so whether Desktop accepts an
    absolute `command` is untested.
  - **Hand test 0** decides it, before Phase E is built. If Desktop
    installs it, runs the bundle's CLI, and still runs it after the app is
    replaced by a newer copy at the same path, Connect makes that `.mcpb`
    (with this copy's path) and opens it with Claude. Otherwise Connect
    writes `claude_desktop_config.json`.
  - Either way, a moved app means connecting again: a new `.mcpb`, or the
    repair in E3.
  - The config file's costs: a restart of Claude, and Desktop rewrites the
    file from memory (sources below). That is why the restart is done for
    the user (E2).
- **Each tool call picks its route.** The connector answers `initialize`,
  `tools/list` and `prompts/*` itself (it is from the same bundle, so it
  has the same definitions). It then sends **each `tools/call`** to the app
  if the app answers, and otherwise runs it on the files. Why per call:
  - The app can be opened or quit in the middle of an agent session.
  - The socket carries a small internal request/reply, not MCP, so the app
    has no MCP session state to lose.
- **Scripts the app doesn't have open go to the files.** If a call names a
  script the app doesn't have open, the app replies "not open here" and the
  connector runs the call on the files. The app has one window and one
  script today (`main.rs` opens one window).
- **Claude Code: `claude mcp add --scope user` when `claude` is found,**
  otherwise a careful merge of `~/.claude.json`. Running Claude Code
  sessions rewrite that file all the time. Its own command is the safe
  writer.
- **No plugin; the guidance is a tool.**
  - A read-only tool `guide(topic: review | translate)` returns the full
    guidance, about 10 KB from today's two skills.
  - The instructions and the relevant tool descriptions say "call
    guide('translate') before translating".
  - A tool works in every client. Prompts don't work reliably in Codex or
    Claude Desktop, and whether Desktop reads `instructions` is
    unconfirmed.
  - Prompts (`/scriptorium:…` in Claude Code) are an extra.
  - `plugin/` is removed (decision 8).

## Research (2026-11-17)

Sources were read by a research agent; anything not confirmed in official
docs is marked.

**Claude Desktop (macOS)**
- Config: `~/Library/Application Support/Claude/claude_desktop_config.json`,
  `{"mcpServers": {"<name>": {"command": "/abs/path", "args": [...],
  "env": {...}}}}`.
  - Use absolute paths.
  - Quit and restart Claude Desktop fully to load changes.
  - Logs: `~/Library/Logs/Claude/mcp.log` and `mcp-server-<name>.log`.
  - Source: https://modelcontextprotocol.io/docs/develop/connect-local-servers
- Desktop writes this file too: it keeps a `preferences` key there.
  - User reports describe it wiping `mcpServers` or replacing the file
    with a preferences-only stub, after launch, login, Cowork or account
    sync.
  - So merge, write atomically, preferably while Desktop is quit, and check
    again later.
  - Not an official statement:
    https://github.com/anthropics/claude-code/issues/32345 ,
    https://github.com/anthropics/claude-code/issues/37286 ,
    https://github.com/anthropics/claude-code/issues/34359
- When launched from the Finder, Desktop doesn't get the shell PATH, and
  its working folder is often `/`. So use an absolute `command`. Community
  reports, *to verify*.
- Spaces in `command` should be fine (it is spawned with no shell). *To
  verify* with an app at `~/Applications/My Apps/Scriptorium.app`.
- Desktop Extensions (`.mcpb`, formerly `.dxt`):
  https://github.com/modelcontextprotocol/mcpb/blob/main/MANIFEST.md
  - The manifest has `server.type` `binary` and `mcp_config {command,
    args, env}` with `${__dirname}`.
  - Installed by double-click, with a review screen.
  - Private files are updated only by reinstalling, and the directory no
    longer takes `.mcpb` submissions (it points local servers to plugins):
    https://support.claude.com/en/articles/10949351-getting-started-with-local-mcp-servers-on-claude-desktop
    , https://claude.com/docs/connectors/building/mcpb
  - *To verify:*
    - Whether a command outside the extension works.
    - Whether a signature is needed (`mcpb sign` exists).
    - Where extensions unpack (reportedly
      `~/Library/Application Support/Claude/Claude Extensions/<id>/`).
    - Whether there is a programmatic install (none is documented).
- Instructions: *to verify* whether Desktop reads them.
  - Prompts show in the "+" menu, but 2026 reports say prompts from local
    stdio servers fail to attach:
    https://claudeissues.com/issue/82045-bug-claude-desktop-prompts-from-local-stdio-mcp-servers-fail-to-attach-remote-mc
  - So the essential rules must also be in the tool descriptions (step D1),
    which every client reads.

**Claude Code** (https://code.claude.com/docs/en/mcp)
- The command: `claude mcp add --scope user --transport stdio scriptorium
  -- "<bundle>/Contents/MacOS/scriptorium" mcp`.
- `--scope user` writes the top-level `mcpServers` in `~/.claude.json`.
  The default (`local`) writes under the project's path in the same file,
  and `project` writes `.mcp.json`.
- `claude mcp add-json <name> '<inner json>'` also exists.
- It honours server instructions (capped at 4,096 characters), prompts (as
  `/server:prompt` commands) and resources.
- Plugins still work; their tools are named
  `mcp__plugin_<plugin>_<server>__<tool>`.

**Codex** (https://learn.chatgpt.com/docs/extend/mcp?surface=cli)
- The config is `~/.codex/config.toml`, with a table per server:
  `[mcp_servers.scriptorium]`, `command = "…"`, `args = ["mcp"]`.
  - Optional fields: `env`, `env_vars`, `cwd`, `startup_timeout_sec`
    (default 10), `tool_timeout_sec` (default 60), `enabled`.
  - The CLI equivalent is `codex mcp add scriptorium -- <cmd> mcp`.
  - "The ChatGPT desktop app, Codex CLI, and IDE extension share this
    configuration."
- It reads `instructions`; "keep the first 512 chars self-contained".
  Prompts and resources aren't documented (*to verify*).
- **Sandbox:** stdio MCP servers are **not** run inside Codex's sandbox.
  - They are spawned with a plain `Command::new` in their own process group
    (`codex-rs/rmcp-client/src/stdio_server_launcher.rs`), with an
    allowlisted environment: `HOME`, `PATH`, `USER`, `TMPDIR`, …
    (`rmcp-client/src/utils.rs`).
  - Servers may opt in to sandboxing themselves (`codex/sandbox-state`,
    `codex-mcp/src/sandbox_metadata.rs`). We don't.
  - So the connector can reach the socket in Application Support and write
    beside the script. That is read from the source on `main`, not tested
    on a Mac (*to verify*, hand test 6).
  - Sources: https://github.com/openai/codex/blob/main/codex-rs/rmcp-client/src/stdio_server_launcher.rs
    , https://github.com/openai/codex/blob/main/codex-rs/rmcp-client/src/utils.rs
  - `HOME` is passed, so the connector finds the socket from `$HOME`.
    Fall back to `getpwuid` if `HOME` is missing.
- Codex's approvals use the `readOnlyHint` annotation, so every tool sets
  its annotations (A1). Checked by a reviewer.

**Environments.** Claude Desktop likely passes only `HOME`, `LOGNAME`,
`PATH`, `SHELL`, `TERM`, `USER` (reviewer's reading; *to verify*). So the
socket's path is worked out from `$HOME` alone, never `$TMPDIR`.

**Lengths.** Claude Code caps instructions and each tool description at
4,096 characters. Older versions reportedly cut at 2,048 (*to verify* which
version changed it). So the rules that matter go in the first 2 KB, and
each description stays under 4,096.

**A moved app.** Configs hold an absolute path. If the user moves or
renames the app, or runs a second copy, the path breaks. On every launch,
the app checks each config it wrote and repairs it (step E3). macOS *App
Translocation* runs an app opened straight from Downloads, without moving
it, from a random read-only path
(`/private/var/folders/…/AppTranslocation/…`). Configs must never point
there. The app detects that and asks the user to move it to Applications
first. *To verify:* that `SecTranslocateIsTranslocatedURL` (or the path
test) sees it.

**Gatekeeper.** Nothing is documented for Claude Desktop spawning a binary
inside an approved, unnotarized app. *To verify* on a fresh Mac, after
"Open Anyway", whether the CLI inside the bundle runs when Claude starts
it, or needs `xattr -dr`.
- So Connect checks this itself (E2): it runs `<bundle>/scriptorium
  --version`, and shows a failure in the sheet.
- Once the user has approved the app, it tries to clear quarantine on its
  own bundle. This needs hand tests on macOS 13–15, since App Management
  may block it.
- `xattr -dr` stays in the docs as the fallback.
- Signing and notarization (Phase H) end the question.

## Names

| Internal | Label (app, docs) |
|---|---|
| the connector, `scriptorium mcp` (`crates/cli/src/mcp.rs`) | "Scriptorium's agent connection" |
| the link: socket + internal protocol (`scriptorium_fsio::link`) | — |
| socket `~/Library/Application Support/Scriptorium/agent.sock` (or `~/.scriptorium/agent.sock`, see "The socket"); Linux `$XDG_RUNTIME_DIR/scriptorium/agent.sock` | — |
| `agent.lock` beside it (who owns the socket) | — |
| route: `App` / `Files` | "working in the open app" / "working on the files" |
| offer `Offer::Chat` / `Offer::Agent` (which tools a host offers) | — |
| crate `scriptorium-connect` (`crates/connect`): config merges | "Connect an AI agent…" |
| tool `where_is_writer` (step C3) | same |
| tool `guide` (step D1) | same |
| prompts `review_pass`, `translate` | "Review pass", "Translate" |
| MCP server name in every config: `scriptorium` | "Scriptorium" |

# How it works

```
Claude Desktop / Claude Code / Codex
   │ stdio (MCP, JSON-RPC lines)
   ▼
Scriptorium.app/Contents/MacOS/scriptorium mcp      (the connector)
   │  initialize, tools/list, prompts/*: answered here
   │  tools/call: per call ──► socket up and same version? ──► the app
   │                         └─ no ──────────────────────────► the files (FileScript)
   ▼
Scriptorium.app (running): link thread ─► channel ─► UI thread:
   ScriptView::update { run_call(LiveScript, Caller::Agent, call) } ─► reply
```

## The internal protocol (`fsio::link`)

One JSON object per line, over the socket. The connector opens one
connection for its whole life (reconnecting when needed) and sends one
request at a time.

```
→ {"hello": {"proto": 1, "version": "0.6.0", "exe": "/Applications/Scriptorium.app/Contents/MacOS/scriptorium"}}
← {"hello": {"proto": 1, "version": "0.6.0", "script": "/Users/a/Film/PURPLE LIST.fdx"}}
→ {"id": 7, "call": {"name": "suggest_changes", "arguments": {…}}}
← {"id": 7, "taken": true, "script": "/Users/a/Film/PURPLE LIST.fdx"}   // the app's UI task picked the call up and is about to run it, on this script
← {"id": 7, "done": {"text": "…", "is_error": false}}
← {"id": 7, "route": "files"}        // instead of taken: not the open script, or nothing open
```

- The hello's `script` is informational only (it may change before any
  call); the script a call runs on comes back in its `taken` (optional).
- The client accepts a repeated `taken`, and a `done` with no `taken`
  before it.
- `taken` is written by the UI task through a `Replier` (a clone of the
  connection's writer, `Conn::replier()`); lines from it and the
  connection thread never interleave.

**Not delivered vs maybe applied.** The connector keeps one connection, so
the app may have quit between two calls.
- The connector checks whether the app has hung up before sending: a poll
  for hang-up or readable EOF.
- A hang-up, an `EPIPE` on send, or EOF before `taken` means **not
  delivered**. It reconnects once, and otherwise runs the call on the files.
- Only a clean hang-up (EOF, reset, `EPIPE`) before any reply, or a
  well-formed `route: files`, means the call didn't run. EOF after `taken`
  and before `done` means "maybe applied", and so does any protocol break
  (a garbled or cut-off line, a wrong id), taken the safe way.
- The UI task sends `taken` from the same update that runs the call, just
  before `run_call`.

- **Same version or no link.** If `proto` and `version` differ, the
  connector closes the link. It works on the files, and says so once in
  its first tool result: "Scriptorium 0.5.1 is open but this connection is
  from 0.6.0 (another copy of the app). Working on the files; the app shows
  the changes." With one bundle this happens only when there are two
  copies of the app.
- **Peer check.** The app accepts only the same user (`getpeereid`, which
  Linux also has).
- **Timeouts.**
  - Connect and hello: 1 s each, then the files (the connect runs on a
    short-lived thread: std has no connect timeout for Unix sockets).
  - A call has no client timeout below the agent's own. A busy app (session
    lock held, writer rewriting a line) is retried inside the app, off the
    UI thread, for `chat::exec`'s `BUSY_FOR`, then answers `BUSY`.
- **The app quits mid-call** (EOF after `taken`, before `done`):
  - A read-only tool (`scriptorium_tools::writes` false) runs again on the
    files.
  - A writing tool does not run again, since it may or may not have been
    applied. It returns an error: "Scriptorium closed during this call; it
    may have been applied. Check with list_suggestions before trying
    again." The next call reconnects or uses the files.
- **The files route waits for the lock like MCP does today.** It uses
  `FileScript::with_lock_wait(LOCK_WAIT)`, plus the busy retries up to
  `BUSY_FOR`. `FileScript`'s default wait is zero (`exec/file.rs:36-43`).

## The socket

- The folder is `~/Library/Application Support/Scriptorium/`, mode 0700.
  The socket is 0600, set right after `bind` (not with `umask`: it is
  process-wide; the 0700 folder covers the moment between).
- **Who owns it.** At launch the app takes `agent.lock` with an exclusive
  `flock`, without waiting.
  - With the lock: it removes any stale `agent.sock` and binds.
  - Without it (another copy runs): it doesn't serve. It shows once: "Another
    Scriptorium is connected to your AI agents."
  - `flock` is released when a process dies, so a crash leaves nothing
    stuck.
- On a clean quit the app calls `Server::close` (removes the socket if it
  is still its own); the accept thread keeps the server from being dropped.
- **Path length.** macOS socket paths are limited to 104 bytes. When
  `$HOME/Library/Application Support/Scriptorium/agent.sock` is too long (a
  home folder over about 45 characters), both sides use
  `$HOME/.scriptorium/agent.sock` (folder 0700) instead.
  - It is one pure rule from `$HOME`, so the app and the connector agree
    with no file to read.
  - Never `$TMPDIR`: Claude Desktop may not pass it.
- Linux (development and tests): `$XDG_RUNTIME_DIR/scriptorium/`.

## Tools: one set, two offers

Today there are two tool paths that overlap:

- **The shared catalogue** (`crates/tools/src/catalogue.rs`, 11 tools) runs
  in two places:
  - through `chat::exec::run_call` (the in-app chat and `scriptorium chat`;
    the thread's own review);
  - through `scriptorium_cli::tool` (`cli/src/commands.rs:1289`; MCP and
    the CLI `tool`; `review_file`, `Target::File`).
  `chat::exec` also handles `Target::File` (`exec/mod.rs:452`), but **not
  the same way** as the CLI (`commands.rs:1374`). The differences:

  | | exec `file_review` | CLI `tool` |
  |---|---|---|
  | missing subfolder | refused (`review_in_folder`, must exist) | created (`agent_review`, `Folders::MayCreate`) |
  | new review | written and attached before the append; an empty review is left if the append fails | added in memory; written only when there is text to write |
  | id salt | the path as given | the resolved path (`review_salt(&at)`) |

  Phase A takes **the CLI's behaviour**, so MCP results stay the same.
  The exec comment "`run_call` refuses `review_file`" becomes true only for
  `Caller::Thread`.
- **MCP-only tools** (`mcp/src/tools.rs`): `attach_reviews`,
  `detach_reviews`, `decide_suggestion`, `edit_paragraph`, `undo`, `redo`,
  `write_script`. They are implemented by the CLI's file commands
  (`commands.rs:490–846`).

After Phase A:

- Every tool spec lives in `scriptorium-tools`, with what it is offered to:
  `catalogue(Offer::Chat)` is today's chat catalogue, unchanged byte for
  byte (threads store a `tools_hash`; any change shows the "older tools"
  notice), and `catalogue(Offer::Agent)` is that plus the MCP-only tools
  and the context tool.
- **One executor:** `chat::exec::run_call(script, caller, call)` with
  `Caller::Thread(&mut ThreadFile)` (today's behaviour) or
  `Caller::Agent` (a `review_file` is needed to append, as MCP requires
  today).
  - The MCP-only tools become `ReviewStore` operations: `decide`, `edit`,
    `undo_redo`, `write_script`, `attach`, `detach`.
  - `FileStore` does them through fsio, with the file logic moved out of
    the CLI. `LiveStore` does them through `ScriptView`.
- `scriptorium_cli::tool` and `crates/mcp` are removed. The CLI `tool`
  command calls `run_call(FileScript, Caller::Agent)`.

## In the app: threading

This is the chat driver's design (`agent_panel/driver.rs:1-40`, `pump` at
`:1157`), reused:

- **A link thread** (`std::thread`) accepts connections, plus one thread per
  connection reading lines. They never touch GPUI.
- Each call goes over an `mpsc` channel to **one GPUI task** owned by the
  `Workspace`.
  - The task holds a `WeakEntity<Workspace>`, not a view: **for each
    call** it asks the workspace for its current `ScriptView`. A write in
    place replaces the view, and so does opening another script; a held
    view would send later calls to the files by mistake.
  - It drains the channel about once a frame and runs each call inside
    **one** `ScriptView::update`:
  `run_call(&mut LiveScript::new(view, cx), Caller::Agent, &call)`. It
  replies on the call's own one-shot channel.
  - It is never nested in another entity's update.
  - It never waits: the session lock is taken with zero wait
    (`try_lock_session`), and busy goes back as `Busy`.
- **Busy retries happen on the connection thread**, as `chat::host::run`
  does for the panel: sleep `BUSY_RETRY`, send again, up to `BUSY_FOR`.
- **No lock is held across UI work.** The `SessionLease` lives only inside
  the one update (as `LiveStore` does today). Socket I/O is never on the UI
  thread.
- Calls from two agents and from the in-app chat all go through the one
  UI thread, one call at a time. Each sees the last one's result.
- **Saved before the reply.** For `decide`, `edit`, `undo`/`redo` and the
  suggesting tools, the session is saved (`save_session`) before the reply
  goes back.
  - Today `after_agent_change` only schedules the autosave, so a crash
    would lose a change the agent was told was made.
  - The suggesting tools already write their review file; the session save
    covers the decisions.

# Phase A — one tool set, one executor

Ends with: the MCP server (still the old binary) and the in-app chat run
the same executor, and every MCP test passes unchanged over it.

### A1. Every tool spec in `scriptorium-tools` (`tools/src/catalogue.rs`, new `tools/src/agent.rs`)

- What changes:
  - Move the MCP-only specs and their descriptions from
    `mcp/src/tools.rs:69-190` into the tools crate.
  - `pub enum Offer { Chat, Agent }`; `catalogue(offer)`.
  - `READ_ONLY` becomes `tools::read_only(name)`, and `writes(name)` grows
    to cover the new tools.
  - Annotations (`readOnlyHint` and friends) are part of the spec.
  - The MCP host fields (`script`, `review_file`, `before`) are applied by
    `catalogue(Offer::Agent)`, not by the host.
- Tests:
  - `catalogue(Offer::Chat)` equals today's catalogue, so the thread
    `tools_hash` is unchanged (a fixed hash in the test).
  - Names are unique.
  - Every Agent tool takes `script` (except `guide`).
  - Every Agent tool has annotations; `readOnlyHint` is right for each
    (Codex's approvals use it).
  - Each description is under 4,096 characters.
  - `check_args` refuses an Agent-only tool under the Chat offer.

### A2. The file commands move into fsio (`fsio/src/ops.rs`, new; `cli/src/commands.rs`)

- What moves: `add_reviews`, `remove_reviews`, `decide_in`, `edit`,
  `undo_redo` and the write path (`commands.rs:490-846`, `:1059`).
  - They become `fsio::ops::{attach, detach, decide, edit, undo_redo,
    write}`, over an `Opened` the caller already holds under the session
    lock.
  - Each returns the same text as today.
  - `Choice` and `SuggestionRef` move with them.
- The CLI commands become thin wrappers.
- Tests: the CLI's existing tests pass unchanged; a test per op on a
  fixture, run through fsio directly.
- Edge cases: the lock is held from read to write (rule 5); `write` keeps
  MCP's rule that it replaces no `.fdx` but the default output.

### A3a. Callers and `Target::File` as the CLI does it (`chat/src/exec/mod.rs`). After A1.

- `run_call(script, caller: Caller<'_>, call)`.
- `Caller::Agent`:
  - The default review is the single attached review, as the CLI does
    (`commands.rs:1326`).
  - `Target::Own` is refused with the CLI's message.
  - It never creates a thread review.
- `file_review` takes the CLI's behaviour (the table in "Tools: one set,
  two offers"):
  - A missing subfolder inside the script's folder is created, but only
    when there is something to write.
  - A new review is added to the session in memory and written with the
    append. It is never written empty.
  - The id is salted from the resolved path.
- Thread callers keep today's behaviour. `review_file` is still refused
  for them by `check_args`.
- Tests:
  - A `reviews/x.review.txt` with no `reviews/` folder: made, and the
    review is there.
  - An append that fails every item: no file and no folder left.
  - The same id as the CLI gives for the same path.
  - An existing non-review file: refused.
  - All `mcp/src/tools.rs` and CLI `tool` tests for the shared tools, run
    through `run_call(FileScript, Caller::Agent)`.
  - The chat tests unchanged.

### A3b. The six host operations (`chat/src/exec/{mod,file}.rs`). After A2, A3a.

- The `ReviewStore` grows `attach`-many, `detach`, `decide`, `edit`,
  `undo_redo`, `write_script`. Their default answers "not offered here".
- `FileStore` implements them over `fsio::ops`.
- `run_call` dispatches the MCP-only tools to them for `Caller::Agent`.
- Tests: the MCP-only tools' tests from `mcp/src/tools.rs` and the CLI
  tests, over `FileScript`. The results' text is the same.

### A4. Hosts switch over (`cli/src/commands.rs`, `mcp/src/tools.rs`). After A3b.

- The CLI `tool` and the MCP server call the executor. Delete
  `scriptorium_cli::tool` and the MCP crate's copy of the specs.
- Tests: the same tool results as before on the fixtures. Snapshot each
  tool's text before A4 and compare after.

# Phase B — the connector and the link

Ends with: `scriptorium mcp` replaces `scriptorium-mcp`. With no app
running it behaves as today; with a test server on the socket, calls go
there.

### B1. The link (`fsio/src/link.rs`, feature `link`; pure parts in `link::proto`)

- Message types (serde), `compatible(ours, theirs) -> bool`, and the path
  rules: `socket_path(home)`, with the 104-byte rule (the
  `~/.scriptorium/` fallback). These are pure and tested.
- The shell:
  - `Client::connect(timeout) -> Option<Client>` (does the hello);
    `Client::call(&req) -> Result<Reply, Gone>`.
  - `Server::bind() -> Result<Server, Owned>` (flock, stale socket, modes,
    peer uid); `Server::accept()`.
- Tests (Linux, temporary folders):
  - Hello and version mismatch.
  - A stale socket is replaced.
  - A second `bind` gets `Owned`.
  - Modes 0700/0600.
  - EOF mid-call is `Gone`.
  - A long home folder uses the short path.

### B2. `scriptorium mcp` (`cli/src/mcp.rs`, new; `cli/src/main.rs`). After A4, B1, D1.

- What moves: the JSON-RPC loop from `mcp/src/main.rs` (protocol versions,
  `guarded` panics, errors).
- `initialize`, `tools/list` (`catalogue(Offer::Agent)`), `prompts/list`
  and `prompts/get` are answered locally.
- `tools/call`:
  - The link if `Client::connect` works.
  - On `route: files`, or no link: `run_call(FileScript::with_lock_wait(
    LOCK_WAIT), Caller::Agent)`, with busy retries up to `BUSY_FOR`.
  - Not delivered, or EOF after `taken`: the rules above.
  - `where_is_writer` and `guide` never go to the files while the link is
    up. The app answers them even for another script.
- `script` is optional:
  - Over the link the app uses its open script.
  - On the files, a missing `script` is an error: "Scriptorium isn't open.
    Ask the writer for the script's path, or to open it in Scriptorium."
  - In the Agent offer (`tools/src/agent.rs`): drop `script` from
    `required`, and reword `SCRIPT` to "Leave out to use the script open
    in Scriptorium; otherwise its absolute path". Update
    `tests/offer.rs` `every_agent_tool_takes_script` to match. (D1's
    guidance already tells agents to leave `script` out.)
- Remove `crates/mcp` from the workspace.
- Tests:
  - A JSON-RPC transcript with no app.
  - A fake app thread on the socket:
    - routed;
    - `route: files`;
    - a version mismatch notice once;
    - the app gone between calls (not delivered: a write tool runs on the
      files, no "maybe applied");
    - EOF after `taken` on a read tool (run again on the files);
    - EOF after `taken` on a write tool (the error, nothing run again).
  - The files route waits for a held lock instead of failing at once.
  - Notifications (no `id`) get no reply.
- Edge cases:
  - stdout carries only protocol; notes go to stderr.
  - The connector starts with `cwd` `/` and a minimal PATH (it needs
    neither).

# Phase C — the app is the server

C1 and C2 land together. Without C2, the app would answer `route: files`
for the host operations it doesn't offer yet, and agents would see the
same tools behave two ways.

### C1. Link listener and call pump (`crates/app/src/agent_link.rs`, new; `workspace.rs`, `main.rs`). After B1, A3b.

- The link thread, the per-connection threads and the GPUI task, as in
  "In the app: threading".
- The routes:
  - A call whose `script` (canonical path) isn't the open script's, or
    with nothing open: `route: files`.
  - No `script` with a script open: the open one.
- When an agent call changes something, the status bar says "An AI agent
  added 6 suggestions to dialogue-pass", using the same notices as a
  reload.
- The server starts when the app starts and stops when it quits. The
  socket is removed on a clean quit; a crash is covered by `flock`.
- Tests (`TestAppContext`, as `live.rs:267`):
  - Read and suggest calls through a fake connection change the live view.
  - Two connections calling at once both apply, in order.
  - A call while the writer rewrites a line is busy, then applies.
  - The "not open" route.
  - A second app instance doesn't bind.
  - After a write in place (the view replaced), the next call reaches the
    new view, not the files.
  - The files route and the app writing the same script at once (as with
    two app instances): both changes land; the app reloads the other's.
  - A decision is in the session file when the reply is sent.
- Edge cases:
  - The script is closed before the call is taken (the workspace has no
    view): `route: files`.
  - A call arrives while the version is being written in place (the watch
    is paused, `script_view.rs:2097`): busy.

### C2. The live host operations (`agent_panel/live.rs`, `script_view.rs`). After C1.

- `LiveStore` implements `decide`, `edit`, `undo_redo`, `write_script`,
  `attach`, `detach` through `ScriptView`.
- **decide:** today's `ScriptView::decide` takes a `Window` for focus
  (`script_view.rs:1096`). Split it into a window-free core, called by
  both. An agent's decision is an undo step, like the writer's.
- **undo/redo:** not `undo_any` (`script_view.rs:2302`). That one picks the
  review's or the editor's undo by the writer's focus, needs a `Window`,
  and clears `rewriting`. Instead:
  - Add a window-free core: the document's `undo`/`redo` through
    `editor.run`, then `refresh`.
  - It is refused with `EDITING_LINE` while `rewriting` is set.
  - It undoes the last step in the script **whoever made it**, the
    writer's typing included, which is what the file-based `undo` does.
    Decided by the user (decision 10).
  - The tool description says so: "Undoes the last change to the script,
    the writer's or yours. Only when the writer asks."
- **write_script:** not `apply_to_fdx`:
  - `apply_to_fdx` commits the writer's half-done rewrite
    (`commit_rewrite_quietly`).
  - Its in-place write retires and reopens the view inside the pump's
    update while a lease is held.
  - Its busy guard only knows the in-app chat.
  So it follows `mcp/src/tools.rs:280`:
  - With no `output` on the version Scriptorium made (or maybe made):
    refused, "the writer saves it in the app with Write .fdx".
  - Refused while `rewriting` is set.
  - Otherwise it writes `output` (or the default output) from the live
    document, without committing anything, with the same file rules as on
    the files.
- Tests:
  - Each op on a live view.
  - Undo after an agent decision.
  - Undo while the writer rewrites: refused.
  - Write while the writer rewrites: refused, and the rewrite untouched.
  - Write on the made version without `output`: refused.
  - An agent-written `.fdx` is byte-identical to the app's Write .fdx of
    the same document.
- Edge cases:
  - An agent decides the suggestion the card shows: the card moves on, as
    after a key.
  - The agent edits the line the writer is rewriting: refused
    (`EDITING_LINE`).

### C3. Where the writer is (`tools/src/context.rs`, new; `agent_link.rs`). After C1.

- Tool `where_is_writer` (read-only).
- The app fills a `ViewContext { script, pair: Option<(this, other,
  side)>, view: Script | Review(name), card: Option<suggestion n>,
  selection: Option<(para ids, text)>, scene: Option<(number, heading)>,
  visible: (first para, last para), unwritten: bool }`.
- `tools::context::text(&ViewContext)` formats it (pure, tested).
- On the files: "Scriptorium isn't open; nothing is selected. Work on the
  script's path."
- Tests: the formatter's cases; the live fill from a view with a
  selection, a review open and a pair.
  - Remove `where_is_writer` from `NOT_TOOLS` in
    `tools/tests/guidance.rs` (D1 names the tool before it exists).
- Edge cases:
  - A selection across paragraphs.
  - A collapsed selection: the cursor's line.
  - No scene yet (before the first heading).
  - The agent panel's own conversation is not reported.

# Phase D — guidance in the server

### D1. One source of guidance (`tools/src/guidance.rs`, new; `tools/src/guidance/*.md`). After A1.

- **The `guide` tool** (read-only, no `script`): `guide(topic: review |
  translate)` returns the full text.
  - It comes from today's skills (`plugin/skills/*/SKILL.md`, about 10 KB),
    rewritten for the app: the script may be the open one, and there is no
    need to drag paths.
  - This is the guidance every client can reach.
- **Instructions:**
  - The first 512 characters stand alone (Codex). The rules that matter
    are in the first 2 KB (older Claude Code). The whole fits in 4,096.
  - Content: what Scriptorium is; call `where_is_writer` first; call
    `guide('review')` before a pass and `guide('translate')` before
    translating; propose, don't edit; feedback goes in chat; always give
    `review_file`.
  - From today's `INSTRUCTIONS` (`mcp/src/main.rs:27`).
- **Tool descriptions** say the same where it matters, since a client may
  drop the instructions:
  - `suggest_changes` and `suggest_change`: "Before a pass, call
    guide('review')" and the `review_file` naming.
  - `new_version` and `read_rows`: "Call guide('translate') before
    translating".
- **Prompts** (extra): `review_pass` and `translate`, the same text as the
  guide, with arguments `scene` and `language`.
- Tests:
  - Size caps.
  - Every tool name in the guidance exists in `catalogue(Offer::Agent)`.
  - The descriptions named above mention `guide`.
  - The prompts' text equals the guide's.

### D2. Remove the plugin (`plugin/`). After D1, B2.

- Delete `plugin/` (decision 8): no skills, no generated SKILL.md.
  `docs/` explains `/scriptorium:review_pass` in Claude Code.
- Release note: D2 and any release must not ship before B2 and C3. D1's
  guidance tells agents to call `where_is_writer` and to leave out
  `script`, which only work once those land.

# Phase E — Connect an AI agent…

### E1. Config merges (`crates/connect`, new crate `scriptorium-connect`; pure)

- `pub struct Entry { command: PathBuf, args: Vec<String> }`, with name
  `scriptorium`.
- For each of `claude_desktop` (JSON), `claude_code` (`~/.claude.json`,
  user scope), `codex` (TOML, with `toml_edit` so comments and order stay):
  - `state(text, this) -> State::{Missing, Ours { command }, Theirs, Broken(why)}`
    (`this`: the running connector's path)
  - `merge(text, &Entry) -> Result<String, Refused>`
  - `remove(text, this) -> Result<String, Refused>`
- "Ours" means the name is `scriptorium` and the command is `this` or ends in
  `/Contents/MacOS/scriptorium`. The old plugin was loaded with
  `--plugin-dir` and never written to a config, so there is nothing of it
  to repair.
- Rules:
  - Never drop or reorder other keys.
  - Keep `preferences` and every other server.
  - Refuse to touch a file that doesn't parse.
  - An empty or missing file becomes a fresh one.
  - Agent configs are re-serialized (JSON: key order, line endings and
    final newline kept; a duplicate key is refused), an exemption from
    AGENTS.md rule 2, which is for the writer's files. E2 keeps a backup
    first.
- Codex gets `args = ["mcp"]` and `tool_timeout_sec = 300` (long
  translation batches; the default is 60).
- The setup command, as text: `<path> mcp`, quoted for the shell.
- Tests:
  - Each format: an empty file, other servers kept, ours updated in place,
    "theirs" (another program's `scriptorium`) left alone with a message.
  - A broken file refused.
  - Unicode and spaces in the path.
  - JSON key order kept.
  - TOML comments kept.

### E2. Writing configs (`connect`, feature `shell`; CLI `scriptorium connect`). After E1.

- The steps: read, merge, back up (`<file>.scriptorium-backup`, the last
  one kept), write atomically (`fsio`'s atomic write), read back and check.
- Claude Code:
  - If `claude` is found, run `claude mcp add --scope user --transport
    stdio scriptorium -- <path> mcp`, after `claude mcp remove --scope user
    scriptorium` if ours is stale.
  - Look for `claude` on the login PATH, `~/.local/bin`,
    `~/.claude/local`, `/opt/homebrew/bin`.
  - Otherwise merge `~/.claude.json`, and say "Quit Claude Code first" if
    its file changed while we merged (compare before the write).
- Claude Desktop (config-file route; the `.mcpb` route just opens the
  file with Claude):
  - Is it running? Check `NSRunningApplication` for bundle id
    `com.anthropic.claudefordesktop` (*to verify* the id).
  - It rewrites the file from memory, so never write while it runs.
  - The sheet offers "Quit and reopen Claude for me":
    1. Ask it to quit (`terminate`); a chat in progress may make it ask the
       user.
    2. Wait up to 30 s for it to exit. If it doesn't, say "Quit Claude
       (⌘Q), then click Connect again", and write nothing.
    3. Merge and write.
    4. Reopen it with `open -b com.anthropic.claudefordesktop`.
  - If it isn't running, just write.
- **The check after connecting** (every agent): run `<bundle>/Contents/
  MacOS/scriptorium --version` the way the agent will (absolute path, no
  shell, `cwd` `/`). If it fails, the sheet shows why. A Gatekeeper block
  gets the `xattr` fix, or the quarantine-clearing attempt in "Gatekeeper"
  above.
- Rule 5, the CLI: `scriptorium connect claude-desktop|claude-code|codex
  [--print]` and `scriptorium connect --status`.
- Tests: on temporary home folders. A file changed between read and write
  isn't overwritten. The backup exists.
- Edge cases:
  - `~/.codex` missing: create it.
  - A file that is a link (dotfile managers): write through it, not over
    it.
  - Read-only files: say so.

### E3. The sheet and the launch check (`crates/app/src/connect.rs`, new; `menus.rs`, `workspace.rs`). After E2.

- **The menu:** Scriptorium › "Connect an AI agent…". It shows a row each
  for Claude Desktop, Claude Code and Codex.
  - Each row has a status: Connected / Not connected / "Points at another
    copy of Scriptorium" / "Can't read its settings".
  - Each row has a Connect button (Disconnect when connected).
  - Then "Other agents: Copy setup command".
  - After Claude Desktop: "Quit Claude (⌘Q) and open it again."
- **The launch check, one rule:** repair silently only a config whose
  owner isn't running; otherwise offer.
  - What it checks: each agent we connected before (remembered in
    `settings.rs`). The entry needs fixing if it is ours with a command
    whose path no longer exists (the app was moved or renamed), or if it
    has gone missing (Claude Desktop dropped it).
  - If another copy of Scriptorium exists at the path in the config, leave
    it and show the row as "another copy". There is no flip-flopping
    between two copies.
  - Codex is always repaired silently (it reads its config at start).
  - Claude Code is repaired silently through `claude mcp`, which is safe
    with sessions running.
  - Claude Desktop is repaired silently only when it isn't running.
    Otherwise the status bar offers "Claude's Scriptorium setting needs
    fixing: Fix", which goes through "Quit and reopen Claude for me".
  - A `.mcpb` that points at a moved app is offered again the same way.
  - Done silently: one status-bar line, "Updated Codex's settings to this
    copy of Scriptorium."
- **Translocated** (run from Downloads without moving): no writes. The sheet
  says "Move Scriptorium to your Applications folder first."
- Tests: the decision table (pure) for every case above; the sheet in the
  headless scenarios (a screenshot).

# Phase F — the command-line tool

### F1. Install command-line tool… (`crates/connect/src/cli_install.rs`; app `connect.rs`, `menus.rs`). After E1.

- **The menu:** Scriptorium › "Install command-line tool…". It runs only
  when chosen; nothing happens at launch.
- **Pure:** `plan(login_path: &[PathBuf], writable: impl Fn(&Path) ->
  bool, existing: impl Fn(&Path) -> Option<Target>) -> Plan::{AlreadyOk,
  Symlink(dir), AdminSymlink, Conflict(other)}`.
  - Tried in order: `/opt/homebrew/bin`, `~/.local/bin`, `~/bin`, each only
    if it is on the login PATH and writable.
  - An existing `scriptorium` that isn't ours is a Conflict. It is left
    alone, and the item names it.
  - Our own old link (to a moved app) is replaced.
- **Shell:**
  - The login PATH comes from `$SHELL -l -c 'printf %s "$PATH"'` with a 3
    s timeout. GUI apps get launchd's minimal PATH.
  - Then either the symlink, or the admin step:
    `osascript -e 'do shell script "mkdir -p /usr/local/bin && ln -sf
    <bundle>/Contents/MacOS/scriptorium /usr/local/bin/scriptorium" with
    administrator privileges'`. That is one password prompt.
  - *To verify:* that `do shell script` with `administrator privileges`
    still works without extra privacy permissions on macOS 15. If it
    doesn't, show the one-line `sudo ln -s …` command to copy instead.
- **Why `/usr/local/bin` and not `/etc/paths.d`:**
  - `/usr/local/bin` is in macOS's default `/etc/paths`, so the command
    works in any new Terminal window.
  - It is one file, pointing at the CLI alone, where `paths.d` would also
    put `scriptorium-app` on PATH.
  - VS Code does the same.
  - The folder doesn't exist on a fresh Apple Silicon Mac, hence the
    `mkdir -p`.
- **After it runs**, the item says: "Installed. Open a new Terminal window
  and type `scriptorium --help`." or why not.
- The app doesn't repair links at launch. Running the item again fixes a
  link left by a moved app; `scriptorium` failing with "no such file" is
  the sign.
- Tests: the planner's table (Homebrew; `~/.local/bin` on PATH;
  neither; a conflict; our stale link); symlink creation in a temporary
  folder.
- Edge cases:
  - Translocated: refuse, and ask the user to move the app first.
  - The user cancels the password prompt: nothing is changed; say so.
  - Two copies of the app: the last one to install wins.

# Phase G — bundle, release, docs

### G1. Bundle and release (`script/bundle-mac.sh`, `.github/workflows/release.yml`). After B2, D2.

- The bundle already has `Contents/MacOS/scriptorium`. Build it with the
  default features.
- Sign inside-out, not `--deep`: `codesign -s - Contents/MacOS/scriptorium`,
  then the app.
- In `release.yml`:
  - Drop "Build plugin binaries" and "Zip plugin".
  - One zip.
  - New release notes.
- Check on CI: `Contents/MacOS/scriptorium mcp` answers `initialize` from
  the bundle (a one-line JSON-RPC smoke test).
- Edge cases:
  - A universal build runs `lipo` on both binaries.
  - The CLI must not be `CFBundleExecutable`.

### G2. Docs (`docs/testing-on-mac.md` §1 and §7, `README.md`, `AGENTS.md`). After E3, F1.

- §1: move the app to Applications **before** opening it. This avoids
  translocation and keeps configs stable.
- §7 is rewritten as "Connecting Claude (or another AI agent)":
  - Scriptorium › Connect an AI agent… › Claude Desktop, then quit and
    reopen Claude.
  - In Claude, look for "scriptorium" under the connectors/tools menu.
  - Ask "What am I looking at in Scriptorium?" (it calls
    `where_is_writer`).
  - Claude Code and Codex, one line each; "Copy setup command" for others.
  - What happens when the app is closed: Claude still works on the files,
    and the app shows the changes when opened.
  - Translation (from today's §7 subsection), without paths to drag.
  - For people who used the plugin: stop starting Claude Code with
    `--plugin-dir`, and delete the `scriptorium-plugin` folder.
  - If Claude can't start Scriptorium (macOS blocked it), use the `xattr`
    line from §1.
- AGENTS.md layout: `mcp` is removed, `connect` is added, and the CLI line
  gains `mcp` and `connect`.

### G3. End to end (`crates/cli/tests/mcp_link.rs`). After C2, C3, B2.

- Start the app in headless Sway (as the chat scenarios do). Run
  `scriptorium mcp` with a scripted transcript.
- Check:
  - `where_is_writer` names the open script.
  - `suggest_changes` shows on the card within a second.
  - Quit the app: the next call goes to the files.
  - Reopen: the suggestions are there.
- Add it to `crates/app/tests/scenarios/run.sh` as S15.

# Phase H — later: signing, notarization, auto-update (notes)

- **Developer ID + notarization:**
  - Hardened runtime on both binaries.
  - Sign the CLI before the app, then `notarytool submit` and `stapler
    staple` the app.
  - This ends the Gatekeeper steps in §1 and the question of whether
    Claude may spawn the CLI from a quarantined bundle.
  - It needs the Apple Developer account (decision 9: yes, before wider
    use).
- **Sparkle:**
  - It replaces the bundle at the same path, so agent configs and the
    symlink stay valid.
  - An update while an agent is connected: the old app quits, and its
    socket goes. The connector, running from the replaced bundle, keeps its
    open file but its next call reconnects to the new app. If the versions
    differ it uses the files and says so.
  - The agent's next restart picks up the new connector.
  - Needs signing first.
- **The `.mcpb` for Claude Desktop** is decided earlier, by hand test 0.

# Edge cases

- **Two agents at once** (say Claude Desktop and Codex): two connections,
  and calls run one at a time on the UI thread. A review file one wrote is
  re-checked before the other appends (`recheck`, `exec/mod.rs:471`).
- **App and file-based agent at the same time** (a call on the files that
  started before the app opened, or one on another script): both take
  `SessionLock`. The app reloads what the other wrote (its watch), and a
  live call that finds the lock held is busy and retried.
- **The app quits mid-call:** read tools run again on the files; writing
  tools report "may have been applied" and never run twice.
- **The app opens mid-session:** the next call connects. Nothing is lost:
  the file-based calls went through the same files.
- **Two app instances:** the first owns `agent.lock`; the second doesn't
  serve and says so.
- **A version mismatch** (two copies of the app, or a Sparkle update with
  an agent still running the old connector): the hello refuses the link,
  the calls use the files, and the agent is told once.
- **The app was moved after connecting:** the agent can't start the
  connector at all (the path is gone). Claude Desktop shows the server
  failed. Next time the app opens, the launch check repairs the config. The
  sheet's status and §7 tell the user to open the app once, then restart
  Claude.
- **The script open is a version, and the agent names the original:**
  `route: files`. The open view sees the pair's change through its watch.
- **A call names a script inside a link, or a relative path:** canonicalize
  before comparing with the open script; the rules in AGENTS.md rule 6 are
  unchanged.
- **Socket permissions:** only the same user can connect (0600, the 0700
  folder, `getpeereid`). Any process of that user can, as with the files
  today: no new exposure.
- **The writer is mid-rewrite:** proposals on that line are refused, and
  other calls are busy only if the files changed (today's `LiveStore`
  rules).
- **A huge result** (`read_script`): a line-based protocol with no size
  limit, but the app answers in its existing parts (the tool already pages).
- **Claude Desktop rewrites its config and drops ours:** the launch check's
  one rule. If Claude isn't running, it is repaired silently; if it is
  running, the fix is offered through "Quit and reopen Claude for me".

# Work breakdown

All builders and reviewers use the strongest model, except A4 and G1
(mechanical; Sonnet is fine).

| # | Piece | Owns | After |
|---|---|---|---|
| A1 | every spec in tools, offers | `crates/tools` | – |
| A2 | file ops into fsio | `crates/fsio/src/ops.rs`, `crates/cli` | – |
| A3a | callers, `Target::File` as the CLI | `crates/chat/src/exec/mod.rs` | A1 |
| A3b | the six host ops | `crates/chat/src/exec` | A2, A3a |
| A4 | hosts switch over | `crates/cli`, `crates/mcp` | A3b |
| B1 | link: protocol, socket | `crates/fsio/src/link.rs` | – |
| B2 | `scriptorium mcp`, drop `crates/mcp` | `crates/cli/src/mcp.rs` | A4, B1, D1 |
| C1 | app listener + pump | `crates/app/src/agent_link.rs`, `workspace.rs` | A3b, B1 |
| C2 | live host ops (lands with C1) | `agent_panel/live.rs`, `script_view.rs` | C1 |
| C3 | `where_is_writer` | `tools/src/context.rs`, `agent_link.rs` | C1 |
| D1 | `guide` tool, instructions, prompts | `tools/src/guidance*` | A1 |
| D2 | remove plugin | `plugin/` | D1, B2 |
| E1 | config merges | `crates/connect` | – |
| E2 | config writes, CLI `connect` | `crates/connect`, `crates/cli` | E1 |
| E3 | sheet, launch check | `crates/app/src/connect.rs`, `menus.rs` | E2 |
| F1 | CLI install | `connect/src/cli_install.rs`, app | E1 |
| G1 | bundle, release | `script/`, `.github/` | B2, D2 |
| G2 | docs | `docs/`, `README.md`, `AGENTS.md` | E3, F1 |
| G3 | end to end, S15 | `crates/cli/tests`, scenarios | C2, C3, B2 |

The order:
- **First:** hand test 0 (the `.mcpb`), which decides E2's Claude Desktop
  route.
- Then: A1 ‖ A2 ‖ B1 ‖ E1 → A3a ‖ D1 ‖ F1 → A3b ‖ E2 → A4 ‖ (C1 + C2) ‖
  E3 → B2 ‖ C3 → D2 → G1 ‖ G3 → G2. Each piece gets two reviewers who didn't build
it (AGENTS.md); two more review each phase.

# Hand tests (on a Mac, before release)

0. **Early, before Phase E: the manifest-only `.mcpb`.**
   1. Make a `.mcpb` with only a `manifest.json` (`server.type` `binary`,
      `mcp_config.command` the absolute path to
      `/Applications/Scriptorium.app/Contents/MacOS/scriptorium`, `args`
      `["mcp"]`).
   2. Double-click it. Does Claude Desktop install it, and are the tools
      listed?
   3. Replace the app with a newer build at the same path, without
      reinstalling: does it still run, and is it the new one?
   4. Is the on/off switch there?
   5. Try a path with a space.
   If all pass, Claude Desktop uses the `.mcpb`; if any fail, the config
   file.

1. Fresh download, move to Applications, open. Nothing is asked at launch.
   Run Scriptorium › Install command-line tool…: no prompt on a Homebrew
   Mac, one password prompt otherwise. In a new Terminal window,
   `scriptorium --help` works.
2. Connect → Claude Desktop, then quit and reopen Claude. "scriptorium" is
   listed. Open a script in the app and ask Claude "What am I looking at?":
   it names the script and the selected line. Ask for a dialogue pass on
   this scene; suggestions appear on the card within a second.
3. Quit the app and ask Claude for another scene: it works on the files
   (asks for the path, or uses the one from before). Open the app: the
   suggestions are there.
4. Rename the app to "Scriptorium 2.app" and open it: "Updated Claude
   Desktop's settings…". Restart Claude: it still connects. Also check a
   path with a space.
5. Claude Code: Connect, then in a new `claude` session run `/mcp`: the
   server is connected. The `/scriptorium:review_pass` prompt works.
6. Codex: Connect, then in `codex` ask "what's open in Scriptorium": it
   reaches the app (this confirms the sandbox finding). With the app closed
   it writes a review next to a script outside its workspace.
7. Claude Desktop and Codex suggesting at once into two reviews: both land.
8. Open the app from Downloads without moving it: the sheet asks to move
   it first, and nothing is written.
9. After "Open Anyway" only (no `xattr`), Claude Desktop can start the CLI
   inside the bundle (this checks the Gatekeeper point that is to verify).
   Connect's own `--version` check gives the same answer. Try it on macOS
   13, 14 and 15, with and without the app clearing its own quarantine.
10. With Claude Desktop running and a chat open, Connect uses "Quit and
    reopen Claude for me". Claude comes back with the server listed, and
    its other MCP servers and preferences are kept.
11. Quit the app between two Claude calls: the second goes to the files
    without a "may have been applied" error.

# Open questions for the user

1. **The connector: `scriptorium mcp` (the CLI) rather than
   `scriptorium-app --mcp`.** The app still has two executables, as today.
   OK?
2. **Claude Desktop:** a manifest-only `.mcpb` if hand test 0 passes,
   otherwise the config file with "Quit and reopen Claude for me". OK?
