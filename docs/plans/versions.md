# Plan: versions (translation first)

Status: plan, final draft after three review rounds, 2026-11-17. Not built.
Built on the `v4/chat` branch (the chat panel); Phases A and B ship
together as **v0.5.0**, with the chat panel. Design record (reasoning and
mockups): the "Versions" brainstorm page of 2026-11-17 (round 4).

**The goal** (the user, 2026-11-17): something that works and is robust,
even if simple. Two versions next to each other, stable paragraph ids, and
a way for an AI agent to make the translation and for a person to review
it. Every mechanism below is measured against that.

## Decisions (made with the user, 2026-11-17)

- **One concept: versions.** A version is any `.fdx` lined up against
  another: a translation, an older draft, a co-writer's copy. A version has
  an optional language. No type, crate, module, key or file format is named
  after translation; "Translation" appears only in labels.
- **The view is the review view, unchanged,** plus a second, read-only
  script lined up row by row on the left of the open one. Suggestion dots
  only on the right. A status dot per row between the columns. Two columns
  now; the model allows N.
- **A version is an `.fdx` you open.** One window, one script. A sidebar
  section "Lined up on the left" shows the version found through the links
  file next to the script, plus "Nothing". The header names both and has
  Swap. File gains "New version…". Errors are status-bar notices.
- **Links:** one readable sidecar file per pair. One row per line,
  `a ids | b ids`. Rows can be 1:N, N:1 or one-sided, and never cross. Each
  paragraph carries its text as of the last sync.
- **Placeholders:** a new version in another language is the original's
  structure, spliced from its bytes: scene headings and character names
  copied, everything else blank with the same element type, linked 1:1.
- **Suggestions are ordinary suggestions** in an ordinary review on the
  version. Agents give one option when the line is obvious, 2–3 only for
  real choices, each with a short note.
- **No app UI for changing links** at first: hand edits, then the CLI.
- **Write .fdx overwrites a version Scriptorium made**, and only that.
  Every other file keeps `<name> (scriptorium).fdx`.
- **One machine.** Both files of a pair live in one folder on one computer.
  No sync folders (iCloud Desktop/Documents, Dropbox) or two people on the
  pair; revisit the hash checks and locking if that changes.
- **The partner uses Claude on their own Mac.** v0.5.0 ships the new tools
  and the translation skill in the Mac plugin zip, with steps they can
  follow.
- **Undo ends at Write .fdx for a version** (accepted 2026-11-17): the
  version's session starts again from the written file; undecide still
  works after it.
- **Write .fdx is refused while a pending suggestion's line was split or
  joined** (accepted 2026-11-17); the message names those suggestions.

Defaults this plan takes (the user may override):
- Scene headings are placeholders: they get suggestions. The brief says how
  to treat INT./EXT. and times of day.
- Character cues are copied and count as in sync. Extensions typed in the
  cue ("(V.O.)") are copied with it; Claude may still suggest on a cue row
  (generic cues like "WOMAN"). "(CONT'D)" and "(MORE)" are not text: Final
  Draft adds them from the file's settings (`<DialogueBreaks
  AutomaticCharacterContinueds="Yes" … DialogueTop="(CONT'D)">` in the
  PURPLE LIST), copied untouched; the translator changes them once in
  Final Draft's Mores and Continueds settings.

## Review outcome (2026-11-17)

**Round 1.** Write in place for versions Scriptorium made; a thin Phase A
first; "links" as the internal name; steps split; crash ordering, CLI
set-aside, empty-line refusals, join/split wording, non-editable kinds.

**Round 2** (the user asked for the simpler shape). "In sync" derived from
stamps and decided suggestions, not stored (no overlays, hooks or
`Renumber` changes). `[empty]` suggestions matched by row content. One
re-find for every "file and links disagree" case. Write in place app-only.

**Round 3** (design and correctness, fresh contexts; plus the move to
`v4/chat`). What changed, and why:
- **v0.5.0 on `v4/chat`.** Phases A and B ship together. Every reference
  re-checked against this branch. Shell code goes in `scriptorium-fsio`
  (the app doesn't depend on the CLI crate here, `crates/app/Cargo.toml`).
  The in-app chat agent gets the new tools through the shared catalogue.
  Phase B's layout extends the chat branch's `agent_panel::columns`.
- **Every `ROW:` suggestion is placed by row content alone** (C1, C7):
  never by `PARA` or text matching. The resolver checks both sides of the
  row; `PARA` only breaks ties between identical rows, which are
  interchangeable.
- **Decisions survive a Final Draft save of the version** (C2): progress is
  written into the review files on every save of the version's session.
- **Re-find never treats two blanks as equal text** (C3); blanks pair by
  position only.
- **New lines in the original get rows of their own** (C4); D3 is only for
  the version's own lines.
- **Updates go into a new dated review** (C5), since a decided line can't
  take a second suggestion in the same review.
- **The links file is used only against the bytes it describes** (C6).
- **"The original's text" is its file** (C8), everywhere.
- **`needs` skips rows with any suggestion** (C9).
- **Status order** puts decided suggestions before the placeholder test
  (D1). **Language names** (D2). **The resolver is in A2** (D3). **Write in
  place owns the session write** (D4). **Partner's first day** in the docs
  and skill (D5). **Cut from A:** `links sync`/`unsync` (to B3), the
  session's `links` field (to B1), joining rows after a merge (D6). **An
  end-to-end test step**, and hand test 1 right after A3 (D7). **Steps
  split** (D8). **Clarifications** (D9). **Every builder and reviewer uses
  Opus** (D10, the user).

## Names

| Internal | Label (app, docs, tool descriptions) |
|---|---|
| crate `scriptorium-versions` (`crates/versions`); shell `scriptorium_fsio::versions` | — |
| `Links`, links file `*.links.txt`, `LinksCtx` | "links file" |
| member `a`, `b`; the *made* member (`created: version new`) | the file's name and language; "the version" |
| `this` / `other` (tools: the script called on / the other member) | "this version" / "the version lined up with it" |
| left / right (app only) | "lined up on the left" |
| `Row`, `cells`, `Stamp` | "row", "last in sync" |
| `RowStatus::{InSync, ThisChanged, OtherChanged, BothChanged, Blank, OneSided}` | in sync · Español changed · English changed · both changed · not yet translated (same language: "blank") · nothing on this side |
| review fields `ROW:`, `ROW TEXT:`; marker `[empty]` | — |
| tools `list_versions`, `new_version`, `read_rows` | same |
| CLI `version new`, `versions`, `rows` | same |
| the agent's review files | "Translation from English", then "Translation from English · updates 2026-11-20" |

Language labels come from a small table in `versions` (about 20 codes: en
English, es Español, fr Français, de Deutsch, it Italiano, pt Português,
ca Català, …; an unknown code shows as itself). A pair with no language
for the original shows the original's file stem instead.

# Data

## The links file

Both files and the links file are in one folder. `version new` writes the
version next to the original. Named `<b stem>.links.txt`, e.g.
`PURPLE LIST.es.links.txt`. A script's pair is the `*.links.txt` in its
folder whose header names it; opening either file finds it. One pair per
script for now: a second is refused with the first's name.

```
SCRIPTORIUM LINKS v1
Id: lk-5k2m9q
a: PURPLE LIST.fdx
a language: en
a hash: 9f2c4e1a7b3d5e60
b: PURPLE LIST.es.fdx
b language: es
b hash: 0c4d2e8f11aa9b03
b created: version new

