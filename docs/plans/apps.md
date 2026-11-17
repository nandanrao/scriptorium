# Plan: apps in the writer's browser

Status: planned, not built. Release target **v0.7.0**, on branch
`v7/modules` (from v0.6.0). First review round folded in (2026-11-17).

**The goal:** an agent (Claude Code, Cowork, Claude Desktop with the
Filesystem extension) builds a small web app on the script: a scene list, a
storyboard, a character bible. It is a folder in the project. The writer
picks it from the Apps menu and it opens in their own browser. It reads
the open script live, proposes changes as suggestions, keeps its own data,
and reloads itself when the agent changes its files.

This is the first step of "Modules: agent-built web apps on the script" in
`docs/PLAN.md` (and its "Update after v0.6.0" block). The writer-facing
name is now **Apps**, and the code uses it too. A panel inside the app
window is the later goal; not in this release.

## Decisions (made with the user)

1. **Shown in the writer's own default browser.** No embedded webview, no
   mini browser window. A panel inside the app window comes later.
2. **The running app hosts apps** with a small HTTP server bound to
   `127.0.0.1` only, running while Scriptorium is open. Nothing serves when
   it is closed (no `scriptorium serve`).
3. **One fixed port, 28417**, so the address is the same after every
   restart (reloads and bookmarks keep working). It is below the ephemeral
   ranges (Linux starts at 32768, macOS at 49152), so the OS never hands it
   to an outgoing connection, and no well-known service uses it. No
   fallback ports and no remembered port: if 28417 is taken, apps don't
   run this time, and Scriptorium says so plainly.
4. **One shared origin for all apps.** They can read each other's data and
   act on the open script; accepted, since all are the writer's own.
   Separate origins later if needed.
5. **Apps live in the open script's folder:** `apps/<name>/` with a
   manifest, HTML/JS/CSS, and `data/` for the app's own data. Versioned
   with the project. **Apps belong to the folder**, not to one script: an
   original and its versions in the same folder share them, and an app's
   calls act on whichever of those scripts is open.
