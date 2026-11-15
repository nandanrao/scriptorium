# Plan: full script editing, then block suggestions

Status: revised after two reviews and user decisions (2026-11-15), to be
reviewed again. Requested by the user: suggestions must be able to remove and
add lines and change several paragraphs at once. That needs a script that can
be fully edited and saved first, so the work is two phases. Follows v0.2.0.

## Review outcome and decisions

- **Two phases.** A: the script is fully editable and saveable (add, remove,
  split, join, retype, multi-paragraph delete and paste), in the app and the
  CLI. B: block suggestions on top.
- **A suggestion is a recorded edit.** Accepting does exactly what human
  input does: Phase A primitives plus the decision metadata, as one undoable
  transaction. Afterwards it is ordinary script content.
- **Retype is in scope** (Phase A). Additions are replacements of the anchor
  line (`[anchor, new lines]`); no `INSERT` syntax, no `insert_before`.
- **No special block editor.** Rewriting a block = accept the closest option,
  then edit with the normal editor.
- **Script view unchanged** (decided 2026-11-15): dots on covered lines, the
  usual highlight on the current suggestion's lines, the change shown only
  on the card.
- **No Final Draft revision marks** for now. **No locked scene numbers**:
  cutting a scene heading removes it like any paragraph (no `OMITTED`).
- **`[cut]`** is the review-file marker for removing lines (confirmed).
- Reviews found: DualDialogue is parsed wrong today; `rebase` can reuse ids;
  `next_id` collides across programs; untyped/indented lines made the block
  grammar ambiguous; restoring by pairing was fragile; before-texts came from
  a log scan. Each is fixed below.

# Phase A — full script editing

## A1. .fdx reader fix (`crates/fdx`)

`PARA_RE` (`lib.rs:20`) is non-greedy: for
`<Paragraph><DualDialogue><Paragraph Type="Character">…</Paragraph>…</DualDialogue></Paragraph>`
it matches from the outer `<Paragraph>` to the first inner `</Paragraph>`.
Result: a phantom `Type=""` paragraph holding the first character's text, that
character missing, and the wrapper's closing tags owned by no paragraph.

Fix: replace `PARA_RE` with a scanner over the top-level children of
`<Content>` that finds each `<Paragraph` and its matching close by depth. A
paragraph whose body (after whitespace) starts with `<DualDialogue` is a
**wrapper**: record its span in `FdxFile::wrappers: Vec<Range<usize>>` and
read its inner paragraphs as ordinary paragraphs with
`FdxParagraph::wrapper: Option<usize>`. The wrapper gets no id.

Ids shift by one after each DualDialogue in files that have one. Saved
sessions on such files then fail to replay (refused, never overwritten, as
today), and `PARA:` entries fall back to `BEFORE` matching. Accepted; the
PURPLE LIST has no DualDialogue.

Tests: hand-written fixture with two DualDialogue blocks; kinds/texts as
expected; no-op write byte-identical.

## A2. .fdx writer (`crates/fdx`, `write_script`)

**Ids.** File paragraphs have ids `0..n`; ids `≥ n` are new. Ids are never
reused: `Script` gains `id_floor: u64` (`#[serde(default)]`), set to `n` by
`to_script`. `Document::with_meta` sets `next_id = max(id_floor, max id + 1)`;
`rebase` stores `next_id` into `id_floor`, and `rebuild` (`sync.rs:95`) goes
through `initial()`, which keeps it. This fixes the collision where the last
paragraph (id `n-1`) is cut, the document rebased (`doc.rs:200,215` recompute
`next_id` from the live max), and a new paragraph gets id `n-1`, which the
writer would take for the original.

**Plan.** Walk the live script. Surviving originals must be increasing, else
`StructureChanged("moved")`. Collect: text edits, retypes, removed originals,
and insertions keyed by anchor (the nearest surviving original before them;
`None` = start of `<Content>`).

**Removing.** Cut `[s, span.end)` where `s` walks back from `span.start` over
spaces/tabs, then one `\n` (and its `\r`), only if those bytes are pure
whitespace; otherwise cut exactly the span.

