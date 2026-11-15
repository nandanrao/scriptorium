# Step 0 spec: stable review ids, `scriptorium-tools`, live reads, CLI lib

Status: revised after two reviews (2026-11-14), ready to build. Part of
[built-in-agent.md](built-in-agent.md) ("Design review — accepted changes" 1–4).
Decisions: AGENTS.md rule 1 is "functional core, imperative shell" (CLI lib is
shell; tools crate is a pure core); threads live in `<script>.threads/`.

**What actually breaks today.** The decision key is `review/<id>/<item.entry>`
(`session.rs:234`), and `item.entry` is the entry's index in the file
(`session.rs:121`), so appending an entry at the end moves no existing key. The
losses are elsewhere: (a) the id is `hash(file text)[..12]` (`session.rs:165`),
so any append or hand edit orphans every decision; (b) `save_progress` sorts
entries into script order (`session.rs:409`), so writing progress renumbers
entries. And `Review::open` matches only against `fdx.to_script()` (the
original), so a suggestion written on a line the writer already changed isn't
found on restart (part B).

## A. Stable review ids (`crates/review`)

**`Id:` header.** `Id: <token>` (`[A-Za-z0-9._-]{1,64}`), honoured only before
the first block; `HEADER_RE` becomes `(Script|Reviewer|Id)`.
`ReviewFile::id: Option<String>`; `serialize` writes it after `Script:`.
`Review::id` = header value, else `hash(text)[..12]` as today. Header-less
files keep exactly today's id, so v1/v2 sessions work unchanged: no key
rewriting, no migration, no new session `VERSION`.

Keys stay `review/<review id>/<item.entry>`.

Who writes `Id:`:
- thread reviews: `Id: thread-<thread id>`;
- `scriptorium suggest` creating a file: `Id: <hash of new text>[..12]`;
- `append_entry` on a header-less file: inserts `Id: <current hash id>`;
- `save_progress`: always writes `Id: <review id>` (also fixes
  `progress -o same-file` changing the id);
- **attaching** a header-less review (`Session` add via app or CLI): the shell
  splices `Id: <current hash>` into the file (one inserted line after
  `Script:`, else after the format line, else at the top; not a reserialize),
  so later hand edits don't orphan its decisions.

Two attached files with the same id: `Session::add` refuses (it already
refuses equal ids, `session.rs:239`); the message names both files and says
they share `Id:`.

**Other API.**
- `Entry.para: Option<ParaId>` from `PARA:` (part B). `Block` needs its own
  `para` slot: `Block::field` sends unknown keys to `mine` today.
- `pub fn file::append_block(text: &str, entry: &Entry, id: &str) -> String`
  splices one block at the end of the file text (comments, wrapping, fences
  survive); inserts `Id:` only if missing. Replaces the parse+serialize in
  `review_cmds.rs:504`.
- ```rust
  pub fn append_entry(&mut self, doc: &Document, r: usize, entry: Entry,
      is_caps: impl Fn(&ElementKind) -> bool) -> Result<Appended, String>;
  pub struct Appended { pub file_text: String, pub item: Option<usize> /* None: unmatched or invisible */ }
  ```
  Matches only the new entry (against the live doc, skipping paragraphs this
  review already uses), and pushes the `Item` at the **end** of `items` (no
  re-sort; reopen sorts by script position as today), so the app's current
  item and other stored item indices stay valid. Id unchanged.
- `Review::restore` is keyed by entry index, not item index.
- `save_progress` keeps file order (no sort); unmatched entries stay put.

**Accepted exception to rule 2.** `save_progress` reserializes the file
(comments and wrapping lost), as it does today. Splicing STATUS/CHOSEN/MINE
into the original text is future work.

**Given up.** Inserting, deleting or reordering entries by hand in the middle
of a file shifts later keys. Strictly better than today, where any edit loses
everything. Content-hash keys may come later.

**Known reviews on resume.** A review whose id the session already knows gets
no `apply_saved` (as today): the session wins; if the file's STATUS lines
disagree with the session's statuses, a note says so.

## B. `PARA:` and live matching

`PARA: 341` (added to `FIELD_RE`), serialized after BEFORE. Tools always write
PARA together with BEFORE (live text, trimmed).

`Review::open` gains `live: &Script` and matches in two passes, tracking used
paragraphs by `ParaId` across both:
1. **PARA pass.** Honoured when the paragraph exists in the live doc, is
   editable, and either `normalize(BEFORE)` equals its original or its live
   text, or the entry was already decided: the live text equals one of the
   entry's AFTERs or its MINE, or the doc already has meta for this entry's key.