6. **The API is the agent tool set**, with the agents' rules: the read
   tools and the suggesting tools; suggestions only, never direct edits;
   AGENTS.md rule 6 for every path. Calls go through **the same path as
   the agent socket**: the app's pump, live on the open script. Plus
   `GET /api/events` (Server-Sent Events: the script changed, the writer
   moved, a suggestion was decided, the app's files changed → reload), and
   the app's data under `apps/<name>/data/` only. A small JS library,
   `/scriptorium.js`, served to every app, wraps these.
7. **Security: simple, and honest about what it isn't** (the user: "Don't
   go crazy on the security. Keep it simple. Don't do things that seem
   secure but aren't. Be honest about the lack of security."). Only:
   - bind `127.0.0.1`;
   - `Host` must be `127.0.0.1:28417` or `localhost:28417` (stops DNS
     rebinding);
   - a per-install token in the opening link, exchanged for a cookie
     (`HttpOnly; SameSite=Lax`), which lasts across restarts;
   - `Origin` checked on `POST` and `PUT`;
   - a CSP that blocks network loads from anywhere but the server (inline
     scripts allowed: agents write them).
   See "What this does and doesn't protect", copied into the docs.
8. **Launching.** An Apps menu lists the open folder's apps and opens one
   in the default browser (`open` on macOS, `xdg-open` on Linux, behind
   one small shell function). Agent tools: `open_app(name)`, so an agent
   that just built one can open it, and `list_apps`. **Hot reload:** the
   app's files change, then about a second of quiet, then an SSE event,
   and the page reloads (the JS library does it).
9. **No special file-writing tool.** Any agent that can write files writes
   the folder. We add `guide('app')` (layout, manifest, the JS API, data
   rules, what not to do), served through the existing `guide` tool and
   named in the instructions and the tool descriptions.
10. **When Scriptorium is gone or elsewhere.** If it quits, the page shows
    "Scriptorium isn't running" (the JS library, on connection loss) and
    reconnects when it is back. If a script from another folder is open,
    the page says "Open <script> in Scriptorium to use this" rather than
    showing the wrong script.
11. **CLI parity** (AGENTS.md rule 5): `scriptorium apps <script>` lists
    apps and checks their manifests. Opening one needs Scriptorium.
12. **Settled after review:** the default review for an app's suggestions
    is `<name> (app).review.txt`; inline scripts are allowed; the cookie
    being sent to other ports on 127.0.0.1 is noted honestly, not fixed.

## What this does and doesn't protect

This text goes into `docs/testing-on-mac.md` and the guide, in these words.

**It stops:**
- other websites open in your browser from reading your script or making
  suggestions through Scriptorium (they don't have the cookie, and the
  `Host` and `Origin` checks turn them away);
- an app loading things from the internet by accident: a library from a
  CDN, analytics, fonts. The browser blocks those loads.

**It does not:**
- keep apps apart. All apps share one address: any app can read and write
  every other app's data, and act on whatever script is open.
- protect you from other programs or other users on this computer. Any
  program running as you can read the token, and anything listening on
  127.0.0.1 may receive the cookie (browsers send cookies to every port of
  a host).
- stop an app written to leak. A page can still send the script away by
  navigating (a link, `window.open`, changing its own address). The block
  covers loads, not navigation.
- keep the token secret from your browser history (the opening link holds
  it).

**So: an app is code you trust, like any program you run.** Use apps you
or your agent wrote, and look at what an agent built before relying on it.

## Choices this plan makes

- **HTTP library: `tiny_http`** (synchronous, small). The app has no async
  runtime of its own (`tokio` is in `Cargo.lock` but not in the app's tree;
  GPUI has its own executor), and the agent link already uses plain
  threads. SSE writes its own head and frames through
  `Request::into_writer()` and flushes after each event. `tiny_http` has
  no limit on header size, so the server checks it (16 KB) itself.
- **A new crate, `scriptorium-apps` (`crates/apps`)** (rule 4), split as
  AGENTS.md rule 1 asks and no further: pure modules (manifest, paths,
  request decisions, events, the list text, the JS library as an asset)
  testable without I/O; a `shell` feature (default) for the folder reads,
  data and the server.
- **Calls reuse the agent link's pump.** The link's `Job` gets a source:
  `Source::Link(Replier)` or `Source::App(name)`. One queue on the UI
  thread for agents, the in-app chat and apps, one call at a time.
  - **Load:** at most 8 app jobs wait at once; more are answered busy at
    once (503). An app call that finds the script busy is answered busy at
    once, not retried for `BUSY_FOR` (10 s): the library retries with a
    short backoff, so a busy app never holds HTTP threads.
- **`Caller::App { name }`** in `chat::exec`, offered **`Offer::App`**, so
  `run_call` itself refuses what an app isn't offered (one check, as for
  agents).
  - `Offer::App` is `Offer::Agent` cut to the app tools (below), with
    `script` removed: an app only ever works on the open script.
  - **Default review:** with no `review_file`, suggestions go into
    `<name> (app).review.txt` in the script's folder, created on first use
    as an agent's new `review_file` is (`SCRIPT REVIEW` header, attached,
    rule 6). An app may name another review under the agent rules.
  - **While the writer rewrites a line,** an app's suggestion goes ahead
    only if its target review is already attached (it then writes only
    that file). A review that would be created and attached is busy.
  - The status bar says "The scene-list app added 1 suggestion to
    scene-list (app)." (`agent_news` with the source's name).