**Inserting.** After anchor `i`: at `span_i.end` (or the wrapper's end, see
below) write `{eol}{indent}{xml}` per new paragraph, `eol`/`indent` copied
from the whitespace before `<Paragraph` of `i`. With anchor `None`: write
`{xml}{eol}{indent}` at the start of the first surviving original; if none
survive, `{eol}{indent}{xml}` right after `<Content>` (indent from the first
original, else four spaces).

**Splice order.** Edits are `(range, bytes)` sorted by start; at equal start a
zero-width insert comes before a cut (an insert at X precedes a cut starting
at X). Ranges must not overlap (assert).

**New paragraph XML.** Template = nearest original of the same `Type` outside
a wrapper (backwards from the insertion point, then forwards). Copy only
allow-listed opening-tag attributes, as raw bytes: `Type`, `Alignment`,
`FirstIndent`, `LeftIndent`, `RightIndent`, `Spacing`, `SpaceBefore`,
`Leading`. No children but text runs. Runs copy `Font` and `Size` from the
template's first run, plus `Style="Bold+Italic+Underline"` from each span. No
template → `<Paragraph Type="…">` with bare `<Text>` runs (escaped).

```xml
    <Paragraph Type="Dialogue">
      <Text Font="Courier Final Draft" Size="12">You lied!</Text>
    </Paragraph>
```

**Retype.** Rewrite the `Type` attribute value in place. The other
allow-listed layout attributes are replaced by the new type's template's (or
removed if there is none), so the paragraph doesn't keep the old type's
indents. Leaving Scene Heading also drops `Number` and the
`<SceneProperties>` child. Everything else stays; runs are untouched unless
the text changed too.

**DualDialogue.** Text edits inside a wrapper are fine. Removing, inserting
or retyping inside a wrapper → `StructureChanged("dual dialogue")` in v1. An
insertion anchored on a wrapper's last paragraph goes after the wrapper's
`</Paragraph>`. Removing every paragraph of a wrapper is refused too (v1).

**Tests:** no-op byte-identical (fixtures + real file); remove first / middle
/ last / all-but-one; remove a scene heading with SceneProperties; insert at
start / middle / end; insert where the next original is removed (splice
order); cut last original then rebase then insert (no id reuse); retype with
and without a template, from Scene Heading; styled spans; CRLF; DualDialogue
text edit ok, structure refused; every output re-parses to the expected kinds
and texts; reordering refused.

**Gate:** a Final Draft hand test before release: open the output, check
layout of new and retyped paragraphs, scene numbering, page breaks; save in
Final Draft and re-read.

## A3. Document ops (`crates/core/src/doc.rs`)

One edit API. The private `Tx` becomes the public builder:
```rust
pub struct NewPara { pub kind: ElementKind, pub spans: Vec<Span> }
impl Document {
    pub fn edit<T>(&mut self, sel: Selection, group: Group,
        f: impl FnOnce(&mut Edit) -> Result<(Selection, T), DocError>) -> Result<T, DocError>;
}
impl Edit<'_> {
    pub fn splice(&mut self, para: ParaId, range: Range<usize>, spans: Vec<Span>) -> Result<(), DocError>;
    pub fn insert_para(&mut self, index: usize, p: NewPara) -> Result<ParaId, DocError>; // fresh id
    pub fn remove_para(&mut self, id: ParaId) -> Result<(), DocError>;
    pub fn set_kind(&mut self, id: ParaId, kind: ElementKind) -> Result<(), DocError>;
    pub fn split(&mut self, at: Pos) -> Result<ParaId, DocError>;
    pub fn join(&mut self, first: ParaId, second: ParaId) -> Result<Pos, DocError>;
    pub fn set_meta(&mut self, key: &str, to: Option<&str>) -> Result<(), DocError>;
}
```
Every command (typing, Enter, joins, `set_kind`, `replace_text`, paste, CLI
commands, review decisions) is built from these. `Op` stays as it is,
including `Split`/`Merge`: saved session logs contain them, and they keep the
id rules of the module docs. Errors roll back the whole transaction (as
`transact` does). `RemovePara` already refuses the last paragraph
(`doc.rs:717`), so anything that replaces the whole script inserts before it
removes.

