# Plan: the built-in agent (chat threads, API keys, sign-in)

Status: approved with changes (2026-11-14). Nothing built yet.

## Decisions (made by the user, 2026-11-14)

- **v1 is API keys only.** No sign-in yet. ChatGPT plan sign-in and OpenRouter
  sign-in (researched below) come after the chat panel works. So v1 needs only
  two wire protocols: Anthropic Messages and OpenAI Chat Completions (OpenAI
  Responses comes with ChatGPT sign-in).
- **The in-app agent only suggests.** It proposes changes into a review; the
  writer makes every change. No `decide_suggestion` / `edit_paragraph` for it.
- **The agent panel is its own collapsible column** at the far right, after the
  review card (Ctrl/Cmd+L).
- **Threads live next to the script** (`<script>.threads/`), travelling with
  the project; the first-use notice says they contain script excerpts.
- **Functional core, imperative shell** (AGENTS.md rule 1, clarified): a crate
  may do I/O in shell modules around a pure core, so `providers` is one crate
  with a pure core (settings model, price table, request assembly) and a shell
  (keyring, config file, HTTP).

## Why

The roadmap's first way into AI ("a built-in agent using the writer's own
key", see PLAN.md › How AI is accessed). The writer chats with an agent in a
panel inside Scriptorium; the agent reads the script, writes reviews
(suggestions the writer then decides in the review view), and later builds
modules. It must work with nothing else installed: no Claude Code, no terminal.

## What the writer sees

- **Settings › AI accounts.** Add one or more accounts: an *API key* for
  Anthropic, OpenAI, Google Gemini, OpenRouter, Fireworks, or *Custom* (any
  endpoint speaking the Anthropic or OpenAI protocol: base URL + key).
  Later: *Continue with ChatGPT* and *Connect OpenRouter* sign-ins.
  Each account lists its models; the writer picks a default.
- **An agent panel**, its own column at the far right (toggle, Ctrl/Cmd+L): a thread
  list and the open thread. Messages stream in; tool calls show as one-line
  rows ("Read scene 12", "Proposed 8 changes → Dialogue pass") that expand.
  A model picker and a running token/cost count per thread.
- **Suggestions land in a review.** Each thread that proposes changes gets its
  own review file (`<thread title>.review.txt` next to the script), attached
  automatically, so it appears in the sidebar and the writer decides it like
  any other review. The panel links to it.
- **Nothing is sent without the writer asking.** The first message on a
  script to a new account shows once what will be sent where.

## How sign-in works (researched 2026-11-14; sign-in is after v1)

| Account | How | Protocol | Notes |
|---|---|---|---|
| ChatGPT Plus/Pro | OAuth, PKCE, loopback `127.0.0.1` callback, no secret. "Plan usage is available to all open-source partners" | OpenAI **Responses** (`stream: true`, `store: false`) | Self-registers with `client_id=dynamic_agent_client`. Must follow OpenAI's UI rules ("Continue with ChatGPT", "Using ChatGPT plan", Manage usage link). Usage-limit errors arrive mid-stream. |
| OpenRouter | OAuth PKCE, localhost callback on any port, returns a long-lived key | OpenAI Chat Completions | One account reaches most models. |
| Anthropic | API key | Anthropic Messages | |
| OpenAI | API key | Responses (or Chat Completions) | |
| Google Gemini | API key (AI Studio) | OpenAI Chat Completions via `generativelanguage.googleapis.com/v1beta/openai/` | Native Gemini API later only if needed. Google-account OAuth needs a verified Google Cloud app: not now. |
| Fireworks, Together, Groq, local (Ollama, LM Studio)… | API key / none | OpenAI Chat Completions (Custom) | |
| **Claude Pro/Max subscription** | **Not possible in-app.** Anthropic forbids third-party apps from offering Claude.ai login or using plan credentials. | — | Path for these writers: Claude Code over ACP (separate plan), or the Scriptorium plugin in Cowork (built). |

Sources: developers.openai.com/siwc (quickstart, sign-in, models-and-inference,
ui-ux-guidelines); openrouter.ai/docs/guides/overview/auth/oauth;
ai.google.dev/gemini-api/docs/openai; code.claude.com/docs/en/legal-and-compliance.
Re-check the ChatGPT rules before building: the program is new and partly
documented (Chat Completions support, refresh-token lifetime unconfirmed).