- **Change events by polling, as the disk watch does.** No hooks inside
  `ScriptView`.
  - A workspace task every 250 ms, only while a page listens, takes a
    `Snapshot` of the open view (script path, the document's log length,
    each attached review's item statuses, the writer's place from
    `where_is_writer`'s `ViewContext`); `apps::events::diff(prev, now)`
    gives events. `script` is sent at most once a second, and only after
    the log has been still for 300 ms, so typing in a long script doesn't
    make every page re-read on each key. `writer` at most 4 a second.
  - A server thread every second, only while a page listens, stamps
    `apps/` (path, size, mtime; outside `data/`); a change followed by a
    second with no further change → `reload` for that app.
- **One owner.** The apps server starts unless the agent link answered
  `Owned` (another copy of Scriptorium serves); any other link error still
  starts it.
- **Port taken:** no apps this run. One notice: "Apps can't open: another
  program is using port 28417. Quit it and restart Scriptorium." The Apps
  menu shows that line, disabled; `open_app` answers it.
- **Opening the browser:** `crate::browser::open(url)` runs `open <url>` or
  `xdg-open <url>`, detached, never through a shell. If
  `SCRIPTORIUM_TEST_OPEN_URL` is set it appends the URL to that file
  instead, so tests never open a browser on anyone's screen.
- **Limits kept:** a request body 1 MB (a 200-item `suggest_changes` fits);
  a data file 20 MB (storyboard images fit); 32 event listeners (the 33rd
  gets 503; the library retries); headers 16 KB.
- **The URL names the project folder,** so a page never acts on another
  folder's script: `http://127.0.0.1:28417/p/<key>/<name>/`, where `<key>`
  is 16 hex digits of FNV-1a 64 of the canonical folder path (stable across
  restarts, no dependency). This is for correctness, not security. A
  request whose key isn't the open folder's gets the "not open" page (HTML)
  or `409 not-open` (API).
- **Build errors are visible to the agent that built the app.** The
  library catches JS errors and blocked loads, shows them on the page in
  plain words, and posts the last one to `POST /api/log`. The server keeps,
  per app and in memory, whether the page ever loaded and its last error,
  and `list_apps` reports both. So Claude Desktop, with no shell, can see
  that its app failed and why.

## Names

| Internal | Label (app, docs) |
|---|---|
| crate `scriptorium-apps` (`crates/apps`) | — |
| folder `apps/<name>/`, manifest `app.json` | "an app" |
| `<name>`: the folder name, `[a-z0-9][a-z0-9-]{0,39}` | — |
| manifest `title` | the menu item, the page title |
| `Offer::App`, `APP_TOOLS` | — |
| `Caller::App { name }` | "the scene-list app" |
| `Source::{Link, App}` on the link's `Job` | — |
| `scriptorium_apps::serve::{Server, Host, Running}` | — |
| `crates/app/src/apps_host.rs`, `crates/app/src/browser.rs` | — |
| menu "Apps" | "Apps" |
| tools `list_apps`, `open_app` | same |
| guide topic `app` | same |
| setting `apps-token` | — |
| JS `/scriptorium.js`, global `Scriptorium` | "the Scriptorium library" |
| cookie `scriptorium` | — |
| examples `scene-list`, `characters-by-scene` | same |

# How it works

```
browser tab  http://127.0.0.1:28417/p/<key>/scene-list/
   │  GET pages, /scriptorium.js     POST /api/a/scene-list/call     GET /api/events (SSE)
   ▼
Scriptorium: HTTP accept thread (tiny_http) ─► one thread per request
   │  pure: apps::route::decide(head, state) → Decision
   │  a call: Job { Source::App("scene-list"), … } ─► the agent link's channel (at most 8 waiting)
   ▼
UI thread: agent_link::pump ─► ScriptView::update { run_call(LiveScript, Caller::App{name}, call) }
   ▲
   └─ events tick (250 ms, while a page listens): Snapshot → diff → SSE
```

## An app

```
apps/
  scene-list/
    app.json
    index.html          (always the entry)
    app.js, style.css
    vendor/             (any library, copied in: nothing loads from the internet)
    data/               (the app's own data; JSON preferred)
      notes.json
```

`app.json`:

```json
{ "scriptorium": 1, "title": "Scene list", "description": "Every scene, following the writer." }
```

- `scriptorium` must be `1` (the format). `title` and `description` are
  required strings. Other keys are ignored.
- Problems (bad JSON, a missing field, a bad folder name, no `index.html`)
  are listed by `scriptorium apps`, `list_apps` and the menu (the item
  disabled, with the first problem).

## The app tools (`Offer::App`)

`script_info`, `list_paragraphs`, `read_scene`, `read_script`,
`list_reviews`, `list_suggestions`, `list_versions`, `read_rows`,
`where_is_writer`, `list_apps`, `suggest_change`, `suggest_changes`.

Not offered: `guide`, `new_version`, `open_app`, and the host operations
(`decide_suggestion`, `edit_paragraph`, `undo`, `redo`, `write_script`,
`attach_reviews`, `detach_reviews`).

What the guide must state exactly (from `crates/tools`, checked by a test
against the tool):
- `list_paragraphs` returns
  `{"total", "offset", "more", "paragraphs": [{"id": 41, "kind": "Dialogue",
  "scene": "INT. KITCHEN - NIGHT", "speaker": "ANNA", "text": "…"}]}`.
  `id` is a number. `scene` is the heading text of the paragraph's scene.
  `speaker` is the character speaking (empty outside dialogue blocks; count
  dialogue with `kind == "Dialogue"`). It is **paged**: 200 by default, at
  most 1,000; keep calling with `offset` while `more`.
- Its filters: `scene` matches **part of the heading** (ignoring case), not
  a number; `speaker` is an exact name (ignoring case); `kind`, `contains`.
- `read_scene` takes a scene number or heading text and returns text
  lines `¶<id> <text>`, not JSON. `script_info` is text too.
- `suggest_changes`: `{"suggestions": [{"paragraph_id": 41, "options":
  ["new text"]}]}`, answering `{"added", "suggestions", "failed"}`.

## HTTP

Every response carries `Cache-Control: no-store`,
`X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, and

```
Content-Security-Policy: default-src 'self'; script-src 'self' 'unsafe-inline';
  style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self';
  frame-ancestors 'none'
```

**Checks** (pure, `apps::route::decide`), in order:
1. `Host` is `127.0.0.1:28417` or `localhost:28417` (the port in use), else
   421.
2. `POST` and `PUT` (and `DELETE`) must send `Origin` equal to
   `http://127.0.0.1:28417` or `http://localhost:28417`, else 403.
3. The cookie equals the token, except for `GET /open`. Else 401: a page
   saying "Open this app from Scriptorium's Apps menu, with its script
   open."
4. The route below; anything else 404. No CORS headers, no `OPTIONS`.

**Paths:** the path is percent-decoded first, then split on `/`, then each
part checked (`apps::paths`): no empty part, no `.` or `..`, no part
starting with `.`, no `\`, `:` or NUL. The same for static files and data.

| Request | Answer |
|---|---|
| `GET /open?t=<token>&to=/p/<key>/<name>/` | Token right and `to` a `/p/…/` path: sets `scriptorium=<token>; HttpOnly; SameSite=Lax; Path=/; Max-Age=31536000`, and a page that moves on with `<meta http-equiv="refresh" content="0;url=<to>">`, so the token leaves the address bar. Wrong token: 401. |
| `GET /` | With the cookie: the open folder's apps, as links. Without: the 401 page. |
| `GET /scriptorium.js` | The library. |
| `GET /p/<key>/<name>/<path>` | A file from `apps/<name>/` (empty `<path>`: `index.html`). Not under `data/`. MIME by extension (a small table). `<key>` not the open folder's: the "not open" page, which loads the library to say which script to open. |
| `GET /api/status` | `{"app": "scriptorium", "version", "project": <key or null>, "script": <file name or null>}` |
| `POST /api/a/<name>/call` `{"name", "arguments", "project"}` | `{"text", "is_error"}`; `409 {"error": "not-open"}` when the open folder isn't `project`; `503 {"error": "busy"}`. |
| `POST /api/a/<name>/log` `{"loaded": true}` or `{"error": "…"}` | Kept in memory for `list_apps`. 204. |
| `GET /api/a/<name>/data/` | `[{"path", "size"}]`. |
| `GET` / `PUT` / `DELETE /api/a/<name>/data/<path>` | Read, replace (atomic, parent folders made), remove. Over 20 MB: 413. |
| `GET /api/events` | SSE (below). |

**Data path safety** (shell, every data and static request): `lstat` each
part from the script's folder down, **`apps` itself, `apps/<name>`, and
`apps/<name>/data` included**; any symlink anywhere is refused (404). The
file's resolved path must then be under `apps/<name>/data/` (static files:
under `apps/<name>/`, outside `data/`). Writes go through
`fsio::write_atomic`.

## Events (`GET /api/events`)

```
event: hello    data: {"project": "<key>", "script": "PURPLE LIST.fdx", "version": "0.7.0"}
event: script   data: {}                                  the text or the suggestions changed (≤ 1/s)
event: decided  data: {"review": "…", "numbers": [3, 4]}
event: writer   data: {"scene": "INT. KITCHEN - NIGHT", "paragraph": 41}   (≤ 4/s)
event: reload   data: {"app": "scene-list"}               its files changed, then a second of quiet
event: open     data: {"project": <key or null>, "script": <name or null>}
: ping          every 15 s (finds closed tabs)
```

## The library (`/scriptorium.js`)

```js
const s = Scriptorium;                    // app name and project key come from the URL
await s.call("read_scene", { scene: 3 }) // → text; throws ScriptoriumError (in plain words) on is_error
await s.json("script_info")              // for tools that answer JSON; parses the text
await s.paragraphs({ scene: "KITCHEN" }) // list_paragraphs, every page fetched for you → array of rows
await s.suggest([{ paragraph_id: 41, options: ["…"] }])   // suggest_changes into the default review
await s.data.read("notes.json")          // → parsed JSON, or null if missing; s.data.blob(path)
await s.data.write("notes.json", value)  // JSON, or a Blob
await s.data.list(); await s.data.remove(path)
s.on("script" | "decided" | "writer" | "open", fn)
```

- `reload` for its own app: `location.reload()`.
- **Errors shown in plain words:** an uncaught error or rejected promise
  shows a bar at the top of the page with the message and file:line; a
  blocked load (`securitypolicyviolation`) shows "This app tried to load
  <url> from the internet, which Scriptorium blocks. Copy it into the app
  folder (for example `vendor/`) and load it from there." Each goes to
  `/api/a/<name>/log`. On load the library posts `{"loaded": true}`.
- **Connection lost** (a fetch or the event stream fails): a full-page
  overlay "Scriptorium isn't running. This page reconnects when it's open
  again." Retry every 2 s, then every 10 s; when `/api/status` answers,
  `location.reload()`.
- **Another folder open** (`open`, or a 409): overlay "Open <script> in
  Scriptorium to use this." The name is the one from `hello` on first
  load, kept in `localStorage` by project key; without it, "Open this
  app's script in Scriptorium."
- Busy (503): retried after 0.5, 1, 2 s, then thrown.
- No dependencies; plain ES2020; small enough to read.

# Steps

Each step: one builder, two reviewers (AGENTS.md). Strongest model, except
A5 and A13 (Sonnet).

### A1. Tool specs and offers (`crates/tools/src/{catalogue,agent,lib}.rs`; `crates/app/src/agent_link.rs` stub)

- `Offer::App`; `pub const APP_TOOLS`; `catalogue(Offer::App)` = the Agent
  specs in `APP_TOOLS`, with `script` removed from properties and
  `required`.
- Agent tools `list_apps` (read-only, takes `script`) and `open_app
  { name }` (takes `script`; `readOnlyHint` true, `openWorldHint` true).
  Descriptions point at `guide('app')`.
- `agent_link`: add `open_app` to `APP_ONLY` now, answered by a stub ("Apps
  arrive in a later step"), so `opens_for_writing_is_what_run_call_does`
  and the link's tests stay green. A9 replaces the stub.
- `chat::exec`: `open_app` on the files returns its "needs Scriptorium"
  text (A4 words it fully).
- Tests (`tools/tests/offer.rs`): `catalogue(Offer::Chat)` unchanged (the
  fixed `tools_hash`); `Offer::App` ⊂ `Offer::Agent`; none has `script`;
  `check_args` refuses `script` and every non-app tool under it; caps and
  annotations for the new tools.

### A2. The pure core (`crates/apps`, new crate: `src/{lib,manifest,paths,route,events,list}.rs`, `assets/scriptorium.js`, `assets/*.html`)

- `manifest::parse(&str) -> Result<Manifest, Vec<Problem>>`; `name_ok`.
- `paths`: `project_key(&Path)`; `decode_and_split(raw) -> Result<Vec<Part>,
  Refused>` (percent-decode, then split, then check parts);
  `static_path`/`data_path` on top (a static path refuses `data/…`).
- `route::decide(&Head, &State) -> Decision` (`Head`: method, path, query,
  headers, header bytes; `State`: port, token, open project). Decisions:
  `Refuse(status, page)`, `Exchange { to }`, `Index`, `Library`, `File`,
  `NotOpenPage`, `Status`, `Call`, `Log`, `DataList/Read/Write/Remove`,
  `Events`. `headers()`, `mime(ext)`, the cookie line.
- `events::{Snapshot, Event, Pacer}`: `diff(prev, now)`; the pacer holds
  `script` to ≤ 1/s after 300 ms of stillness and `writer` to ≤ 4/s;
  `quiet(stamps over time) -> Option<reload>` (a second of quiet). SSE
  frame text.
- `list::text(&[Found], &Seen)` and `list::json`: per app its title, its
  folder (`<script folder>/apps/<name>/`), problems, and from `Seen`
  whether its page loaded and its last error.
- Assets: the library and the 401, "not open" and exchange pages.
- Tests: each check (a foreign `Host`, `Host` without port, a rebinding
  name, POST without or with a foreign `Origin`, no cookie, a wrong
  token); `to` outside `/p/` refused; paths (`../`, `%2e%2e`, `%2F`
  inside a part, `a/./b`, `.git`, `data/x` as static, absolute, `\`);
  manifest problems; the pacer (a burst of typing gives one `script` after
  the pause; held writer moves); quiet.

### A3. Reading the folder (`crates/apps/src/shell.rs`, feature `shell`). After A2.

- `scan(dir) -> Vec<Found>` (sorted, at most 50); `read_static`,
  `data_list/read/write/remove` (20 MB), with the symlink rule (lstat
  every part, `apps` and `data` included; resolved under the right
  folder); `stamp(dir)`.
- Tests (temp folders): `data/` itself a link out → refused, nothing
  written; a middle folder (`data/img`) a link → refused; `apps` a link →
  no apps; the file a link → refused; 20 MB + 1 refused with nothing left;
  a broken manifest listed with its problem.

### A4. The executor (`crates/chat/src/exec/mod.rs`, `exec/tests.rs`). After A1, A3.

- `Caller::App { name }` → `Offer::App`; the default review `<name>
  (app).review.txt` through the agent's `file_review` path (created on
  first write, never empty).
- `list_apps` (any caller offered it): `apps::shell::scan(store.dir())`
  and `list::text` (no `Seen` on the files).
- `script_info` gains a first line `folder <absolute script folder>` for
  the Agent and App offers (Claude Desktop needs the folder to write an
  app). The Chat offer's output stays as it is.
- `open_app` on the files: Scriptorium isn't running: "Scriptorium isn't
  running, so the app can't be opened. Ask the writer to open the script in
  Scriptorium, then call open_app again."
- Tests: an app's first suggestion creates and attaches `scene-list
  (app).review.txt` with `SCRIPT REVIEW`, the second appends; host
  operations and `script` refused for an app; `list_apps` on a fixture;
  `script_info`'s folder line; `open_app` on the files.

### A5. CLI `scriptorium apps` (`crates/cli/src/{main,apps}.rs`, `crates/cli/tests/apps.rs`). After A3.

- `scriptorium apps <script> [--json]`: the list; exit 1 if any manifest has
  problems. `scriptorium tool list_apps <script>` works through A4.
- Tests: the fixtures; a broken manifest → exit 1 and its problem; no
  `apps/` → "No apps in <folder>/apps/."

### A6. The server (`crates/apps/src/serve.rs`, `tiny_http`). After A2, A3.

- `Server::start(port, token, host: Arc<dyn Host>) -> Result<Running,
  Taken>`; an accept thread, a thread per request; the header-size check;
  `route::decide`, then files, data, `host.call`, the log.
- `trait Host: Send + Sync { fn open(&self) -> Option<Open>; fn call(&self,
  app: &str, project: &str, call: Call) -> CallAnswer }`
  (`Done { text, is_error } | NotOpen | Busy`).
- SSE listeners (32), each a thread with a small bounded channel (a slow
  one is dropped), frames flushed per event, pings every 15 s;
  `Running::{broadcast, listeners, seen, stop}`.
- The stamps thread (1 s, only while listened to) → `reload` after a
  second of quiet.
- Tests over real sockets with a fake `Host` (`ureq` or a raw
  `TcpStream`), on a free port:
  - The exchange: `/open` sets the cookie and refreshes to `to`; the page
    with the cookie 200, without 401; `/` with and without.
  - Refusals end to end: foreign `Host`, POST without `Origin`, foreign
    `Origin`; a 17 KB header → 431; a 1 MB + 1 body → 413 and `host.call`
    never called.
  - The CSP on every answer, the 401 page included.
  - `..`, `%2e%2e` → 404; a linked `data/` → 404.
  - Data round trip; `data/` not served as static.
  - `/log` then `seen()` reports it.
  - SSE: a broadcast arrives within 1 s; edits to a file three times in
    two seconds give one `reload`, a second after the last; the 33rd
    listener 503.
  - The port held by another listener → `Taken`.

### A7. The app hosts apps (`crates/app/src/{apps_host,agent_link,workspace,main,settings}.rs`). After A4, A6.

- `agent_link`: `Job.source: Source::{Link(Replier), App(String)}` plus the
  project key for app jobs; `run_request` takes the source (an app job
  answered busy goes back at once); `run_on_view` picks `Caller::App`,
  writes `taken` only for links, checks the project again on the UI
  thread, and applies the rewriting rule for apps (attached review only);
  `agent_news` names the app.
- `apps_host.rs`: start after `agent_link::start` unless `Owned`; the
  `Host` (a count of waiting app jobs, at most 8); the events tick (250
  ms, only with listeners, through the pacer); stop on quit; the "port
  taken" notice. `OpenScript` gains the folder and its key.
- `settings.rs`: `apps_token()` (32 random bytes from `getrandom`, hex, made
  on first use, file mode 0600; this run only if it can't be saved).
  `SCRIPTORIUM_TEST_APPS_PORT` replaces 28417 (tests and scenarios must
  not meet a real copy).
- Tests (`TestAppContext`, running `handle` by hand as the link's tests
  do):
  - A read call answers from the live view; a suggestion shows on the
    card, lands in `scene-list (app).review.txt`, and the status bar names
    the app.
  - While the writer rewrites a line: a suggestion into an attached review
    applies; one that would create its default review is busy (503).
  - A call for another folder's key: not open.
  - Nine app jobs waiting: the ninth is busy at once.
  - An agent call and an app call interleaved: both apply, in order.
  - The tick: an agent's decision → `decided`; moving the selection →
    `writer`; ten edits in a second → one `script`; another script opens
    → `open`.

### A8. Opening apps (`crates/app/src/{browser,agent_link}.rs`). After A7.

- `browser::open(url)`: `open`/`xdg-open`, detached; the
  `SCRIPTORIUM_TEST_OPEN_URL` hook.
- `open_app` in `agent_link`, replacing A1's stub, answered by the app
  (checked against the offer; routed by `script` as any call). It opens
  `http://127.0.0.1:28417/open?t=<token>&to=/p/<key>/<name>/` and answers
  "Opened Scene list in the writer's browser." (never the token). Other
  answers:
  - no script open: "No script is open in Scriptorium. Ask the writer to
    open <script, if given>, then call open_app again.";
  - a script from another folder open (or the `script` named isn't the
    open one): "Scriptorium has <open script> open, not <named>. Ask the
    writer to open <named> in Scriptorium, then call open_app again.";
  - port taken: the notice's words;
  - an unknown app, or one with problems: the problem, and "check it with
    list_apps".
  (Scriptorium not running is A4's answer on the files.)
- `list_apps` through the app adds each app's `Seen` (loaded, last error).
- Tests: `open_app` through a fake connection writes the URL to the hook
  file; each refusal text; `list_apps` shows a logged error.

### A9. The Apps menu (`crates/app/src/{menus,workspace}.rs`). After A7. Parallel with A8.

- `menus::definition(&AppsMenu)`: a menu "Apps" with an item per good app
  (its `title`), apps with problems disabled, "No apps in this script's
  folder" when none, the "port taken" line when there is no server;
  action `OpenApp { name }` (through A8's `browser::open`; built after A8
  or with a stub opener).
- macOS: `cx.set_menus` again when the list changes (the stamps thread sees
  `apps/*/app.json` come, go or change; another script opens). Linux: the
  File dropdown gets the same items under an "Apps" heading.
- Tests: the definition with apps, with problems, with none, with no
  server; `dropdown_has_…` updated.

### A10. Guidance and examples (`crates/tools/src/guidance.rs`, `guidance/app.md`, `crates/apps/examples/{scene-list,characters-by-scene}/`). After A1, A2.

- `Topic::App` ("app"). `guide('app')` covers, briefly:
  - what an app is and when to build one;
  - where: the exact folder, `<script folder>/apps/<name>/` (from
    `script_info` or `list_apps`); it needs a way to write files (Claude
    Code, Cowork, or Claude Desktop with the Filesystem extension);
  - `app.json` and `index.html`;
  - the library, with the exact row shapes and paging above, and
    `Scriptorium.paragraphs` doing the paging;
  - suggestions go to the default review unless the writer names one;
  - data: JSON in `data/` through the library;
  - **no internet**: no CDN, no web fonts; draw charts by hand with SVG or
    canvas; any library must be copied into the folder;
  - **show script text with `textContent`, never `innerHTML`**;
  - check: `scriptorium apps <script>` (with a shell) or `list_apps`
    (without), `open_app`, then `list_apps` again to see whether the page
    loaded or what error it hit;
  - "What this does and doesn't protect", short.
- `INSTRUCTIONS`: "To build an app (a small web page on the script), call
  guide('app') first." Caps still hold.
- Examples: `scene-list` (every scene; follows the writer; a one-line
  suggestion) and `characters-by-scene` (an SVG grid of who speaks in which
  scene, from `Scriptorium.paragraphs()`), both linked from the guide and
  used by S16.
- Tests (`tools/tests/guidance.rs`): tools named exist; each
  `Scriptorium.*` named exists in `scriptorium.js`; the `list_paragraphs`
  row keys in the guide equal the tool's real output on a fixture; caps;
  prompts unchanged.

### A11. End to end, S16 "apps" (`crates/app/tests/scenarios/run.sh`, `s16_apps.sh`). After A5, A8, A9, A10.

As S15: a scratch copy of the fixture script, headless Sway,
`XDG_CONFIG_HOME`/`XDG_STATE_HOME`/`XDG_RUNTIME_DIR` in the scratch
folder, `SCRIPTORIUM_TEST_APPS_PORT=39417`,
`SCRIPTORIUM_TEST_OPEN_URL=$WORK/opened`.

1. Copy both examples into `$WORK/apps/`; `scriptorium apps
   $WORK/film.fdx` lists them, exit 0.
2. Start the app. `scriptorium mcp` transcript: `list_apps` (names the
   folder), `open_app {"name": "scene-list"}`. `$WORK/opened` holds one
   URL.
3. curl without a cookie: `/p/<key>/scene-list/` and `/` → 401.
4. curl the opened URL with a cookie jar: 200, `Set-Cookie` with
   `HttpOnly` and `SameSite=Lax`, the refresh; then the page → 200 with
   the CSP.
5. Refusals: `Host: evil.example:39417` → 421; POST with `Origin:
   http://evil.example` → 403; POST without `Origin` → 403;
   `/p/<key>/scene-list/%2e%2e/app.json` → 404.
6. `POST …/call` `read_scene {"scene": 1}` → the scene.
7. `curl -N /api/events` in the background (with a time cap) → `hello`.
8. `POST` `suggest_changes`, one item → `"added": 1`. Screenshot: the
   card shows it; the sidebar lists "scene-list (app)". The event log has
   `script`.
9. Touch `apps/scene-list/index.html` → `reload` after about a second.
10. `PUT …/data/notes.json`, `GET` it back; `…/data/%2e%2e/x` → 404.
11. **A real browser, if `geckodriver` and `firefox` are installed** (they
    are on the dev machine; otherwise the step prints SKIP and it is hand
    test 3): `firefox -headless` with a fresh profile, DISPLAY and
    WAYLAND_DISPLAY unset, driven over WebDriver with curl. Load the opened
    URL; within 5 s the page shows the first scene heading. Load a test
    app `csp-probe` (fixture: a `<script src="https://example.com/x.js">`):
    `list_apps` reports its error naming the URL. Quit Scriptorium: the
    scene-list page shows "Scriptorium isn't running".
12. Start Scriptorium again: curl with the old cookie jar works (no
    `/open`); the browser page reloads by itself.

### A12. Docs (`README.md`, `AGENTS.md`, `docs/testing-on-mac.md`, `docs/PLAN.md`, `.github/release-notes.md`). After A11.

- README: apps in a paragraph; the `apps` command.
- AGENTS.md: layout gains `apps/ scriptorium-apps`; app layout gains
  `apps_host.rs` and `browser.rs`; the scenarios line names S16; rule 6
  gains app data paths (no links anywhere under `apps/`).
- `docs/testing-on-mac.md`: "What's new in 0.7.0"; a section "Apps":
  what you need (the Scriptorium connection and a way for the agent to
  write files: Claude Code, Cowork, or Claude Desktop with the Filesystem
  extension); asking for one ("Build me an app that…"); opening it from
  the Apps menu; what the page does when Scriptorium is closed or another
  script is open; where its data is; if port 28417 is taken; **"What this
  does and doesn't protect"** in the words above. Then "Please try
  (0.7.0)": the hand tests, for the writer.
- PLAN.md: a "v0.7.0" entry under Decided; the modules section notes the
  rename to Apps, marks the browser step built, links this plan.
- Release notes for 0.7.0.

### A13. Release (`Cargo.toml`, `Cargo.lock`). After A12.

- Version 0.7.0. The bundle needs nothing new.
- Checks (fmt, clippy, test) and scenarios S1–S16.

**Order:** A1 ‖ A2 → A3 ‖ A10 → A4 ‖ A6 → A5 ‖ A7 → A8 ‖ A9 → A11 → A12 →
A13. (A10 needs only A1 and A2 and may run beside A3–A9.)

# Hand tests (on a Mac, before release)

1. **Safari** as the default browser. Copy `scene-list` into a script's
   folder. Apps › Scene list: Safari opens it; the address bar has no
   token. Click around in Scriptorium: the page follows the scene within
   a second. Suggest from the page: it shows on the card.
2. Quit Scriptorium: "Scriptorium isn't running." Open it with the same
   script: the page comes back on its own. Reload by hand: still works.
   Bookmark it, quit Safari and Scriptorium, open both, use the bookmark:
   it works.
3. **Chrome** as the default browser: 1 and 2 again. Also (if S16's
   browser step was skipped): an app loading a CDN script shows the
   "copy it into the app folder" bar, naming the URL.
4. Open a script from another folder: "Open <script> in Scriptorium to use
   this." Open the first again: it comes back.
5. **Claude Desktop + the Filesystem extension**, the screenwriter's way:
   "Make me an app that shows which characters are in which scene." Claude
   calls `guide('app')`, writes `apps/…` in the folder `list_apps` gave,
   calls `open_app`, and the browser opens it. Break it on purpose ("load
   Chart.js from a CDN"): `list_apps` shows the blocked load, and Claude
   fixes it.
6. **Claude Code:** build an app, run `scriptorium apps`, `open_app`; ask
   for a change: the open page reloads by itself once.
7. A script folder with spaces and accents (`~/Documents/Guión final/`):
   everything above works.
8. **macOS folder access:** a script in Documents, Desktop or iCloud
   Drive. Note any prompt Scriptorium or Claude gets the first time and
   whether apps work after allowing it.
9. Another program on port 28417 (`python3 -m http.server 28417`) before
   opening Scriptorium: the notice, the disabled Apps line, `open_app`
   says why. Quit it and restart Scriptorium: apps work.
10. **Typing in a long script with an app open** (the full PURPLE LIST):
    typing stays smooth; the page updates after a pause, not on every key.
11. Two copies of Scriptorium open: only the first serves apps.

# Edge cases

- **The script is written in place** (the view replaced): app calls are
  busy for that moment; the library retries.
- **An agent and an app at once:** one queue on the UI thread.
- **An agent writes many app files:** one `reload`, a second after the
  last write.
- **`localhost` vs `127.0.0.1`:** both pass the `Host` check, but links use
  `127.0.0.1`; a page typed as `localhost` has no cookie and shows the 401
  page.
- **The settings folder isn't writable:** a token for this run only; pages
  need the menu again after a restart. Logged.
- **An app named like a route** (`api`, `open`, `p`): names only appear
  inside `/p/<key>/`, so no clash.
- **The writer edits `data/` by hand while a page is open:** no event; the
  page sees it on its next read.

# As built

(Left empty until built.)