New command `paste_fragment(sel, &[NewPara])`: one paragraph → its spans go in
at the cursor; several → split at the cursor, the first fragment's spans join
the head (which keeps its kind unless empty), middle ones are inserted with
their kinds, the last is prepended to the tail.

Tests: each primitive inverts; undo/redo restores ids; ids never reused after
rebase; paste_fragment cases incl. at start/end and over a multi-paragraph
selection.

## A4. Editor (`crates/app/src/editor.rs`)

Core already has Enter split, Backspace/Delete join, multi-paragraph
`delete_selection` and Tab `cycle_kind`; the editor gates them behind
`structural` (`editor.rs:169,225`). Remove the flag and the refusal notice:
structural editing everywhere. Enter always splits; the review line-rewrite
"done" moves to Cmd/Ctrl+Enter (and Esc / moving off as today). Copy writes
plain text plus JSON metadata (`ClipboardItem::new_string_with_json_metadata`,
`{"scriptorium":1,"paras":[{kind,spans}]}`); paste uses `paste_fragment` when
the metadata is there, else today's plain-text rule (non-blank lines become
paragraphs via Enter). Cut of a multi-paragraph selection deletes it. Headless
smoke test per AGENTS.md.

## A5. CLI parity (`crates/cli/src/commands.rs`)

New subcommands, each one transaction, saved under the session lock, printing
the affected paragraphs with ids:
- `insert <script> (--after <id> | --start) <TYPE> <text>`
- `remove <script> <id>[-<id>]`
- `split <script> <id> <char offset>`
- `join <script> <id>` (joins it with the next paragraph)
- `retype <script> <id> <TYPE>`

`TYPE` is an editable element name (case-insensitive). MCP tools for these
only when a writer asks (as with `edit_paragraph`); none now.

# Phase B — block suggestions

**Shape.** Every suggestion replaces a contiguous run of N ≥ 1 live
paragraphs with M ≥ 0 typed paragraphs. 1→1 untyped is today's line
suggestion, unchanged. M = 0 is a cut. An **addition** is a replacement of the
anchor line with `[anchor unchanged, new lines]`. Join, split and retype are
just N→M shapes.

## B1. Review file (`crates/review/src/file.rs`)

```
---
PARA: 412, 413, 414, 415
BEFORE:
  CHARACTER: MARIA
  DIALOGUE: I don't know what you want from me.
  CHARACTER: TOMAS
  DIALOGUE: The truth.
AFTER:
  CHARACTER: MARIA
  DIALOGUE: What do you want?
AFTER: [cut]
WHY: The exchange restates scene 11.
```

Grammar:
- **Block mode** starts only when `BEFORE:`/`AFTER:`/`MINE:` has an empty
  value and the next non-empty line is indented `TYPE: text`. `TYPE` is one
  of SCENE HEADING, ACTION, CHARACTER, PARENTHETICAL, DIALOGUE, TRANSITION,
  SHOT, GENERAL, in capitals.
- A line indented **deeper** than the typed line continues it. An untyped
  line at the typed lines' indent is its own paragraph: allowed in BEFORE
  (matched on text only), refused in AFTER/MINE (block skipped, warning).
- Inside a block, a field line must start at column 0 to end the block
  (`FIELD_RE` today allows indentation, `file.rs:38`; outside block mode that
  tolerance stays, for pasted files).
- `AFTER: [cut]` is the empty option (a cut), only spelled explicitly.
- `PARA:` lists every id of the run. Repeated `AFTER:` are alternatives.

Types: `Line { kind: Option<ElementKind>, text }`; `Entry { para:
Vec<ParaId>, before: Vec<Line>, options: Vec<Vec<Line>>, why, status, chosen,
mine: Option<Vec<Line>> }`; `Entry::single(before, options)` for the line
form. An untyped one-line entry takes the paragraph's own kind.