# One row per line: a ids | b ids. Under it, one line per paragraph: its
# kind, then its text when the row was last in sync ("=") or when it was
# made a placeholder ("~"). Neither: never in sync.
---
14 | 14
  a14 heading = INT. FERRY TERMINAL - NIGHT
  b14 heading ~ INT. FERRY TERMINAL - NIGHT
15 | 15 16
  a15 action = Rain hammers the glass. LUCÍA (40s) counts coins on a bench.
  b15 action = La lluvia golpea el cristal.
  b16 action = LUCÍA (cuarenta y tantos) cuenta monedas en un banco.
16 | 17
  a16 character = TOMÁS
  b17 character = TOMÁS
17 | 18
  a17 dialogue = You're not getting on that boat.
  b18 dialogue ~
18 |
  a18 dialogue = Watch me.
19 | 19
  a19 "End of Act" = END OF ACT ONE
  b19 "End of Act"
```

Rules:

- Header lines are `key: value`, keys case-insensitive, before the first
  `---`. `Id:` is a stable token (`[A-Za-z0-9._-]{1,64}`), made as review
  ids are (`new_review_id`, `crates/fsio/src/opened.rs:388`).
- `a:`/`b:` are plain `.fdx` file names in the folder (no `/`, `\`, `:`,
  no leading `.`), different even ignoring case. `<m> language:` is
  optional. Exactly two members in v1 ("one pair per links file for now").
  A single letter alone or with a member field (`x:`, `q hash:`) is a
  reserved member key: an unknown letter is refused, not kept. An empty
  key is refused.
- `<m> hash:` is `sessionfile::hash` (`review/src/sessionfile.rs:352`) of
  the bytes the ids refer to. Ids are positions in that file
  (`FdxFile::to_script`, `fdx/src/lib.rs:281`). **The links file only ever
  names file ids.**
- `<m> created: version new` marks the file Scriptorium made. It is the
  one switch for "which side is written": written in place, counted for
  `needs`, and the only side `row` suggestions go to. Phase E sets it per
  pair.
- Row lines: ids ascending within a side, rows in order on both sides (no
  crossing), each id once; a side may be empty, not both. Every listed
  paragraph has exactly one text line under its row:
  `  <member><id> <kind>`, then optionally `= <text>` or `~ <text>`, one
  line, normalized (below), possibly empty. No `=`/`~`: never in sync.
  The kind is stored so re-find can pair same-kind lines after Final Draft
  has rewritten the file and the old bytes are gone. Kinds are `heading`,
  `action`, `character`, `parenthetical`, `dialogue`, `transition`, `shot`,
  `general` (case-insensitive); any other style name (`ElementKind::Other`)
  is written in double quotes, with `\"` and `\\` escaped.
- A row whose ids all disappear (re-find, `after_write`) is dropped: an
  all-empty row is refused on write.
- `#` and blank lines are comments; unknown header keys are kept on write.
  A parse error is reported with its line; nothing rewrites a file that
  doesn't parse, and the pair shows as broken.
- Written whole through `write_atomic` (`crates/fsio/src/lib.rs:32`),
  under the links lock.
- A file named in `a:`/`b:` that is missing: the message gives the fix
  ("edit line 3 of PURPLE LIST.es.links.txt to the new name") and names any
  `.fdx` in the folder with the recorded hash.
- **Used only against the bytes it describes (C6).** Every reader (CLI,
  MCP, chat agent, app) uses a member's rows only when that member's
  `hash:` equals the bytes it has open. Otherwise it re-finds (if it is the
  opener, below) or says "PURPLE LIST.es.fdx changed outside Scriptorium;
  reopen it" and resolves no rows. An app window keeps what it showed last,
  with that notice.
- A file starting with `SCRIPTORIUM LINKS` is refused as a review: Add
  review, a dropped file (`app/src/workspace.rs:182-185`,
  `script_view.rs:647-651`) and `reviews add` say "this is a links file,
  not a review". (Agents are already refused anything that doesn't start
  with `SCRIPT REVIEW`, `file::is_review_text`, `file.rs:671`.)
- N members later: `c:` and `ids | ids | ids`; the model is already
  `members: Vec<Member>`, `cells: Vec<Vec<ParaId>>`.

## Texts

**The original's text is its file (C8).** Status, `ROW TEXT`, the resolver
and stamps read the non-made member from its `.fdx` only, never from its
session. So an English edit made inside Scriptorium shows in the pair only
once it is in the English file. `version new` refuses while the original's
session has changes not in its file (any event): "PURPLE LIST.fdx has
changes in Scriptorium that aren't in the file. Write .fdx and save the
written file over PURPLE LIST.fdx, or undo them." The version's own text
is its live document (the session applied), as everywhere else.

`stamp_text(text, caps)` (`versions::links`) = `core::text::normalize` (`core/src/text.rs:5`:
whitespace, quote and dash style, ellipsis), upper-cased for kinds shown in
capitals (`FdxFile::is_caps`, `fdx/src/lib.rs:268`, passed to `versions`
as a closure). So a Final Draft re-save or smart quotes never count as a
change. No NFC normalization now; the hand test checks accented text after
a Final Draft save.

## Review files

Two fields and a marker (`review/src/file.rs`; `FIELD_RE` at `:95` gains
`ROW` and `ROW TEXT`, capitals only):

```
---
BEFORE: [empty]
PARA: 18
AFTER: Mirá cómo lo hago.
AFTER: Ya vas a ver.
WHY: Keeps her defiance; 2 is softer.
ROW: lk-5k2m9q
ROW TEXT: Watch me.
```

- `ROW: <links id>`: a suggestion for a row of that pair.
- `ROW TEXT:` one per paragraph on the other side of the row, in order, its
  normalized text then. An empty paragraph is `ROW TEXT:` with nothing
  after it; no `ROW TEXT:` lines means the other side was empty.
- `BEFORE: [empty]` is one blank line. Valid only with `ROW:`. In a block,
  a typed blank line is `DIALOGUE:` with nothing after it.
- No header change. Builds up to 0.4 read `ROW` lines as continuations of
  `WHY` and `[empty]` as text, so they show these entries as not found.
  The writer always puts a `WHY:` line (empty if need be) right before
  `ROW`, so they are never read into an option. An old build that decides
  an entry rewrites the file and folds the `ROW` lines into `WHY`, losing
  the row (accepted risk; no header change).

# How it works

## Status (derived)

Per row: the live paragraphs of each cell. In this order:

1. A side with no live paragraphs → `OneSided`.
2. Both sides all blank → `InSync` (nothing to translate). The version's
   side all blank while the other side has text → `Blank`.
3. A decided suggestion (accepted, rewritten, kept) placed on this row
   (see the resolver) whose `ROW TEXT` equals the original's current text
   (any such suggestion will do) → `InSync`. If there are decided
   suggestions but none reads the original now, and the original's side
   differs from its `=` stamps → `OtherChanged`; otherwise go on to the
   stamps. This comes before step 4 so that an accepted line identical to
   the original ("MONTAGE", a heading of proper names) reads in sync (D1).
