# Building an app for Scriptorium

An app is a small web page on the script: a scene list, a storyboard, a
character bible, a chart. The writer opens it from Scriptorium's Apps menu, and
it shows in their own browser. It reads the open script live, proposes changes
as suggestions (never edits), keeps its own data, and reloads itself when you
change its files.

Build one when the writer wants a view or a tool they will come back to. For a
one-off answer, answer in chat instead.

You need a way to write files (Claude Code, Cowork, or Claude Desktop with the
Filesystem extension, allowed to write the script's folder). There is no tool
that writes apps.

## Where

An app is a folder: `<script folder>/apps/<name>/`. Call list_apps (or
script_info) to get the exact path. `<name>` is lower-case letters, digits and
`-`, at most 40 characters, e.g. `scene-list`. Apps belong to the folder: every
script in it (an original and its versions) shares them, and an app works on
whichever of them is open.

```
apps/scene-list/
  app.json      required
  index.html    required, always the entry
  app.js, style.css, vendor/ ...   anything else the page loads
  data/         the app's own data (written through the library)
```

`app.json`:

```json
{ "scriptorium": 1, "title": "Scene list", "description": "Every scene, following the writer." }
```

`scriptorium` must be 1. `title` (the menu item) and `description` are
required.

## The page

Put the library first in `<head>`, before your own scripts:

```html
<script src="/scriptorium.js"></script>
```

It gives the page a global `Scriptorium`. Use it for every call; don't post
forms (`<form method=post>`) or call the server by hand.

- `await Scriptorium.call(tool, args)`: run a tool on the open script; returns
  its text. It throws a `ScriptoriumError` in plain words on an error.
- `await Scriptorium.json(tool, args)`: the same, parsed, for the tools that
  answer JSON: list_paragraphs, list_reviews, list_suggestions, list_versions,
  read_rows, suggest_change, suggest_changes. The others answer text.
- `await Scriptorium.paragraphs(filters)`: every paragraph that matches, as an
  array of rows. It does the paging for you.
- `await Scriptorium.suggest(list)`: propose changes (below).
- `await Scriptorium.writer()`: where the writer is now, `{scene, paragraph}`
  (as the `"writer"` event), or null.
- `Scriptorium.data`: the app's own files (below).
- `Scriptorium.on(event, fn)`: `"script"` (the text or the suggestions
  changed: read again), `"writer"` (the writer moved: `{scene, paragraph}`,
  the scene's heading text and the paragraph id; `fn` is also called once
  with where they are now), `"decided"` (the writer
  decided suggestions: `{review, numbers}`), `"open"` (another script was
  opened).

The tools an app may call: script_info, list_paragraphs, read_scene,
read_script, list_reviews, list_suggestions, list_versions, read_rows,
suggest_change, suggest_changes, where_is_writer, list_apps. Leave out
`script`: an app always works on the open script.

## Reading the script

list_paragraphs (or `Scriptorium.paragraphs`) gives rows like this:

```json
{"total": 1, "offset": 0, "more": false, "paragraphs": [{"id": 41, "kind": "Dialogue", "scene": "INT. KITCHEN - NIGHT", "speaker": "ANNA", "text": "Where were you?"}]}
```

- `id` is a number: the paragraph id suggestions take.
- `kind` is the element type: "Scene Heading", "Action", "Character",
  "Dialogue", "Parenthetical", "Transition" and others.
- `scene` is the heading text of the paragraph's scene.
- `speaker` is who speaks, empty outside dialogue. Count dialogue with
  `kind == "Dialogue"`.
- It is paged: 200 rows by default, at most 1,000; call again with `offset`
  while `more` is true. `Scriptorium.paragraphs` does this for you.
- Filters: `scene` matches part of the heading (ignoring case), not a number;
  `speaker` is an exact name (ignoring case); `kind`; `contains` (text).

read_scene takes a scene number or part of a heading and answers text, one
line a paragraph: `¶41 Where were you?`.

To follow the writer, use `Scriptorium.on("writer", fn)`: it tells you where
they are as the page loads, then each time they move. Don't parse
where_is_writer yourself.

## Suggesting

```js
await Scriptorium.suggest([{ paragraph_id: 41, options: ["Where have you been?"] }]);
```

The answer is `{added, suggestions, failed}`. Check `failed`. The library sends
long lists in parts of 200 for you. Items take the same fields as
suggest_changes.

By default an app's suggestions go into `<name> (app).review.txt` next to the
script (e.g. `scene-list (app).review.txt`), created on first use. The tool
descriptions say "always give review_file": that is for you, the agent, not for
apps. Give `review_file` in an app only when the writer names a review.

## Data

The app's own data lives in `apps/<name>/data/`, through the library only:

```js
const notes = (await Scriptorium.data.read("notes.json")) || {};  // null if missing
await Scriptorium.data.write("notes.json", notes);                  // JSON, or a Blob
await Scriptorium.data.blob("img/shot-1.png");                       // a Blob, or null
await Scriptorium.data.list();                                       // [{path, size}]
await Scriptorium.data.remove("notes.json");
```

Prefer JSON. A file is at most 20 MB. Key notes by paragraph id or scene
heading, and expect lines to move or go.

## Rules

- **No internet.** Nothing loads from outside: no CDN, no web fonts, no
  analytics. The browser blocks it. Draw charts by hand with SVG or canvas. If
  you need a library, copy its file into the app folder (e.g. `vendor/`) and
  load it from there.
- **Show script text with `textContent`, never `innerHTML`.** Script text is
  text, not HTML. Build elements with `document.createElement` (and
  `createElementNS` for SVG) and set `textContent`.
- No `eval`, `new Function` or WebAssembly; they are blocked.
- Inline `<script>` and `<style>` are fine, but separate files are easier to
  change.
- Keep the page light: read once on load and again on `"script"`, not on a
  timer.
- The writer's script must be open in Scriptorium. If it isn't, the page says
  so and comes back when it is.
- Keep to a few app tabs at once: browsers open about 6 connections to one
  address, and each app tab holds one.

The library shows errors on the page in plain words (a bar at the top) and
tells Scriptorium the last one.

## Check it

1. After writing the files, call list_apps (or run `scriptorium apps <script>`
   with a shell). Fix any problem it lists for your app.
2. Call open_app with the name to open it in the writer's browser.
3. Call list_apps again: it says whether the page loaded and the last error it
   hit. Fix the error and check again. The page reloads by itself about a
   second after you change its files.

## An example

A whole app: who speaks in which scene, as an SVG grid. The writer's scene is
marked; click a scene number to suggest a new heading. `app.json` as above;
`index.html`:

```html
<!doctype html>
<html><head>
<script src="/scriptorium.js"></script>
<meta charset="utf-8"><title>Characters by scene</title>
</head><body>
<svg id="grid"></svg>
<script>
const S = Scriptorium, NS = "http://www.w3.org/2000/svg", grid = document.getElementById("grid");
let sceneOf = new Map(), here = null;
function el(name, attrs, text) {
  const n = document.createElementNS(NS, name);
  for (const k in attrs) n.setAttribute(k, attrs[k]);
  if (text !== undefined) n.textContent = text;  // script text: textContent only
  return n;
}
async function load() {
  const rows = await S.paragraphs(), heads = [], who = new Map();
  sceneOf = new Map();
  for (const r of rows) {
    if (r.kind === "Scene Heading") heads.push(r);
    sceneOf.set(r.id, heads.length - 1);
    if (r.kind !== "Dialogue" || !r.speaker || heads.length === 0) continue;
    if (!who.has(r.speaker)) who.set(r.speaker, new Set());
    who.get(r.speaker).add(heads.length - 1);
  }
  const names = [...who.keys()], cells = [];
  heads.forEach((h, x) => {
    const n = el("text", { x: 120 + x * 12, y: 10 }, String(x + 1));
    n.onclick = () => rename(h);
    cells.push(n);
  });
  names.forEach((name, y) => {
    cells.push(el("text", { x: 0, y: y * 12 + 24 }, name));
    for (const x of who.get(name)) cells.push(el("rect", { x: 120 + x * 12, y: y * 12 + 14, width: 10, height: 10, "data-scene": x }));
  });
  grid.setAttribute("width", 120 + heads.length * 12);
  grid.setAttribute("height", 14 + names.length * 12);
  grid.replaceChildren(...cells);
  mark();
}
function mark() {
  for (const r of grid.querySelectorAll("rect")) r.setAttribute("fill", Number(r.dataset.scene) === sceneOf.get(here) ? "#e90" : "#36a");
}
async function rename(h) {
  const typed = prompt("A new heading for this scene:", h.text);
  if (!typed || typed === h.text) return;
  const answer = await S.suggest([{ paragraph_id: h.id, options: [typed] }]);
  alert(answer.failed.length ? answer.failed[0].error : "Suggested. The writer decides it in Scriptorium.");
}
S.on("writer", (w) => { here = w.paragraph; mark(); });
S.on("script", load);
load();
</script>
</body></html>
```

If you have the Scriptorium source, two fuller examples are in
`crates/apps/examples/`: `scene-list` (every scene; follows the writer; a
one-line suggestion) and `characters-by-scene` (this grid, with names, counts
and styles).

## What this does and doesn't protect

It stops other websites from reading the script or suggesting through
Scriptorium, and stops an app loading things from the internet by accident.

It does not keep apps apart: any app can read and write every other app's
data and act on the open script. It does not protect against other programs
on this computer. It does not stop an app written to leak (by a link or by
changing its own address). So an app is code the writer trusts, like any
program they run. Tell the writer what you built.