2. **BEFORE pass** for everything else: against the original, as today. No
   second BEFORE-vs-live pass (pasted files are written against the original;
   tools always write PARA). A match whose paragraph was deleted live is
   unmatched.

`Item::index` = live index. `Item::before` = the text BEFORE was matched
against: the original text, or the live text (edge whitespace kept) when only
live matched; never the post-decision text (a decided entry gets the
original). Options are filtered (`looks_same`) and edge-whitespaced against
`Item::before`.

**New `resume`:**
```rust
pub struct ReviewSource { pub name: String, pub path: String, pub text: String }
pub fn resume(sources: Vec<ReviewSource>, script: Script, script_contents: &str, saved: Option<&str>,
              is_caps: &dyn Fn(&ElementKind) -> bool, script_name: &str) -> Opened;
```
With a v2 `base`: replay base + events (needs no reviews) → open and match
reviews against (original, live) → `apply_saved` for reviews the session
didn't know. Fresh or v1: live = original; open reviews, then build the base
from their saved decisions as today. App File › Add review passes the live doc
too.

Build B first among the risky parts. **The v2 fixture is generated with
today's code before any change.**

## C. New crate `crates/tools` (`scriptorium-tools`, pure; depends on core, review)

```rust
pub struct ToolSpec { pub name: String, pub description: String, pub schema: Value }
pub fn catalogue() -> Vec<ToolSpec>;   // base schemas; MCP adds `script`, `review_file`, `before` itself
pub struct Ctx<'a> { pub doc: &'a Document, pub session: &'a Session, pub script_name: &'a str,
    pub is_caps: &'a dyn Fn(&ElementKind) -> bool,
    pub default_review: Option<usize> }  // host decides: agent → the thread's review; MCP → the only attached
pub fn call(ctx: &Ctx, name: &str, args: &Value) -> Result<Outcome, ToolError>;
pub struct Outcome { pub text: String, pub effects: Vec<Effect> }  // text final for read tools
pub enum Effect { Append { target: Target, entries: Vec<Entry>, failed: Vec<Failed> } }
pub enum Target { Default, Review(usize), File(String) }
pub struct Failed { pub index: usize, pub paragraph_id: u64, pub error: String }
pub enum ToolError { Unknown(String), Args(String), Invalid(String) }   // Display + Error
pub fn apply(session: &mut Session, doc: &Document, r: usize, entries: Vec<Entry>, failed: Vec<Failed>,
    is_caps: …) -> Result<Applied, ToolError>;  // Applied { file_text, numbers: Vec<Option<usize>>, message }
pub fn script_size(doc: &Document) -> (usize /*chars*/, usize /*≈tokens*/);
```
For suggest tools, `call` returns validation errors and effects; the host
resolves the target and calls `apply`, and `Applied.message` (which knows the
numbering and unmatched/invisible results) is the tool result. Targets:
`Default` with no review yet → agent host creates `<thread title>.review.txt`
with `Id: thread-<id>` and attaches it; `File` → MCP host opens/creates and
attaches it (auto-attach). `crates/agent/src/tools.rs` drops its catalogue;
agent keeps/re-exports a `ToolSpec` type and receives specs as values.

| Tool | Args | Returns |
|---|---|---|
| `script_info` | – | today's `info` text + `reviews N` |
| `list_paragraphs` | contains, scene, speaker, kind, offset, limit | JSON `{total, offset, more, paragraphs:[{id,kind,scene,speaker,text}]}` |
| `read_scene` | `scene` (1-based int or heading substring) or `paragraph_id` | header `SCENE 12 of 87 (¶340–¶372)`, then per paragraph `¶<id> ` + compact layout: scene headings and character cues in caps on their own line, dialogue indented 2 spaces, parentheticals as written; non-editable paragraphs end with ` [not editable]`. Description: "paragraph_id is the number after ¶" |
| `read_script` | – | `film.fdx: 87 scenes, 2914 paragraphs, ≈35k tokens`, then every scene as `read_scene` |
| `list_reviews` | `summary` (bool) | JSON `[{name, path, id, default, total, pending, accepted, kept, rewritten}]`; summary adds `invisible`, `unmatched:[{block,before}]`, `warnings`. Replaces `review_summary` |
| `list_suggestions` | `review`, `pending_only` | JSON rows as today's `SuggestionRow` plus `entry` (stable file index); default `default_review` |
| `suggest_change` | `paragraph_id` (MCP also `before`), `options` 1–3, `why` | effect `Append` |
| `suggest_changes` | `suggestions:[{paragraph_id, options, why}]` ≤200 | valid ones in one `Append` (one write); always success-shaped `{"added":n,"failed":[{index,paragraph_id,error}]}` |