4. The version's side still equal to its `~` stamps → `Blank`.
5. Otherwise against the links file's `=` stamps: a side is changed when a
   paragraph has no `=` stamp or its `stamp_text` differs. Neither →
   `InSync`; one → that side; both → `BothChanged`.

Only the version's side can be `Blank`. Non-editable kinds
(`ElementKind::Other`, `core/src/model.rs:29`: "End of Act", "Lyrics") get
the note "can't be translated here; change it in Final Draft".

`needs` (what Claude should work on) is `Blank`, `OtherChanged` or
`BothChanged`, **and no suggestion of any status placed on the row whose
`ROW TEXT` equals the original's current text** (C9; amended after the A2
review: a row whose original changed since every suggestion on it needs a
new one); a non-editable row never needs.

`RowStatus` is always from the version's side ("this" = the version,
"other" = the original). A host showing rows on the original swaps
`ThisChanged`/`OtherChanged` and shows nothing as `needs`. One `is_caps`
(the original file's) is used for both members for now. Rows with
nothing on the version's side (new in the original, C4, or a version line
the writer deleted or joined on purpose) need nothing in Phase A and carry
the note `NothingOnVersionSide`: "nothing on this side: if the line is new
in the original, add an empty line of this type in Final Draft at this
point, then reopen; if you removed it on purpose, leave it" (neutral after
the A7a review; Phase C adds lines by suggestion).

**Why this is enough.** The decision that made a row in sync is review
metadata in the event log, with the paragraphs it touched
(`review/src/meta.rs`); undo, redo and undecide already reverse it, so "in
sync" follows them with no new state. The links file's stamps are the
baseline for what wasn't decided in this session.

## D3: the version's lines the links file doesn't list

A paragraph of the **version** that the links file doesn't list (split off,
added by hand or in Final Draft) goes into the first row after the nearest
listed paragraph above it whose version side is empty, if there is one
before the next listed paragraph; otherwise into that paragraph's row. New
paragraphs of the **original** are never placed by D3: re-find gives each
its own row (C4). A row whose listed version lines are all gone counts as
empty here. Several unlisted lines in a run go in order: each after the
row of the line placed just above it (listed or placed by D3), so they
never cross.

Two examples (rows as `a | b`):

1. Rows `16 | 17`, `17 | 18`. In the editor the writer presses Enter in the
   middle of b18; the tail gets fresh id 2930, unlisted. Nearest listed
   above: b18 (row `17 | 18`); no empty-version row before the next listed
   paragraph; so 2930 joins row `17 | 18`, which reads "Español changed".
2. Re-find gave English line a19 (new in Final Draft) its own row
   `19 | ` between `18 | 20` and `20 | 21`. The translator adds an empty
   Dialogue line after b20 in Final Draft and reopens: the new paragraph is
   unlisted; nearest listed above is b20 (row `18 | 20`); the row after it,
   `19 | `, has an empty version side and comes before b21; so it goes
   there. The row reads `Blank` and `needs` a suggestion.

## The resolver: placing `ROW:` suggestions (C1, C7)