**Header.** A file with any block entry is written as `SCRIPT REVIEW v2`;
v1 files stay v1. New builds keep entries they can't parse verbatim when
writing progress. Old builds reading a v2 file lose its blocks (accepted).

## B2. `replace_run` (`crates/core/src/doc.rs`)

```rust
pub enum Run { Ids(Vec<ParaId>), Empty { after: Option<ParaId> } } // Empty: restoring a cut
impl Edit<'_> {
    pub fn replace_run(&mut self, run: Run, new: &[NewPara]) -> Result<Vec<ParaId>, DocError>;
    pub fn restore_run(&mut self, run: Run, before: &[Paragraph]) -> Result<(), DocError>;
}
```
`replace_run`: the run must be live and consecutive (else `Mismatch`). Pair
old and new lines: only `diff::opcodes` Equal lines over (kind, normalized
text), or inside a Replace chunk same-kind lines in order with
`similarity ≥ FUZZY_THRESHOLD` (moved from `review::matching` to
`core::text`); paired lines get a common-ends splice and keep id and styles.
Everything else is `insert_para` + `remove_para`, inserts first. Returns the
run's ids afterwards (empty for a cut).

`restore_run` (keep mine / undecide after accept): no pairing. Target = the
`before` paragraphs with their own ids, by position. A before id still live in
the run is set in place (whole-text splice + `set_kind`); missing ones are
inserted with that id; run ids not in `before` are removed; inserts first.

## B3. Matching and decisions (`crates/review/src/{matching,session}.rs`)

**Matching** (one N-line matcher; N = 1 behaves as today).
- PARA pass: all ids live, consecutive, editable, unused; each line equals its
  original or live text (kinds where typed), or the entry is decided (meta
  exists). Ids `≥ n` have no original: they match only on live text + kinds.
- BEFORE pass, against the original: windows of exactly N paragraphs whose
  lines match. Exact first; for N ≥ 2, several exact windows → unmatched
  (`Unplaced::Ambiguous`). Fuzzy requires **every** line ≥ threshold. Matched
  ids must still be live and consecutive, else `Unplaced::Restructured`.
  BEFORE is the fallback for pasted files; tools always write PARA.
- Overlap: within a review items share no paragraph (`Taken`); across
  reviews allowed. An item whose lines another review removed is unmatched
  with status "removed by review X"; `Session::can_apply` refuses accepting
  it.

`Item` gains `paras: Vec<ParaId>`, typed `before`/`options`, and
`before_paras: Vec<Paragraph>`, taken from the original `Script` given to
`Review::open`/`open_live` (ids `≥ n`: from the live document at open time),
never from a log scan. `Item::para()` = first id.

**Meta values.** `<decision>@<before ids>[=>(<after ids> | after:<id> |
start)]`: `accepted:0@412,413,414,415=>412,2921`, `rejected@412,413`,
`accepted:1@412,413=>after:411` (cut; position only for cuts),
`rewritten@22=>22,2922`. Block entries always get full values, even in legacy
mode. Old values (`accepted:2@7`, no `@`) read as before with after = before.
No session version bump: old builds can't parse the new values and show those
items as pending (accepted, documented).

**Decide** = one `doc.edit` transaction. Accept(k): `replace_run(current run,
option k)` + `set_meta` with the returned ids. Current run = item ids while
pending, stored after ids once decided (a cut: `Run::Empty`). KeepMine /
Undecided: pending → meta only; else `restore_run(current run,
before_paras)`. Rewrite (CLI/MCP only) = `replace_run` with the given lines,
`rewritten`.

**Status.** Accepted(k) when the after ids are live, consecutive and read as
option k (cut: no before id live). Otherwise Rewritten, MINE = live lines
from the first to the last **live** after id, including new lines between. If
no after id is live, MINE = `[cut]`. Never write MINE from a paragraph that is
gone.

## B4. Sync (`crates/review/src/sessionfile/sync.rs`, `with_unsaved`)