Secrets (keys, refresh tokens) go in the OS keyring (macOS Keychain, Linux
Secret Service) via the `keyring` crate; settings (accounts, models, default)
in a plain JSON config file. Never in the project folder.

## Architecture

Two wire protocols cover v1: **Anthropic Messages** and **OpenAI Chat
Completions** (OpenAI, Gemini, OpenRouter, Fireworks, local servers…). OpenAI
Responses joins with ChatGPT sign-in. All stream over SSE.

```
crates/agent  (scriptorium-agent, pure: no I/O)
  thread      messages, tool calls/results, usage; serde to JSON; check_sendable
  protocol/   one module per wire protocol, plus ToolSpec (name, description, schema):
                build_request(protocol, call, thread) -> Request (url, headers, JSON body)
                ReplyParser: SSE lines in -> Events out (text, thinking, tool calls, usage, stop), then a Turn
  turn        the agent loop as a state machine (Run): Send -> reply -> RunTools -> results -> Send…
              (caller does the HTTP and runs the tools; the crate never blocks)
  file        a thread as stored on disk, with its system prompt and tools hash
  presets     known providers' endpoints

crates/tools  (scriptorium-tools, pure; uses the agent crate only for ToolSpec)
  catalogue   the tool catalogue: names, descriptions, JSON schemas (shared with the MCP server)
  prompt      the system prompt, versioned in the repo

crates/providers (scriptorium-providers, does I/O: used by CLI and app)
  accounts    config file + keyring
  oauth       (after v1) PKCE + loopback listener (ChatGPT, OpenRouter), token refresh
  http        streaming POST on a worker thread (ureq: small, sync), feeds SseParser
  models      list models per account
```

**Tools act on the live session, not the file.** In the app, the agent's tools
call the same library functions the CLI uses, but on the `ScriptView`'s open
`Document` and `Session`, so a change shows at once and joins the one undo
history. The CLI/MCP run the same tool catalogue against the session file.
One catalogue (`scriptorium-tools`), two executors:

- `app`: live executor (in-memory document, review attached in the view).
- `cli` / `mcp`: file executor (open session, apply, save, as today).

To get there, the CLI's `Opened` (open script + reviews + session; save) moves
out of `cli/review_cmds.rs` into a library both use, and the MCP server
switches from spawning the CLI to the shared catalogue. Same tools, same
names, everywhere.