`review` knows nothing about links. The host gives `Review::open_live`
(`review/src/session.rs:684`) and `match_live` (`matching.rs:184`) a
resolver, `&dyn Fn(&Entry, &[Vec<Line>]) -> Vec<Vec<ParaId>>`, built by
`versions::resolve` from the links file and both scripts. Every entry with
`ROW:` goes only through it, never through `match_para` (`:228`) or
`before_pass` (`:200`). For an entry it returns the version-side cells of
the rows whose original side reads `ROW TEXT` and whose version side reads
one of the texts review passes: a pending entry passes `BEFORE` then its
options; one decided in the document (its metadata, on lines still live)
only the text it put there (the chosen option, its lines as they read now
if rewritten, `BEFORE` if kept); one decided only in its file (`STATUS:`
not yet applied) its chosen option or `MINE`, then `BEFORE` (a kept one:
`BEFORE`), so it is found whether or not the file already reads it.
Candidates whose version side reads the first text come first, so a
pending entry never takes a row already accepted with its option; within
each group, the cell holding the preferred ids comes first: the lines the
decision is on now when decided in the document, else `PARA`. Review
places entries decided in the document first, then those decided only in
their file, then pending ones, each on the first candidate not already
used (a decided block only on a cell holding the lines its decision
left), and computes its status from its decision metadata as today.
Candidates that are all unusable (a line entry on a row of several lines,
a line that can't take suggestions, a block of another length) give a
reason too. No candidate
gives a reason (`NotFound`: another pair, the original's line changed,
the version's line reads none of the texts).

- A Final Draft save of either file renumbers ids, but a row's texts don't
  change, so pending and decided suggestions stay on their rows.
- The English line reworded since → no candidate → not found ("the English
  line changed since this suggestion"), and the row `needs` a new one.
- Kept, then the writer typed into the line by hand → the line reads none
  of the entry's texts → not found; the row reads "Español changed". No
  other blank is marked kept.
- Several candidates (a repeated cue "TOMÁS", a repeated heading, two
  "Yes." rows): they read the same on both sides, so the suggestion means
  the same on each; a decided entry only finds rows that read its option,
  so it stays on an accepted row. `PARA` picks among them when it can.
- Without a usable links file (missing, broken, or not for these bytes),
  `ROW:` entries are not found, with that reason.

This is the only links-aware parameter review gains, on the open path only
(`resume` → `open_live`), not on decisions.

## Re-find: when a file and the links file disagree

The program that opens a pair (CLI, MCP, chat agent, app) re-finds a
member whose bytes don't hash to its `hash:` (Final Draft saved it, or a
write was interrupted): `versions::align::by_text(old: [(id, kind,
stamp)], new: &Script)`:

1. Exact stamp text, in order (`core::diff::opcodes`,
   `core/src/diff.rs:32`, on (kind, stamp text)). **Blank stamps and blank
   paragraphs never count as equal here** (each gets a unique key), so a
   run of blank Action lines can't be paired out of order (C3).
2. Inside each changed stretch, same-kind lines in order, by position, as
   far as the kinds agree. Blanks pair here, and so does a fully rewritten
   line between found neighbours.
3. Left over: new paragraphs of the version are unlisted (D3); new
   paragraphs of the original get a row each, empty on the version side,
   in order (C4). Old ids left over leave their rows (a row whose version
   side becomes empty reads "nothing on this side"; B3's `links join`
   fixes it).

Cues and headings are verbatim on both sides of a placeholder version, so
they anchor step 1 even after translation. Re-find changes only ids, never
stamps. It writes under the links lock, re-hashing the file just before
writing (and starting over if it changed again), and says: "PURPLE
LIST.fdx changed outside Scriptorium: 2,914 lines found again, 3 changed,
4 new, 1 gone." No fuzzy pass yet (Phase C, if needed).

C3's case, as a test: old version `[H, A"", A"", A"", C, D"", H]`, new
`[H, A"x", A"", A"y", C, D"", H]`: H, C and the second H pair in step 1;
the three Actions and the Dialogue pair by position in step 2.

## Decisions survive a Final Draft save of the version (C2)

A session is set aside when its script's bytes change
(`review/src/sessionfile.rs:236`; the app on open,
`app/src/script_view.rs:124-139`). Review files keep decisions only when
progress is written into them (`scriptorium progress`,
`cli/src/commands.rs:961-970`). So, for the **made member only**, every
save of its session also writes progress (STATUS/CHOSEN/MINE) into each
attached review file, under the session lock: the app's `write_session`
(`script_view.rs:1231`) and each CLI command that saves a decision
(`decide_in`, `commands.rs:692-737`, and the tool hosts). A fresh open then
restores the decisions through `apply_saved` and the resolver.

What is still lost: typing directly into the version (not through a
suggestion) since the last Write .fdx, when Final Draft saves the version
first. The docs say so; the app's "Started fresh" notice names the
set-aside file that holds it.

## Writing a version in place (app only)

The app's Write .fdx (`apply_to_fdx`, `script_view.rs:1789`) on the made
member writes that file. The CLI's `apply` (`commands.rs:1012`) and MCP's
`write_script` (`mcp/src/tools.rs:166`) need `-o`/`output` for it ("PURPLE
LIST.es.fdx is written from the app: Write .fdx"), so nothing writes the
file under an open window. Any other file keeps `<name> (scriptorium).fdx`
(`script_view.rs:252`, `commands.rs:1098`).

`scriptorium_fsio::versions::write_in_place`, one function:

1. **Guard**, under the script lock and the links lock: the bytes on disk
   hash to the session's script hash, and the links file's `hash:` for
   this member equals it. Otherwise: "PURPLE LIST.es.fdx changed since you
   opened it. Close it in Final Draft and reopen it here." Refuse while a
   pending suggestion's line was split or joined since it was made
   (`Session::line_change`, `session.rs:1452`): "Decide or rewrite #4 and
   #9 first." (The rebased session has no record of the split, and
   accepting the whole-line option later would duplicate the tail;
   `Session::check`, `session.rs:1590-1606`.)
2. **Reload review files** that changed on disk (an agent may have just
   appended), as the app's reload does (`reload`, `script_view.rs:1324`).
3. **Write progress** into each attached review file (old ids: placement
   doesn't depend on them). For entries placed as items, rewrite `PARA` to
   the new positions as a hint; drop `PARA` for ids that are gone; never
   move an entry that wasn't placed.
4. **Write the `.fdx`** (`FdxFile::write_script`). Paragraph *i* of the new
   file is the live paragraph *i*.
5. **Update the links file:** ids to positions, the new hash, `=` stamps
   with the current texts of every row that reads `InSync`
   (`versions::written::after_write`), including rows in sync only through
   a decided suggestion, so they stay in sync if a review file is lost
   later. Other rows keep their stamps, except that a row which lost a
   listed version line drops the stamps of its remaining version lines
   (they described the old cell). Rows left with no ids are dropped.
6. **Write a fresh session** for the new bytes: what a first open builds
   (`resume` with no saved session, `sessionfile.rs:224-230`). Decisions
   come back from step 3 through `apply_saved`.

The app then drops its old `ScriptView` **without a final save** and opens
the written file (D4); `write_in_place` owns the session write, so a
pending autosave (`schedule_autosave`, `script_view.rs:1194`, 600 ms) or
the watch tick (`watch_files`, `:1300`) of the old view must be cancelled
first. A crash after step 4 looks like an outside change next time:
re-find maps the ids, the old session is set aside, and the decisions are
in the review files.

The status bar says once: "Wrote PURPLE LIST.es.fdx. Undo starts again from
here." Undecide still works after it: the blank comes back and the row
reads "not yet translated". That changes only the row's status label,
never `needs`: the suggestion, pending again, still reads the original
(decided by the orchestrator after the A7a review).

## Opening a version from the CLI after a Final Draft save

Today the CLI refuses a session it can't use
(`crates/fsio/src/opened.rs:117-123`); the app sets it aside. New rule, for
all scripts, when the reason is "the script has changed": set the session
aside **with a note** if it holds nothing the files don't: its decision
keys with their status and choice (ignoring `@para`, C9) equal the review
files' saved progress, and it has no events but decisions. Otherwise
refuse: "PURPLE LIST.es.fdx changed outside Scriptorium, and its session
holds typing that isn't in any file. Open the script in the Scriptorium
app: it keeps that session as a set-aside file and starts fresh."
`set_aside_path` moves from `app/src/disk.rs:207` into `scriptorium-fsio`
so both use one name. With C2's progress on every save, the refusal only
happens after typing in the version.

## Ids and sessions

- Fresh ids (`core/src/doc.rs:23-25`) can be given out again after a
  session reset. That can't mislead anything here: the links file names
  only file ids, and `ROW:` entries are placed by row content.
- After a write, the rebased session has no split history; step 1's guard
  covers it.

## Locking

- `SessionLock` per script (`crates/fsio/src/lib.rs:405-442`); a links file
  has its own lock, `<links stem>.lock`, through `SessionLock::acquire`.
- Writing the links file (version new, re-find, write in place, later link
  edits) holds the links lock from reading it to writing it.
- Decisions write only this script's session (plus, for the version, its
  review files' progress), under its lock.
- Order: at most one script lock, taken before the links lock. The other
  member is opened read-only, from its file, and never saved.
- Two windows on the two files, or the app and the CLI: each writes only
  its own session; each window reloads the links file and the other member
  when they change (every second, `watch_files`).

# Code layout

- **`crates/versions`** (`scriptorium-versions`): pure (rule 1), compiles
  to wasm. Depends on `scriptorium-core`; `scriptorium-fdx` only as a
  dev-dependency (fixtures). `is_caps` comes in as a closure.
  - `links`: `Links { id, members, rows, paras, extra_headers }`,
    `Member { file, language, hash, created }`, `Row { cells }`,
    `Para { kind, stamp: Option<Stamp> }`,
    `Stamp::{Synced(String), Placeholder(String)}`; `parse`, `write`,
    `validate`; `language_name(code)`.
  - `rows`: `lay_out(links, original: &Script, version: &Script, placed:
    &[Placed], is_caps) -> Vec<LaidRow { cells, status, needs, note,
    suggestion }>`. `Placed { cell, decided, kept_blank, row_text }` comes
    from the host (review items with `ROW:`), so versions doesn't depend
    on review. D3 lives here.
  - `resolve`: the resolver.
  - `placeholder`: `blank_copy(original: &Script, id, original_member:
    Member, version_member: Member, is_caps: impl Fn(&ElementKind) -> bool)
    -> Result<(Script, Links), BlankCopyError>` (sets `created` on both
    members; the host fills the version's hash once written).
  - `links::stamp_text(text, caps)`, re-exported at the crate root: the one
    normalization every stamp and `ROW TEXT` comparison uses.
  - `align::by_text`: re-find.
  - `written`: `after_write`.
  - Later: `edit` (B3/C), `align::by_structure` (E), `glossary` (D).
- **`scriptorium_fsio::versions`** (feature `opened`; the app, CLI, MCP and
  chat host all depend on fsio): finding the pair, opening the other member
  from its file, the links lock, re-find writes, `version_new`,
  `write_in_place`, `set_aside_path`, progress-on-save for the version.
- **review:** `[empty]`, `ROW`/`ROW TEXT`, `Item::row`, the resolver
  parameter, the empty-line refusals.
- **tools:** `Ctx` (`tools/src/lib.rs:39-51`) gains `links:
  Option<LinksCtx>`; new tools; `row` on `suggest_change(s)`; `Outcome::NewVersion`.
- **hosts of the tools:** `cli::commands::tool` (`commands.rs:1197`, Ctx at
  `:1229`), MCP through it (`mcp/src/tools.rs:69`, `shared`), and the chat
  agent (`crates/chat/src/exec/mod.rs:521`). All three build `LinksCtx`
  and carry out `NewVersion` through fsio. The chat agent sees the new
  tools automatically (`chat/src/host.rs:341`, the shared catalogue).

# Phase A — translate through Claude

Ends with: `scriptorium version new "PURPLE LIST.fdx" --lang es` (or
Claude, through the plugin or the chat panel) makes a Spanish version that
opens in Final Draft; Claude translates it into "Translation from English";
the writer decides in the app as today, with the English line on the card;
accepted rows read in sync; Write .fdx writes `PURPLE LIST.es.fdx` itself;
Final Draft edits to either file are found again.

Every builder and every reviewer is **Opus** (the user, 2026-11-17).

### A1. Links file, crate skeleton, language names (`versions::links`)

Types, parse/write/validate, errors with line numbers, unknown headers
kept, `language_name`. Workspace entry, `[lints] workspace = true`, wasm
check as for core.

Tests: the example round-trips byte for byte; hand-edited spacing reads;
crossing rows, an id twice, ids out of order, a bad text line each refused
with the line; one-sided rows; `~` with and without text; empty `=` text;
unknown header kept; unknown language code shown as itself.

### A2. Status, D3 and the resolver (`versions::rows`, `versions::resolve`). After A1.

Tests: every status; the order of D1 (an accepted line identical to the
English reads in sync); a decided suggestion whose `ROW TEXT` still matches
→ in sync, the English changes after → "English changed"; undecide → back
to stamps; kept on a blank → `Blank`, not `needs`; a row with a pending
suggestion isn't `needs`; `Blank` never on the original. D3's two examples;
Enter at the start of a non-empty line inserts an empty paragraph above
(`core/src/doc.rs:17-18`, `:969-971`) → it joins the row above; Backspace at
the start of a translated line under a blank removes the blank (a join
keeps the non-empty paragraph's id, `doc.rs:20-21`, `:990-993`) → the row
above goes one-sided. Resolver: the English reworded → none; ids shifted
(a line inserted above) → same rows; a repeated cue with one accepted and
one pending instance → the decided one on the accepted row; kept then
hand-typed → none; `PARA` picks among identical rows.

### A3. Placeholder version (`versions::placeholder`, fdx tests). After A1.

`blank_copy(script)` takes the original's file script (C8): Scene Heading
→ kept, `~`. Character → kept, `=`. Non-editable kinds → kept, `=`. Empty
→ kept, `=`. Everything else → spans emptied, `~` "". Rows `i | i`. Dual
dialogue (`Script::fixed`, `core/src/model.rs:133`) stays fixed; blanking
is a text edit. The host writes it with `write_script` on the original's
`FdxFile`, then `scriptorium_fdx::clear_text_offsets`, so every byte
outside the blanked runs is the original's (title page, Mores and
Continueds, scene numbers and summaries) except two lists of character
offsets into the English text: `<ScriptNotes>` (the writer's notes,
`ScriptNote Range="…"`) and `<SpellCheckIgnoreLists><IgnoredRanges>`.
Those are emptied (the wrapper elements stay, their children go): in the
version they would sit on the wrong words, and the notes are the English
writer's. The title page and scene summaries stay in English by design;
the translator changes them in Final Draft.

Tests (fixtures; the real script when present): count, kinds and ids of
the written file match; headings and names byte-identical; dual dialogue
blanked; the first run's attributes kept with empty text, in a CRLF file
too; the new file's no-op round-trip is byte-identical.

**Right after A3: hand test 1** (Final Draft opens the placeholder file,
and saving it once changes nothing `rows` would show), before more is
built on it.

### A4. Re-find (`versions::align::by_text`). After A1.

Tests: C3's case above; a line edited; inserted; removed; a scene added to
the English (rows of their own, C4); a whole file re-saved with other
whitespace; a translated heading saved in Final Draft; a Spanish line split
in two in Final Draft (the new half unlisted, D3); two Spanish lines in
adjacent rows merged (the second row reads "nothing on this side"); a fully
rewritten line between found neighbours; Final Draft dropping or keeping
empty paragraphs on save (whatever hand test 1 shows).

### A5a. Opening and set-aside (`crates/fsio/src/opened.rs`, `fsio::versions`). Parallel with A1–A4.

The CLI set-aside rule, `set_aside_path` moved from `app/src/disk.rs:207`,
progress-on-save for the made member (C2) in fsio's `Opened::save`
(`opened.rs:272`). Tests: a Final Draft-style save with only decisions in
the session → set aside with a note, decisions back from the review files;
with typing → the refusal; `@para` differences ignored.

### A5b. `version new` (`fsio::versions::version_new`, CLI). After A3, A5a.

```
scriptorium version new <script.fdx> --lang <code> [--from <code>] [-o <name.fdx>]
      make a version lined up with the script: scene headings and character
      names copied, other lines blank, every line linked. Default name
      "<stem>.<code>.fdx" next to the script. --from: the script's language.
```

Opens the original from its file and refuses if its session has changes
(C8): writing its live document must give back its file byte for byte (an
undone edit or a kept decision doesn't block; a write error is reported as
itself). Writes the `.fdx` with `create_new` (a failed write removes the
partial file), then the links file. A re-run:
links file and `.fdx` both present → check the `.fdx` hash against `b
hash:`; equal → "already made" with the paths; different → re-find (the
original too, if its hash differs). A re-run whose `--lang` or `--from`
contradicts the pair is refused. Only the links file → the `.fdx` was
renamed, moved or deleted: refused with the missing-file fix (the `b:` line
to edit, any `.fdx` holding `b hash:`'s bytes); the links file is never
deleted. Only the `.fdx` → adopt it if byte-identical to what it would
write, else refuse naming the file to move away. A links file naming either
file that doesn't parse refuses. Names and codes are checked before any
lock file is made. Prints a summary, a tip when `--from` wasn't given, a
next step, and last the version's absolute path.

Tests: both files made; re-runs after each simulated crash; the original
never written; refused with changes in the original's session; refused
when a pair exists.

### A5c. `versions` and `rows` (CLI). After A2, A4, A5a.

```
scriptorium versions <script.fdx> [--json]
      the version lined up with the script: file, language, links file,
      rows by status, rows new in the original
scriptorium rows <script.fdx> [--scene <n|text>] [--needs] [--json]
      rows as "#<row> <status>  ¶ids text | ¶ids text", with any
      suggestion on the row and its status, and notes
```

Every command that reads a pair re-finds first, under the links lock
(read-only commands print the note); `apply` on the made member needs
`-o`. Tests: headings `Blank`, names in sync on a new version; a broken
links file → clear error, untouched; `apply` without `-o` refused on the
version.

### A6a. Review file format (`crates/review/src/file.rs`). Parallel with A1.

`[empty]`, `ROW`/`ROW TEXT`, `Entry::check` (`file.rs:230`; `[empty]`
needs `ROW:`), serialize/append, `Item::row: Option<RowRef>` (links id,
texts; opaque to review), the `SCRIPTORIUM LINKS` refusal. Tests:
round-trips (`[empty]`, a typed blank block line, an empty `ROW TEXT:`);
refused without `ROW:`; a links file given as a review → refused; existing
review files unchanged.

### A6b. Placing `ROW:` entries (`crates/review`: matching, session). After A6a.

The resolver parameter on `resume`/`open_live`/`match_live`, tested with a
fake closure; `ROW:` entries skip `match_para` and `before_pass`. The
empty-line refusals relaxed for `[empty]` entries only and only on the
resolver's path: `append_entries` (`session.rs:1164-1172`, "needs a
BEFORE"); the unplaced explanation's text search (`session.rs:612-622`)
skipped for `ROW:` entries. (`matching.rs:66`, `:84-85`, `:259` stay as
they are: `ROW:` entries never reach them.) Tests: placed where the fake
says; none → not found with the reason; decided status from metadata;
accept, undo, redo, undecide on a blank; two entries never on one cell.

### A7a. After a write (`versions::written`). After A2.

`after_write`. Tests: ids to positions with a split line in the session;
in-sync rows get `=` stamps with current text; `Blank` rows keep `~`;
changed rows keep their old stamps.

### A7b. Write in place (`fsio::versions::write_in_place`). After A5b, A6b, A7a.

Steps 1–6. Tests: accept rows, write: the file holds them, links has
positions, the new hash and `=` stamps, reopening shows them decided and in
sync; pending blank suggestions survive and still place; a not-found entry
stays not found, not moved; entries an agent appended just before the
write are kept; the guard refuses after a Final Draft save and on a links
hash mismatch; a pending suggestion over a split line refuses the write;
the `.fdx` write failing midway (simulated) leaves the old file, links and
session usable; undecide after the write → `Blank`.

### A8. Read tools (`crates/tools`, the three hosts). After A2, A5c, A6b.

- `list_versions`: what `versions` prints.
- `read_rows {scene?, from_row?, limit?, needs?}`: `scene` is a scene
  number (1 = the first) or part of its heading, as `read_scene` takes it;
  `from_row` is a row number as `read_rows` and `rows` print it (rows are
  numbered from 1 in script order, after re-find); `limit` defaults to 200.
  JSON rows `{row, status, needs, note?, this: [{id, kind, text}], other:
  [{id, kind, text}], suggestion?: {review, n, status}}`. Blank paragraphs
  are shown. Called on the original: rows shown, nothing `needs`.

Hosts: `LinksCtx` built in `commands::tool` (`cli/src/commands.rs:1229`),
the chat agent (`chat/src/exec/mod.rs:521`), and through `tool` for MCP;
both tools added to the read-only lists (`mcp/src/tools.rs:64`,
`cli/src/commands.rs:1119`).

Tests: a new version: every blank row `needs`; a kept or pending row isn't;
called on the original: nothing `needs`; MCP and the chat catalogue list
both tools; the MCP instructions test (`mcp/src/tools.rs:448`) passes.

### A9. Write tools (`crates/tools`, the three hosts). After A6b, A8.

- `new_version {language, original_language}` → `Outcome::NewVersion`; the
  hosts run A5b and return the version's absolute path ("from now on, use
  this path as `script`"). `original_language` is required in the tool
  (Claude can tell from the script).
- `suggest_change` / each `suggest_changes` item: optional `row` (a row
  number from `read_rows`). Only on the made member ("rows are suggested on
  the version, PURPLE LIST.es.fdx; this is the original"). The target must
  be the version side of that row: the host checks with `LinksCtx`. A blank
  target is allowed only with `row` on a `Blank` row: `check_line`
  (`suggest.rs:46-48`), `validate` (`:107`, which builds `[empty]` instead
  of `text.trim()`), and `block.rs:125-132` (including a blank
  `insert_after` anchor) relaxed for that case. The entry gets `ROW:` and
  `ROW TEXT:` from the original's file now.
- **Updates (C5):** a review holds one suggestion per line
  (`check_duplicate`, `suggest.rs:62-84`, any status). A `row` suggestion
  refused there because the line was decided says: "this line was decided
  in <review>; put updates in a new review, e.g. \"Translation from English
  · updates 2026-11-20\"". The skill does that for rows reading "English
  changed" or "both changed". (The chat agent writes to its own thread's
  review, so a new conversation is a new review.)
- `write_script` (MCP) description: "For a version Scriptorium made, the
  writer saves it in the app with Write .fdx; here, give `output`."
- The built-in prompt (`tools/src/prompt.rs`) and descriptions: "Give one
  option when the line is obvious. Give two or three only for a real
  choice (formality, an idiom, a pun, length), each with a short why
  saying how it differs."
- **The agent's review attaches to the version.** A `review_file` given to
  a suggesting tool is created and attached to the session of the script
  the tool was called on (`suggest.rs:23`, `commands::tool`), and the app
  picks up session changes every second. Tested for the version: suggest on
  the Spanish with `review_file` → `reviews` on the Spanish lists it; the
  app (headless) shows it.

Tests: a batch on blank rows → `[empty]` entries with `ROW`; a target not
in the row refused with the row's ids; `row` on the original refused; a
blank target without `row` still refused; a re-suggestion on a decided
row → the message naming a new review, and it succeeds there;
`write_script` on the version without `output` refused; `new_version`
returns the absolute path; the prompt names only real tools.

### A10. App (`crates/app/src/script_view.rs`). After A7b, A9.

- Opening (`Opened::load`, `script_view.rs:100-139`): find the pair,
  re-find, pass the resolver to `resume_from_disk`; reload the pair when
  the links file or the original changes (`watch_files`, `:1300`); C6's
  notice when the links file no longer matches the open bytes.
- The card (`render_card`, `:2221`) shows an item's `ROW TEXT` above its
  options, labelled with the original's language, and when the original's
  file text differs: "The English line changed since this suggestion",
  with a word diff (`core::diff::word_diff`, `diff.rs:130`).
- Write .fdx (`apply_to_fdx`, `:1789`) on the made member: cancel the
  autosave and the watch task, call `write_in_place`, drop the view
  without saving, open the written file (D4).
- Progress-on-save (C2) in `write_session` (`:1231`) for the made member.
- Opening the original: a status-bar note "Lined up with PURPLE
  LIST.es.fdx: open that file to translate".

Tests (headless): write after an agent append keeps the new suggestions;
an autosave scheduled just before the write leaves the new session
unchanged; write then reopen shows decisions; a refused write shows the
notice; the chat agent's `read_rows` works in the panel.

### A11. Plugin, skill, docs (`plugin/`, `docs/testing-on-mac.md`). After A9.

- `plugin/skills/translate-version/SKILL.md`: to translate a script, make
  the version if there isn't one (`new_version`) and use the path it
  returns from then on; read `brief.<lang>.md` next to it and `glossary.md`
  in the folder if they exist (as files); then scene by scene: `read_rows`
  with `needs`, `suggest_changes` with `row`, into `Translation from
  <Language>.review.txt`; updates into a new dated review. Rows with any
  suggestion are skipped. One option when obvious, two or three for real
  choices, each with a short note. Suggest on cue rows for generic cues
  and typed extensions. Report rows new in the original and say how to add
  them (an empty line of that type in Final Draft). Never edit the `.fdx`
  or the links file by hand.
- MCP server instructions name the new tools. `plugin/package.sh` copies
  `skills/`; check the new skill is in the Mac zip.
- `docs/testing-on-mac.md`: "What's new in 0.5.0" (translation, plus the
  0.4.0 chat panel, which ships in the same release), and in §7 a part
  "Translating with Claude":
  - Update: replace the plugin folder with the new one, run the `xattr`
    step again, start Claude with `--plugin-dir`, check `/mcp`.
  - Ask: "Make a Spanish version of this script and translate scene 1."
  - Where things appear: the Spanish file and its links file next to the
    English; the review in the sidebar when you open the Spanish.
  - How to work: decide in Scriptorium. Close the Spanish file in Final
    Draft before Write .fdx. Write .fdx saves the Spanish file itself; then
    polish in Final Draft and reopen in Scriptorium whenever you like.
    Typing straight into the Spanish in Scriptorium is lost if Final Draft
    saves the file before you Write .fdx; decisions are not.
  - Keep the project folder out of iCloud-synced Desktop or Documents, and
    edit it on one computer.
  - The title page and scene summaries stay in English, and the English
    script notes aren't copied: change the title page (and summaries, if
    you use them) in Final Draft.
  - New English lines: add an empty line of that type in the Spanish in
    Final Draft at that place, then reopen; Claude fills it.
  - Start over: delete `PURPLE LIST.es.fdx`, `PURPLE LIST.es.links.txt`,
    `PURPLE LIST.es.scriptorium.json` and the translation review files;
    `version new` again.
  - Keep together when sharing: the two scripts, the links file, review
    files and session files.

### A12. End to end (`crates/fsio/tests`, integration). After A7b, A9.

Fixtures, and the real script when present: `version new` → `suggest_changes`
through `tools::call` with a `Ctx` → decide (accept, keep, rewrite) →
`write_in_place` → simulated Final Draft edits to both files (a heading
changed, a Spanish split, an English line added and one reworded) →
re-find → `rows`: every status as expected, every decision still placed,
the reworded row `needs`.

**Phase A reviews:** two Opus reviewers per step (correctness, design);
two on the whole phase.

# Phase B — the left column in the app

Ends with: English lined up left of the Spanish, row by row, with status
dots, beside the card and the chat panel; the sidebar shows the pair;
Swap; File › New version…; Mark in sync.

### B1. Pair state (`crates/app/src/links_view.rs`, new). After A10.

Grows A10's loading: rows recomputed on document changes (debounced like
the outline, `OUTLINE_SETTLE`, `script_view.rs:287`) and when the links
file or the original changes; per-paragraph bands for the editor. The
session gains `#[serde(default)] links: Option<String>` (the choice under
"Lined up on the left", `"none"` for Nothing), carried through every save
(`SessionFile::new` at `script_view.rs:767`, `:1234`; fsio's
`Opened::save`, `opened.rs:272`). Older builds drop it (harmless).

### B2. Two columns (`crates/app/src/{elements,editor,layout}.rs`). After B1.

The editor stays one `gpui::list` of right paragraphs (`editor.rs:769`).

**Spike first** (throwaway, one builder): 1:2 and 2:1 rows, a one-sided
row on each side, a two-line right paragraph against a five-line left one.
Try (1) *slices*: each right item draws its share of the left cell,
heights `max` per item; (2) *row owner*: the first right item draws the
whole row, the others zero height. Pick by: no overlap or clipping while
scrolling, cursor and selection work everywhere, code local to
`ParaElement`.

Then: `Decor` (`editor.rs:79`) gains the bands and `columns: bool`;
`ParaElement::request_layout` (`elements.rs:134-215`) shapes the left text
and sets heights; `paint` (`:230`) draws the left cell on grey paper, the
status dot in a 28 px gutter, the right cell, hatching for an empty side;
the review mark stays in the right column's margin (`paint_mark`, `:279`).
When bands change without a document change (the other window, a
re-find), the affected items are re-measured with `ListState::splice(range,
count)`, or `remeasure()` after a reload, as `sync_list` (`editor.rs:292`)
does. Each column uses the page geometry the chat branch already has: the
zoom fits one column (`zoom_for`, `layout.rs`), and when the page is wider
than the column the outer margins are cropped (`page_left`), keeping the
text column and the mark; the minimum is `READABLE_ZOOM` (0.6).

### B3. Sidebar, header, Swap, New version, Mark in sync. After B1.

- `render_sidebar` (`script_view.rs:2025`) and the rail (`render_rail`,
  `:2157`): "Lined up on the left": the pair ("◧ English · PURPLE LIST",
  "212 to translate"), broken or missing greyed with the fix, and
  "Nothing".
- Toolbar (`workspace.rs:364`): "PURPLE LIST · Español ⟵ English" and Swap
  (opens the left member with this one lined up).
- File › New version… (`menus.rs:52-55`; Cmd/Ctrl+T, free on this branch):
  a sheet (language, original's language, saved-as name); Create and open
  runs A5b and opens the version with the original on the left.
- Mark in sync / Not in sync (Edit menu, and `scriptorium links <script>
  sync|unsync <row>`): writes `=` stamps for the row's current texts (or
  removes them) in the links file under its lock; refused for a row with
  unlisted paragraphs ("Write .fdx first"), so the links file never names
  a session-only id. Not on the undo stack; the status bar says "Marked
  row 37 in sync (Edit › Not in sync takes it back)".
- `links join <row>` (CLI) for rows a merge left empty on one side.

### B4. Five columns (`crates/app/src/agent_panel/mod.rs`, `script_view.rs`). After B2.

The window holds sidebar (or rail) | left script | right script | card |
chat panel. `agent_panel::columns` (`mod.rs:136`) gains `lined_up: bool`.
With it, the script area needs two columns at `READABLE_ZOOM` plus the
28 px gutter. Narrowing order, extending the existing one: the sidebar
becomes a rail; the zoom shrinks (both columns share it); the panel narrows
to `PANEL_MIN`; the card narrows to `CARD_MIN`; **then the left column
gives way** (a 28 px strip: "English hidden: widen the window or close the
chat (Cmd+L)"); then the existing order (the script or the card). The
window's minimum size (`window_min_size`, `mod.rs:103`) doesn't change.

On a 13" MacBook Air (window ~1,470 px), with the 44 px rail and the 380 px
card: chat closed → ~1,018 px for both columns, ~0.74 zoom (~12 px
Courier), both shown; chat open → even at the narrowest panel and card the
columns would be under 0.6, so the English gives way. Checked on the
partner's MacBook before tuning.

Tests: `columns` with `lined_up` at 1,920, 1,470 and 1,100 px, panel open
and closed; the left column gives way before the card or the script; no
column ever cut off at the minimum size; headless shots at those widths.

**Phase B reviews:** two Opus reviewers per step, and a design review of
the screenshots against the mockup.

# Release v0.5.0

After Phases A and B, and the hand tests below (the gate): bump the
workspace version (`Cargo.toml`, `version = "0.4.0"` → `"0.5.0"`) and the
plugin's (`plugin/.claude-plugin/plugin.json`), finish "What's new in
0.5.0" in `docs/testing-on-mac.md`, and tag `v0.5.0`. The release workflow
(`.github/workflows/release.yml`) checks the tag against the version and
attaches `Scriptorium-0.5.0-mac.zip` and `Scriptorium-plugin-0.5.0-mac.zip`.

# Later phases

- **C. Fixing links and adding lines.** `links move|split` and the `links`
  tool; one-sided rows filled by insert suggestions (`insert_after` on the
  last version line above, D3 places the accepted lines, the decided item
  covers them); a fuzzy pass in re-find if hand tests show position pairing
  guessing wrong.
- **D. Context, brief, glossary.** `versions::glossary` (`glossary.md`: one
  table, columns by language code plus `note`; `misses` underlined in the
  left cell; "Add to glossary…"; `scriptorium glossary add|check`);
  `row_context` (the brief, the glossary lines for the scene, the whole
  scene as rows, the English word diff since the stamp); the brief prompt
  `crates/tools/src/prompts/brief.md`, as a plugin skill
  `translation-brief` (same text, tested) and as a command in the chat
  panel.
- **E. Linking two existing files.** `align::by_structure`; `scriptorium
  links new <a> <b> --into <a|b>` (sets `created`'s successor: which side
  is written), File › Line up with….
- **F. Same language.** `version new --copy`; `align::by_text` across
  files; `suggest_from_left`; same-language labels.

# Edge cases

- **Final Draft edits either file.** Re-found on the next open. The
  version's session is set aside; decisions come back from the review files
  (C2). Pending suggestions still place (row content). Write .fdx refuses
  until the version is reopened.
- **Ids.** File ids hold while the hash holds; the links file names only
  file ids.
- **Splits and joins.** See A2's tests and D3's examples.
- **Dual dialogue:** blanked and linked 1:1; insertions among fixed lines
  refused as today.
- **Title page, headers, Mores and Continueds, scene summaries:** copied
  untouched; changed in Final Draft.
- **Non-editable kinds:** copied, noted, never `needs`.
- **Revision colours.** Blanked runs keep the first run's attributes; hand
  test 1 checks them; if wrong, `blank_copy` drops revision attributes.
- **Renamed or moved files:** the missing message with the hand-edit fix.
- **The English changes after a suggestion:** the card says so with a
  diff; the suggestion is no longer placed (its `ROW TEXT` is gone) and the
  row `needs` a new one, which goes in a new dated review if the line was
  decided.
- **A broken links file:** shown broken with its line; nothing writes it.

# Work breakdown

All builders and reviewers: Opus.

| # | Piece | Owns | After |
|---|---|---|---|
| A1 | links file, skeleton, language names | `crates/versions` | – |
| A2 | status, D3, resolver | `versions::{rows,resolve}` | A1 |
| A3 | `blank_copy`; fdx tests; hand test 1 | `versions::placeholder`, `crates/fdx/tests` | A1 |
| A4 | re-find | `versions::align` | A1 |
| A5a | set-aside rule, progress on save | `crates/fsio`, `app/src/disk.rs` | – |
| A5b | `version new` | `fsio::versions`, `crates/cli` | A3, A5a |
| A5c | `versions`, `rows` | `crates/cli` | A2, A4, A5a |
| A6a | review file format | `review/src/file.rs` | – |
| A6b | placing `ROW:` entries | `review/src/{matching,session,sessionfile}.rs` | A6a |
| A7a | `after_write` | `versions::written` | A2 |
| A7b | `write_in_place` | `fsio::versions` | A5b, A6b, A7a |
| A8 | read tools | `crates/tools`, cli, mcp, chat | A2, A5c, A6b |
| A9 | write tools | `crates/tools`, cli, mcp, chat | A6b, A8 |
| A10 | app | `crates/app/src/script_view.rs` | A7b, A9 |
| A11 | skill, docs | `plugin/`, `docs/` | A9 |
| A12 | end-to-end test | `crates/fsio/tests` | A7b, A9 |
| B1 | pair state, session field | `crates/app/src/links_view.rs` | A10 |
| B2 | spike, two columns | `crates/app/src/{elements,editor,layout}.rs` | B1 |
| B3 | sidebar, Swap, New version, Mark in sync | `script_view.rs`, `workspace.rs`, `menus.rs`, cli | B1 |
| B4 | five columns | `agent_panel/mod.rs`, `script_view.rs` | B2 |

Phase A, in parallel where marked:
A1 ‖ A5a ‖ A6a → A2 ‖ A3 (then hand test 1) ‖ A4 ‖ A6b → A5b ‖ A5c ‖ A7a
→ A7b ‖ A8 → A9 → A10 ‖ A11 ‖ A12.

Phase B: B1 → B2 (spike first) ‖ B3 → B4. Then the hand tests, then the
release.

Each piece: two reviewers that didn't build it, in fresh contexts, one for
correctness and one for design (AGENTS.md); two more on each whole phase.

# Hand-test checklist (gate for v0.5.0)

On copies, in a scratch folder, not in iCloud. Screenshot each result.

1. (Right after A3.) `scriptorium version new "PURPLE LIST.fdx" --lang es`.
   Open `PURPLE LIST.es.fdx` in **Final Draft**: no warning; headings and
   names in English with scene numbers; every other line an empty line of
   the right type; "(CONT'D)" still appears where Final Draft adds it; the
   English title page and scene summaries (by design: they are changed
   in Final Draft); no script notes; dual dialogue intact; revision
   colours sane. Save it
   once unchanged and close. `scriptorium rows "PURPLE LIST.es.fdx"`: the
   file is found again and nothing moved (note whether Final Draft dropped
   or merged empty paragraphs).
2. Ask Claude (plugin) to translate scene 1. Open the Spanish in the app:
   "Translation from English" in the sidebar; the card shows each English
   line; accept a few, keep one, rewrite one. `rows --scene 1`: accepted
   and rewritten rows in sync; the kept blank "not yet translated" but not
   `needs`. Undo an accept: "not yet translated" and `needs` again.
3. Ask Claude to continue with scene 2 while the app is open; Write .fdx
   straight away. The status bar says it wrote `PURPLE LIST.es.fdx` and
   that undo starts again; scene 2's new suggestions are still there. Open
   the file in Final Draft: Spanish lines with the right types and indents;
   untouched blanks empty.
4. In Final Draft: change a translated heading, split one Spanish action
   line in two, save, close. Reopen in the app: found again with a note;
   those two rows "Español changed"; nothing else moved; decisions and
   pending suggestions still on their rows.
5. In Final Draft: reword an English line and insert a new English line
   near the top; save. `rows` on the Spanish: the reworded row "English
   changed" and `needs`; the new line a row of its own with the note; every
   other row unchanged. Ask Claude to update: its suggestions go into a new
   dated review.
6. Chat panel (Cmd+L) on the Spanish: ask it to translate scene 3; the
   suggestions appear on the card.
7. (B) Open the Spanish: English on the left, rows lined up; a two-line
   Spanish action beside a one-line English one pads the English row.
   Statuses match `rows`. Swap. On the MacBook: with the chat closed both
   columns show; with it open the English gives way, with the note.
8. (B) English and Spanish in two windows. Accept in the Spanish: the
   English window's left column updates within a second. Mark a
   hand-edited row in sync; Not in sync takes it back.
9. **On the partner's Mac, with the v0.5.0 plugin zip:** follow
   `docs/testing-on-mac.md` §7 "Translating with Claude" from a fresh
   download: `/mcp` shows `scriptorium`; "Make a Spanish version of this
   script and translate scene 1" makes the file and the review; the app
   shows them; Write .fdx saves the Spanish file.

# Open questions for the user

None that change the build. The two from the last round are decided (undo
ends at Write .fdx; the write is refused while a pending suggestion's line
was split or joined).