Validation (both suggest tools): paragraph exists and is editable; no newlines
in options; at least one option visible (`looks_same` with caps); not a
duplicate: identical to a pending item on the same paragraph in the target
review ("already suggested as #n"; no check when the review doesn't exist
yet), or a second item on the same paragraph within one batch. Keep error
strings "give exactly one of paragraph_id or before", "no paragraph N".

MCP-only tools (`attach_reviews`, `detach_reviews`, `decide_suggestion`,
`edit_paragraph`, `undo`, `redo`, `write_script`, and `review_summary` as an
alias of `list_reviews {summary:true}`) stay in `crates/mcp`.

**Numbering.** CLI/MCP `decide <n>` numbers by script order on each open; an
append between `list` and `decide` in another process can shift it. Accepted
for now; `decide` also accepts `e<entry>` (the stable `entry` from the rows).

## D. CLI lib + bin (shell)

`crates/cli/src/lib.rs` (`scriptorium_cli`): `opened` (`Opened { fdx, raw,
session, doc, base, session_path, dir, script_name, given }` with `open`,
`open_one`, `save`, `write_review(path, text)`); `fsio::write_atomic(path,
bytes)` (`.<name>.tmp-<pid>` in the same dir, fsync, rename); `commands` (each
returns `Result<String, String>` = stdout text, notes appended as today).
`main.rs` only prints. All writes (session, review files incl. the attach-time
`Id:` splice, output `.fdx`) via `write_atomic`. `info`/`dump` open the session
read-only and read the live doc. New `scriptorium tool <script> <name> ['<json
args>'] [--review-file f]` runs any tools-crate tool and applies effects
(rule 5).

**MCP in-process is last and optional** for the agent's step 1. If done: MCP
depends on `scriptorium-cli` + `scriptorium-tools`, stops spawning,
`run()`/`cli_binary()` deleted. If dropped: MCP keeps spawning and routes the
shared tools through `scriptorium tool`. Either way tool names and output
texts are kept.

Intentional changes → edits to `crates/mcp/tests/mcp.rs`: expect `read_scene`,
`read_script`, `suggest_changes`; `suggest_change` auto-attaches (a later
`attach_reviews` is a no-op and passes); `options` maxItems 3; review files
gain `Id:`/`PARA:`. Add `mcp_reads_live_text_after_decide`. If in-process:
also drop the `cli()` skip and `SCRIPTORIUM_BIN`.

## E. Work breakdown