**Starting tool set** (the MCP set, plus what chat needs): script_info,
list_paragraphs, read_scene, search, list_reviews, list_suggestions,
suggest_change (into the thread's review). Nothing that changes the script:
the writer decides every suggestion. (The MCP server keeps decide/edit for
outside agents acting on the writer's explicit request.)

**Agents first-class:** `scriptorium chat <script> [--account a] [--model m]
"<message>"` runs the same agent headless and appends to a thread; `scriptorium
accounts [add|remove|list|login]` manages accounts. Everything the panel does is
testable without the GUI.

**Threads** are JSON files in a `<script>.threads/` folder next to the script
(one per thread: title, account, model, messages, usage), so they travel with
the project and the CLI and app share them. Script text sent to a provider is
whatever tools returned; no hidden full-script upload. (A feature film is about
35k tokens, so "read the whole script" is a legitimate tool call when the
writer wants a whole-script pass; it shows the cost first.)

## UI pieces we have to build (GPUI gives none of them)

- Multi-line text input for the composer (reuse ideas from `editor.rs`).
- Streaming message view: Markdown-lite (paragraphs, bold/italic, lists, code)
  with our own small renderer. Zed's `markdown` crate is GPL: don't copy.
- Tool-call rows, the model picker, the accounts settings sheet, the
  "Continue with ChatGPT" button and usage notices OpenAI requires.

## Libraries and Zed (researched 2026-11-14)

The user is fine with GPL but wants Zed code only where it helps. Findings:
- **Provider layer: keep ours.** No Rust crate handles every replay detail we
  need (Anthropic signed thinking, Gemini thought signatures over the
  OpenAI-compatible endpoint, OpenRouter `reasoning_details`): `genai` (best,
  MIT/Apache) loses Gemini signatures there; all need tokio. Zed's provider
  clients build cleanly but would replace ~550 lines and lose the same Gemini
  case. Read `genai` and Zed's `anthropic`/`open_ai` sources for edge cases.
- **Decision (user, 2026-11-14): use gpui-kit via our fork.** `gpui-component`
  is now **longbridge/gpui-kit** (Apache-2.0 code; ~2,500 tests; unstyled
  `gpui-base` behaviour layer + styled components; strong text input, streaming
  Markdown `TextView`, `MessageScroller`). We forked it to
  github.com/nandanrao/gpui-kit, will move the fork to our GPUI pin, build the
  chat panel on `gpui-base` + Input/TextView/MessageScroller with our own look,
  and contribute fixes upstream: optional heavy dependencies, opt-in `Root` and
  global theme (no panics), splitting very large files, GPUI tracking. The user
  wants a shared GPUI component library in the ecosystem and wants Scriptorium
  to help maintain it. (Superseded below: "build our own".)
- **Decision (user, 2026-11-14): follow gpui-kit's GPUI.** Scriptorium moves
  from its Zed git pin to `gpui-pre` (weekly unpatched snapshots of Zed's GPUI
  that gpui-kit publishes and pins exactly; ~1 week behind Zed main) and
  upgrades in step with gpui-kit. Zed plans official weekly GPUI releases on
  crates.io; expect gpui-kit and us to converge on those. Our fork of gpui-kit
  then only carries our contributions, not a GPUI upgrade. The cost of the
  switch is being measured on branch `spike/gpui-pre`.
- ~~**Chat panel: build our own**~~ (thread list, messages, tool rows, model
  picker, composer from our `editor.rs`). Zed's `agent_ui`/`acp_thread`/`editor`
  drag in most of Zed (Project, Workspace, LSP). `gpui-component` (Apache) has
  the widgets but pins an older GPUI snapshot (`gpui-pre 0.3.12`, ~95 commits
  behind our pin), so it can't be mixed in; copying pieces is allowed.
- **Markdown: our own small renderer on `pulldown-cmark`** (paragraphs,
  emphasis, lists, quotes, code). Fallback: a time-boxed spike of Zed's
  `markdown` crate (good streaming API; needs Zed settings/theme globals and
  pulls tree-sitter via `language`).
- **ACP later:** `agent-client-protocol` 5.3 (Apache) has a client side and is
  runtime-agnostic, so it fits GPUI.
- Licence stays MIT OR Apache-2.0 unless we adopt a GPL crate.

## Design review (2026-11-14) — accepted changes

A design reviewer (fresh context) checked this plan and the first code. Accepted:

1. **Review ids must survive appends (bug, fix first).** A review's id is a
   hash of its file's text, and decisions are keyed `review/<id>/<entry index>`.
   When the agent appends a suggestion to a review the writer has partly
   decided, the id changes and earlier decisions lose their status (the text
   changes stay). `save_progress` also reorders entries. Fix in `crates/review`:
   an optional `Id:` header (thread reviews get the thread id; files pasted from
   a chat fall back to the content hash), per-entry keys that don't depend on
   position, `save_progress` keeps file order, and `Session::append_entry` that
   adds a suggestion without changing the id. Existing session files must keep
   working (read old keys). Regression test: decide, append, reopen.
2. **A pure `scriptorium-tools` crate runs the tools.** `call(ctx, name, args)`
   over a `Document` + `Session` returns the result text plus *effects* as
   values (e.g. "append this suggestion to review X"); each host applies them.
   The tool catalogue moves there from `agent` (which then knows nothing about
   scripts). The app applies effects to its live session; the CLI and the MCP
   server go through a shared `Opened` (the CLI becomes lib + bin) and the MCP
   server stops spawning the CLI.
3. **Tools read the live document, not the original file.** Today `dump`,
   `info` and `suggest` read `fdx.to_script()`, so after decisions or edits
   agents see stale text. Read tools use the session's document; suggestions
   carry the paragraph id (optional `PARA:` field, `BEFORE` stays as fallback).
4. **Tool set:** add `read_scene` (one scene as screenplay text, cheaper than
   JSON rows), `read_script` (whole script, reports its size so the cost can be
   shown first), and `suggest_changes` (a batch: a pass makes 50–200
   suggestions). One schema shared by the agent and MCP; MCP adds only
   `script`/`review_file`/`before`. Merge `review_summary` into `list_reviews`.
5. **Concurrency:** the in-app agent writes only its review file, never the
   session file. Session and thread files are written atomically (temp file +
   rename). The app checks the session file before autosave and reloads or
   warns if the CLI changed it.
6. **In v1, not polish:** cancel (abort the request; unanswered tool calls get
   "cancelled" results); cost (cache-write tokens in `Usage`, a price table per
   preset/model); keys without a keyring (env var
   `SCRIPTORIUM_<PROVIDER>_KEY`, else a clear notice; never a plain-text file);
   offline errors as notices with the thread resumable; reasoning effort and
   output limit as per-account settings.

New step order: **0.** review ids + `scriptorium-tools` + live reads (with
reviewers) → **1.** agent core (tests, turn loop) → **2.** providers + CLI chat
→ **3.** panel → **4.** polish.

## Steps (status 2026-11-14)

0. **Step 0** (review ids, live matching, tools crate, CLI lib): built,
   reviewed, committed (c066c8a). Remaining: **P4** the MCP server links the CLI
   lib in-process (stops spawning the binary; uses `add_host_fields`/
   `add_before`); **P5** the app applies the host contract (`write_atomic`,
   suggestion numbers by script position like the CLI, Add review via the
   shared code). See step0-review-ids-and-tools.md.
1. **`crates/agent`**: done (threads, both protocols, origin-gated replay, turn
   driver with retries/cancel/round limit, thread files). Frozen.
2. **`crates/providers` + CLI**: accounts config, keyring, API keys, price and
   model-capability tables, HTTP streaming shell; `scriptorium accounts` and
   `scriptorium chat`. Live smoke test per provider with the user's keys (not
   in CI). **Next after step 0.**
3. **Agent panel in the app: paused (user, 2026-11-14), for a future week.**
   Don't design or build it yet. When resumed: design first (reviewed), on
   gpui-kit (`gpui-base` + Input/TextView/MessageScroller, our own look via a
   theme file), threads list, composer, streaming view, tool rows,
   suggestions into the thread's review, settings sheet, live executor; tested
   headless per AGENTS.md. GPUI is already on `gpui-pre` so gpui-kit can be
   added directly; our fork (nandanrao/gpui-kit) is for upstream fixes.
4. **Polish**: cost display, cancel/stop, retry, errors as notices, a first-use
   disclosure, prompt tuning for review quality (port the language-review
   prompt from `script-review`). Parts that don't need the panel can go with
   step 2's CLI chat.

After v1: OAuth sign-in (OpenRouter, then ChatGPT with its UI rules, as
`scriptorium accounts login …` and buttons in settings).

Out of scope here: ACP (Claude Code, Codex, Gemini CLI inside Scriptorium:
next plan), modules, several agents editing at once.

## Open questions

- Coordination: the panel (step 3) changes `script_view.rs` and
  `workspace.rs`; check for other sessions' work there when it resumes.

## Agent crate: final cleanup before freezing (after round 4 reviews)

1. One sendability check: `check_sendable(protocol, &Origin)`; drop
   `ready_to_send`; target-less check becomes `pub(crate)`; fix the
   `AgentError::NotReady` doc.
2. One way to resend: `start(&mut Thread)` always first applies stored
   results that match pending calls, then cancels the rest (this also fixes
   `resume` discarding stored bad-argument results); remove `resume`.
3. A run ends on Mismatch (clear stored results); recovery is `start`.
4. `input_error_result` private; `triage`/`Triage` `pub(crate)`; keep `cancel`
   and `protocol::is_sent` public.
5. `is_sent` counts only text for User messages; `check` rejects blocks in the
   wrong role (ToolResult in User/Assistant, ToolCall in Tool/User).
6. Cap a provider's retry-after (e.g. 300 s; beyond that, end with the error).