`applies` already checks Insert/RemovePara by neighbour ids (`sync.rs:207`)
but replays the stored index. Change: recompute indices from the neighbour
ids in the reloaded document. Fresh ids the host made that the reloaded
document already used (or that are below its `next_id`) are renumbered to the
target's fresh ids, and the mapping is applied to every later unsaved event:
ops, selections, and decision meta values (`=>` lists).

## B5. Tools, CLI, MCP (`crates/tools`, `crates/cli`, `crates/mcp`)

`suggest_change` / each `suggest_changes` item: exactly one target of
`paragraph_id` | `paragraph_ids` (`[first, last]` inclusive, or the full
list) | `insert_after` (sugar: becomes `[anchor, …options]` on that id).
`options`: strings (single-line, only with `paragraph_id`) or
`[[{kind, text}, …], …]`; the schema states `[]` = cut. Empty paragraphs
(`read_scene` hides them): trimmed at the run's edges, refused inside a
`[first, last]` run ("give the full id list"), allowed in an explicit list.
Validation: run consecutive and editable; ≤ 40 lines a side, ≤ 3 options; no
line breaks; at least one option visible; no overlap; `apply` re-checks the
live run. Tools always write PARA. Prompt: "Use the smallest unit the writer
can judge on its own. Group lines that only make sense together into one
suggestion with `paragraph_ids`. Use `insert_after` to add lines, `[]` to
cut."

CLI: `suggest … --paras 412-415 | --insert-after N --block $'CHARACTER:
MARIA\nDIALOGUE: What?'` (repeatable; `--block '[cut]'` = cut); `decide <n>
rewrite-block '<typed lines>'`.

## B6. App (`crates/app/src/{script_view,editor}.rs`)

Script view as today: a dot on every line a block covers (an addition: on the
anchor line); the current suggestion's lines get the usual highlight. The card
shows BEFORE struck in red and AFTER in green, laid out by kind, with a
heading "Cut 4 lines" / "Add 2 lines after ¶22" / "Replace 4 lines with 2".
Keys unchanged (a/1/2/3/e/u/j/k). `e` accepts the highlighted option and
puts the cursor in it; the writer edits with the normal editor and the status
follows from B3.

# Work breakdown

| # | Piece | Owns | After | Model |
|---|---|---|---|---|
| A1 | reader fix, DualDialogue fixture | `crates/fdx` | – | strongest |
| A2 | writer: id-diff, remove/insert/retype, splice order | `crates/fdx` | A1 | strongest |
| A3 | `Edit` builder, `id_floor`, `paste_fragment` | `crates/core` | – | strongest |
| A4 | editor: drop `structural`, clipboard JSON | `crates/app/src/editor.rs` | A3 | Sonnet + headless smoke |
| A5 | CLI `insert/remove/split/join/retype` | `crates/cli` | A2, A3 | Sonnet |
| B1 | `Line`/`Entry`, block grammar, v2 header | `crates/review/src/file.rs` | – | strongest |
| B1b | migrate `Entry` callers | tools, cli, mcp, app | B1 | Sonnet |
| B2 | `replace_run`, `restore_run` | `crates/core/src/doc.rs` | A3 | strongest |
| B3 | matcher, meta values, decide, status | `crates/review/src/{matching,session}.rs` | B1, B2 | strongest |
| B4 | sync index recompute, id renumbering | `sessionfile/sync.rs` | B3 | strongest |
| B5 | tool validation/schema/prompt; CLI/MCP flags | tools (+ cli/mcp) | B3 | strongest + Sonnet |
| B6 | card, dots | `crates/app/src/script_view.rs` | B3 | Sonnet + headless smoke |

Order: A1 → A2 ‖ A3 → A4 ‖ A5 → **Final Draft hand test gates the release**;
then B1 ‖ B2 → B1b, B3 → B4 ‖ B5 ‖ B6. Two reviewers per piece (correctness,
design), fresh contexts.

## Open questions

None from the user. For the next review: the retype attribute rule (A2), the
Enter→Cmd/Ctrl+Enter change for line rewrites (A4), and the ambiguity rule
for N ≥ 2 BEFORE matches (B3).