| Piece | Owns | Depends on | Checks |
|---|---|---|---|
| P1a ids, PARA field, append | `crates/review/src/{file,session}.rs`, `crates/review/tests/ids.rs` | – (publishes signatures day 0) | `id_header_parses_serializes_and_overrides_hash`, `headerless_id_unchanged`, `para_field_round_trips`, `append_block_keeps_comments_inserts_id`, `append_entry_pushes_item_at_end`, `decide_append_reopen_keeps_both_statuses`, `save_progress_keeps_file_order` (incl. unmatched), `progress_round_trip` (no session → same statuses; with a session nothing applied twice, id unchanged), `restore_by_entry_index`, `duplicate_id_refused` |
| P1b live matching + resume | `crates/review/src/{matching,sessionfile}.rs`, `Review::open`, `crates/review/tests/{live,fixtures/v2}` | P1a | `v2_session_replays_identically` (fixture `tests/fixtures/v2/{sample.fdx, a.review.txt (with STATUS), sample.scriptorium.json}` made with today's code first; assert statuses, text, undo depth, one undo), `v1_session_still_reads`, `para_honoured_when_before_matches`, `para_mismatch_falls_back_to_before`, `para_on_paragraph_created_by_split`, `para_decided_entry_reopens_decided`, `undecided_restores_before`, `suggestion_on_edited_line_matches_after_restart`, `para_on_deleted_paragraph_unmatched`; existing `review.rs` updated only for signatures |
| P2 tools crate | `crates/tools/**`, `crates/agent/src/tools.rs`, root `Cargo.toml` (1 line) | P1a API (stub first) | per-tool unit tests over in-memory Document/Session incl. `read_scene` format, batch partial failure shape, in-batch and pending duplicates |
| P3 CLI lib/bin | `crates/cli/**` | P1b, P2 | `cli.rs` passes; `tool_subcommand`, `atomic_write_leaves_no_temp`, `info_reads_live_doc`, `attach_splices_id`, `decide_by_entry_ref` |
| P4 MCP | `crates/mcp/**` | P3 | `mcp.rs` with only the edits in D |
| P5 app | `crates/app/src/script_view.rs`, `crates/app/Cargo.toml` | P1b, P3 (`write_atomic`) | builds, tests pass, headless smoke test (add a header-less review: file gains `Id:`) |

Order: P1a → (P1b ‖ P2) → P3 → (P4 ‖ P5). Each: fmt, clippy warning-free,
tests, then two reviewers.

## Decided defaults (user may override)

1. File STATUS disagrees with the session on a known review: session wins, with a note.
2. Editing an AFTER by hand: status is rechecked against the live text (may show Rewritten). Fine.
3. MCP `suggest_change` auto-attaches the review file.
4. Two attached files with the same `Id`: refused.
5. `read_script` asks for confirmation in the app only above a threshold
   (≈20k tokens, or a cost set in settings), using `script_size`.

## Review outcome

Two independent reviews of the draft; changes accepted by the orchestrator:
- **No content-hash keys, no session v3, no migration, no `legacy_id`/`seen`.**
  `item.entry` is already a file index, so appends move no key; the real
  losses were the text-hash id and `save_progress` sorting. An `Id:` header
  (spliced in on attach) plus file-order progress fixes both with far less
  risk. Mid-file hand edits still shift keys; accepted.
- **Live matching trimmed.** PARA honoured also for already-decided entries;
  no BEFORE-vs-live pass; `Item::before` never the post-decision text; used
  paragraphs tracked by `ParaId`; deleted paragraphs unmatched. Resume order
  fixed: replay, then match, then `apply_saved`.
- **Index stability.** Appended items go at the end of `items`; `restore` keyed
  by entry; `decide e<entry>` for a stable reference.
- **Tools API.** No `Host` in the pure crate; `default_review` replaces host
  branches; suggest results come from `apply`; batch result always
  success-shaped; duplicate rules defined; `read_scene` format compacted.
- **MCP in-process made optional and last**; CLI lib/bin and atomic writes kept.
- **Work split** into P1a/P1b so the tools crate starts on the small API.
- **Open questions** turned into defaults above.

## Notes from the P1a reviews (for later pieces)

P1a (ids, PARA field, append, file order) is built; design review: good to
build on. Carry these into later briefs:
- **P1b:** `append_entry` matches by BEFORE only and ignores `entry.para`. Put one
  PARA-then-BEFORE matcher in `matching.rs`, used by both `Review::open` and
  `append_entry`, and share the `Item` construction. `restored()` should count
  only decisions that apply.
- **P1b/P5: stale stored text.** `Review` splices appends onto the text it read
  at open. If the CLI/MCP appended to the file meanwhile, the app would
  overwrite their entries; `save_progress` over the same file doesn't update it
  either. Add `Review::text()`; the shell compares it with the file on disk
  before writing and reloads (or refuses) if they differ.
- **P3/P5: write order.** `append_entry` updates memory first. Shells write the
  review file atomically first, then the session; on a failed write, reload.
  The `Id:` splice currently uses plain `std::fs::write` (app `script_view.rs`,
  CLI `Opened::save`): switch to `write_atomic`.
- Minor: `suggest` hashes a reserialized file for a new file's id, not the
  text written (spec says the text written).

## Notes from the P2 reviews (fixes for the tools crate, after P1b lands)

P2 (`crates/tools`) is built; both reviews done. Fix list for the next builder:
1. **Wrong-paragraph bug (high).** `apply` must re-check duplicates against the
   resolved review (items on the paragraph) and against earlier proposals in the
   same call, then call review's `append_entries(..., para_only = true)` once.
   Failed or unmatched proposals are never written.
2. **`before` lookup:** use `matching::match_entries` (respects `is_caps`, used
   paragraphs, script order) instead of `find_by_text`. With several exact
   matches, return an error listing their ids and asking for `paragraph_id`.
3. **Results:** `apply`'s message gives `#n` and the stable `e<entry>`.
4. **`Outcome` becomes an enum:** `Text(String) | Append(Append)`.
5. **`read_scene`:** capitals from `ctx.is_caps`, not hardcoded; skip empty
   paragraphs; `scene: ""` is an error; a heading that is all digits counts as
   a number only if it's a valid scene index, otherwise it's heading text
   (document it); reject U+2028/U+2029 like newlines.
6. **A batch whose proposals all fail inside apply** still returns the
   success-shaped JSON.
7. **Duplicate rule:** keep one suggestion per paragraph per review (the
   matcher can show only one), but say why in the error: "this conversation's
   review already has a suggestion on ¶N (status); the writer can ask for a new
   one in a new conversation". Freeing decided paragraphs is later work.
8. **MCP:** the base `suggest_change` schema requires `paragraph_id`; MCP must
   relax that to "exactly one of paragraph_id or before".

## Host contract for P3/P4/P5 (from the tools-crate design re-review)

The tools API is ready; no breaking changes needed. Every host follows this:
- **Only the host writes files.** Save `Applied.file_text` only when `Some`
  (`None` = nothing written).
- **Before `apply`,** compare `Review::text()` with the file on disk; if they
  differ, reopen that review, then `apply` (don't re-run `call`; `apply`
  re-validates against the review it's given).
- **Write order:** review file first (`write_atomic`), then the session file.
  On a failed write, reload.
- An `Outcome::Text` (including a batch with nothing valid) is final; don't
  call `apply` for it.
- Small additions, done in P3: make `find_review` public (resolve
  `Target::File` by canonical path, not basename); add
  `add_host_fields(spec, &[..])` beside `add_before`; move the system prompt to
  `tools::prompt::system(script)` with a test `unknown_tools(SYSTEM,
  &catalogue())`, naming `read_scene`, `read_script`, `suggest_changes`. When
  review's `Unplaced` reasons land, `apply` uses them to replace its final
  tuple matching (keep the `check_line`/`check_duplicate` pre-checks for their
  messages).
- **P3 `tool` subcommand:** `--review-file` → `review_file` arg;
  `default_review` = the single attached review; `Target::Own` → error "give
  --review-file".
- **P4 MCP:** MCP-only tools handled first; `review_summary` =
  `list_reviews {summary:true}`; strip `script`; `Own` → same error; `File` →
  open or create the file, pin `Id:`, `Session::add`, then `apply`.
- **P5 app:** the agent never writes the session file. `Own` → build the review
  text with `Id: thread-<id>` in memory, `Session::add`, `apply`; write the
  file only if `file_text` is `Some`, otherwise remove the review again.
  `script_size` drives the `read_script` confirmation.

## Tools crate, round 3 fixes (do with the P3 additions above)

From the correctness re-review (no high-severity findings):
1. `before` fuzzy path can pick the wrong line: collect every candidate at or
   above `FUZZY_THRESHOLD` (compare exact matches ignoring case for caps
   elements); if more than one, or the top two are close, error listing ids;
   when fuzzy, include the matched text in the result.
2. An invisible entry is written to the review in memory but `apply` returns
   `Err`, so a host won't save: return `Ok` with `file_text` and a message (or
   carry `file_text` in the error).
3. `apply` must require `normalize(live) == normalize(entry.before)` so a
   changed line is never written via `match_para`'s decided() fallback.
4. `Target::Review(usize)` is a position: carry the review id and check it in
   `apply`.
5. Nit: `scene: "007"`/`"+3"` should be heading text, not a number.
- (added after the review-crate fix round) **Raw text only:** `ReviewSource.text`
  and any text compared with `Review::text()` must be the file's raw contents
  (no trimming or line-ending conversion), or the hash and disk checks never
  match. **`Review::wrote(text)`** is called only by the shell, only after it
  has saved `save_progress` output over this review's own path with
  `write_atomic`.

## Review crate: pending small changes (before P3)

1. `Unplaced::Taken { entry: usize, item: Option<usize> }` so hosts can name
   the stable `e<entry>` that claimed the paragraph.
2. `Review::wrote(text)` returns `Result` (error if the entry count changes);
   consider the name `wrote_progress`.

## Prompt edits (accepted, apply with the next tools change)

In `crates/tools/src/prompt.rs`:
1. Replace "Before proposing, check list_suggestions…" with: "Each conversation
   has its own review, holding one suggestion per line. If a proposal is
   refused as a duplicate, or the writer mentions earlier suggestions, check
   list_suggestions."
2. Add to the finished-pass bullet: "Don't repeat the proposed text in your
   reply; the writer sees it in the review."
3. Add to the read_scene bullet: "For an overview, script_info is cheap."
4. Add to the first paragraph: "If the writer asks for feedback rather than
   edits, answer in chat; propose changes when they ask for them."
