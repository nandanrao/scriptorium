# Plan: full script editing, then block suggestions

Status: Phase A (A1–A7) built and reviewed; Final Draft hand test pending; Phase B next. Revised after second review round (2026-11-15). Requested by the
user: suggestions must be able to remove and add lines and change several
paragraphs at once. That needs a script that can be fully edited and saved
first, so the work is two phases. Follows v0.2.0.

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
- First round found: DualDialogue is parsed wrong today; `rebase` can reuse
  ids; `next_id` collides across programs; untyped/indented lines made the
  block grammar ambiguous; restoring by pairing was fragile; before-texts
  came from a log scan. Each is fixed below.

Second round (2026-11-15):
- **Sync moves into Phase A** (A6). Planned as a minimal guard (on a
  fresh-id clash, keep the host's work unsaved, write a set-aside file, ask
  the writer to reload) with renumbering (B4) as a fallback; built as
  renumbering instead (2026-11-15), so B4 is merged into A6 and there is no
  guard or set-aside flow for clashes.
- **Refuse up front** (A3): wrapper members are marked "no structural
  edits" by `to_script`; editor, `Edit`, CLI and tool validation refuse at
  edit time; the writer's refusal stays only as a backstop.
- **Existing line suggestions under structural edits** get a Phase A rule
  (A7).
- **The DualDialogue id-shift claim was wrong** (checked against
  `fdx/src/lib.rs:20`): ids shift only when the first inner child is
  self-closing (A1). Sessions are guarded by a reader version.
- **Templates** use the file's most common attribute set per Type; new XML
  uses the file's line ending, and the hardcoded `\n` in `render_runs` /
  `rewrite_paragraph` (`lib.rs:312,329,334`) is fixed (A2).
- **Split ids**: `Edit::split`, paste and CLI split follow `enter_at`
  (`doc.rs:815`): at offset 0 the new id goes above, at the end below, so
  the original id keeps the original text (A3).
- **CLI for agents** (A5): multi-paragraph `insert`, `split --at`, `remove
  a..b` in live order, `--json`.
- **Plain-text paste** goes through a pure classifier (D6, A3); copying to
  Final Draft is plain text (documented).
- **Line-rewrite "done"** moves to Cmd/Ctrl+Enter with a one-time notice;
  A4 gets a strongest-model reviewer.
- **Block fixes** (Phase B): accepted blocks re-place on reopen from their
  meta value (C1); `before_paras` = the entry's typed BEFORE with its PARA
  ids (C2); entry indices are kept stable by placeholders (C4);
  `restore_run` fallbacks and `replace_run(Run::Empty)` defined (C8);
  accepting over lines another review decided is **refused** (C9); fuzzy
  ambiguity defined (C10).
- **`similarity` is already in `core::text`** (D4) (`core/src/text.rs:37`); only
  `FUZZY_THRESHOLD` (`review/src/matching.rs:18`) moves, which touches
  `review::matching` and `session.rs:28`.
- **Final Draft hand test** is a fixed checklist (end of doc) that gates
  the release.

# Phase A — full script editing

## A1. .fdx reader fix (`crates/fdx`)

`PARA_RE` (`lib.rs:20`) is non-greedy: for
`<Paragraph><DualDialogue><Paragraph Type="Character">…</Paragraph>…</DualDialogue></Paragraph>`
it matches from the outer `<Paragraph>` to the first inner `</Paragraph>`.
Result: a phantom paragraph (the outer's `Type`, usually none) holding the
first inner paragraph's runs, and the wrapper's closing tags owned by no
paragraph.

Fix: replace `PARA_RE` with a scanner over the top-level children of
`<Content>` that finds each `<Paragraph` and its matching close by depth. A
paragraph whose body (after whitespace) starts with `<DualDialogue` is a
**wrapper**: record its span in `FdxFile::wrappers: Vec<Range<usize>>` and
read its inner paragraphs as ordinary paragraphs with
`FdxParagraph::wrapper: Option<usize>`. The wrapper gets no id.

**Ids (C6).** The phantom takes the first inner paragraph's place, so ids
normally don't shift (only that paragraph's kind changes). If the first
inner child is self-closing, the phantom swallows it and the second, and
every later id shifts by one. `SessionFile::new` / `to_file` write
`reader: <version>` into the session file (`#[serde(default)]`, so old files
= reader 1; the new reader is 2). `resume` gets a new parameter carrying
the raw file and the current reader version; with a different stored
version, parse the same raw file with both readers: if every id has the
same text in both, keep the session; otherwise set it aside (as
`Resumed::Discarded` does today) with a notice. As built, the parameter is
`reader_1: Option<&Script>` (the file as reader 1 read it); callers always pass
`Some(legacy_script())`. `same_ids` (the equivalence test) lets an untouched Scene
Heading's text differ between the readers (a heading's text is the one thing
the summary change moves); `touched_ids` names the paragraphs the saved edits
use, which must match exactly. Files without DualDialogue number alike under
both readers, so the check passes trivially. Both readers stay in the
crate for this check (old one behind `legacy_paragraphs()`). Callers
updated: `crates/cli/src/opened.rs`, `crates/app/src/script_view.rs`.
`PARA:` entries that no longer fit fall back to `BEFORE` matching.

**Wrapper membership in the model.** A1 also adds `Script::fixed:
BTreeMap<ParaId, u32>` (with `is_fixed` / `fixed_group`) (`#[serde(default)]`) in `crates/core/src/model.rs`,
recording which wrapper each paragraph belongs to, filled by `to_script`
from `FdxParagraph::wrapper`. A2 and A3 use it (A3 for the edit-time
refusal). Because it records the wrapper, not just a fixed set, an insert
between two adjacent DualDialogue blocks (different wrappers) stays possible.

Tests: fixture with two DualDialogue blocks, one with a self-closing first
child; kinds/texts as expected; no-op write byte-identical; reader
equivalence: normal wrapper → session kept, self-closing first child →
set aside.

## A2. .fdx writer (`crates/fdx`, `write_script`)

**Ids.** File paragraphs have ids `0..n`; ids `≥ n` are new. Fresh ids are never
reused: `Script` gains `id_floor: u64` (`#[serde(default)]`), set to `n` by
`to_script`. `Document::with_meta` sets `next_id = max(id_floor, max id + 1)`;
`rebase` stores `next_id` into `id_floor`, and `rebuild` (`sync.rs:95`) goes
through `initial()`, which keeps it. This fixes the collision where the last
paragraph (id `n-1`) is cut, the document rebased (`doc.rs:200,215` recompute
`next_id` from the live max), and a new paragraph gets id `n-1`, which the
writer would take for the original.

**Exception to "never reused".** `Edit` gets one more primitive, `insert_para_with_id(index,
id, NewPara)`, which inserts a paragraph with a given id that was used
before and is currently absent (error if live or `≥ next_id`). It is used
only by `restore_run` and undo/redo; every other insert takes a fresh id.

**Plan.** Walk the live script. Surviving originals must be increasing, else
`StructureChanged { reason: Moved, para }`. Collect: text edits, retypes, removed originals,
and insertions keyed by anchor (the nearest surviving original before them;
`None` = start of `<Content>`).

**Removing.** Cut `[s, span.end)` where `s` walks back from `span.start` over
spaces/tabs, then one eol, only if those bytes are pure whitespace;
otherwise cut exactly the span.

**Inserting.** After anchor `i`: at `span_i.end` (or the wrapper's end, see
below) write `{eol}{indent}{xml}` per new paragraph, `indent` copied from
the whitespace before `<Paragraph` of `i`. With anchor `None`: write
`{xml}{eol}{indent}` at the start of the first surviving original (or of its
wrapper, if it is inside one); if none survive, `{eol}{indent}{xml}` right
after `<Content>` (indent from the first original, else four spaces).

**Line endings (C7).** `eol` = the file's (`\r\n` if the raw file contains one,
else `\n`), used for all new XML and by `render_runs` / `rewrite_paragraph`,
which hardcode `\n` today (`lib.rs:312,329,334`).

**Splice order.** Edits are `(range, bytes)` sorted by start; at equal start a
zero-width insert comes before a cut (an insert at X precedes a cut starting
at X). Ranges must not overlap (assert).

**New paragraph XML (C7).** Template = the most common set of allow-listed
opening-tag attributes among the file's paragraphs of that `Type` outside
wrappers (ties: the first seen); none → `Type` only. Allow-list, copied as
raw bytes: `Type`, `Alignment`, `FirstIndent`, `LeftIndent`, `RightIndent`,
`Spacing`, `SpaceBefore`, `Leading`. No children but text runs. Runs copy
`Font` and `Size` from the most common first run of that Type, plus
`Style="Bold+Italic+Underline"` from each span; none → bare `<Text>` runs
(escaped).

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

**DualDialogue (C6).** Text edits inside a wrapper are fine. Structural changes
inside are refused at edit time (A3); the writer still returns
`StructureChanged { reason: InDualDialogue, para }` as a backstop (`reason` is
`Moved | Duplicated | InDualDialogue`; `para` is the paragraph at fault). New or
edited text with a control character or line break is refused too
(`BadText`). An insertion anchored on
a wrapper's last paragraph goes after the wrapper's `</Paragraph>`.

**Tests:** no-op byte-identical (fixtures + real file); remove first / middle
/ last / all-but-one; remove a scene heading with SceneProperties; insert at
start / middle / end; insert at start when the first original is in a
wrapper; insert where the next original is removed (splice order); cut last
original then rebase then insert (no id reuse); retype with and without a
template, from Scene Heading; template picks the majority attributes, not
the nearest; styled spans; CRLF file: new and rewritten XML use CRLF;
DualDialogue text edit ok; every output re-parses to the expected kinds and
texts; reordering refused.

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
    pub fn insert_para_with_id(&mut self, index: usize, id: ParaId, p: NewPara) -> Result<(), DocError>; // restore_run/undo only (A2)
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
(`doc.rs:716`), so anything that replaces the whole script inserts before it
removes. `Document::next_id()` becomes a public getter (A6).

**Split ids (C5).** `split` follows `enter_at` (`doc.rs:815`): offset 0 of a
non-empty paragraph inserts the new empty paragraph above; at the end the
new one goes below; in the middle the original keeps the head. Paste and
CLI split use it, so the original id stays with the original text.

**No structural edits (D2).** Uses `Script::fixed` (A1). `Edit`
refuses (`DocError::Fixed`) `remove_para`, `split`, `join` and `set_kind` on
a wrapper member, and `insert_para` between two paragraphs of the *same*
wrapper (between different wrappers, or a wrapper and a plain paragraph, is
allowed). Text splices are
allowed. Replaying `Op`s is not checked (old logs). The editor shows "Dual
dialogue lines can't be added, removed or split here; edit their text, or
change them in Final Draft." CLI and tool validation map the error to the
same message.

New command `paste_fragment(sel, &[NewPara])`: one paragraph → its spans go in
at the cursor; several → split at the cursor, the first fragment's spans join
the head (which keeps its kind unless empty), middle ones are inserted with
their kinds, the last is prepended to the tail.

**Plain-text classifier** (`core::paste::classify(&str) -> Vec<NewPara>`,
pure): blank lines separate nothing but are dropped. Per line, trimmed:
starts with `INT.`/`EXT.`/`EST.`/`INT./EXT.`/`I/E` → Scene Heading; `(…)` →
Parenthetical; ends with `TO:` → Transition; ALL CAPS, ≤ 38 chars, and
followed by a non-blank line → Character; else Dialogue if the previous
line was a Character, Parenthetical or Dialogue (and no blank line
between), otherwise Action. A single line pastes into the current paragraph
unclassified.

Tests: each primitive inverts; undo/redo restores ids; ids never reused after
rebase; split at 0 / middle / end keeps the original id on the original
text; wrapper members refuse each structural primitive, text edits pass; insert between two adjacent wrappers allowed;
paste_fragment cases incl. at start/end and over a multi-paragraph
selection; classifier table (heading, character + dialogue, parenthetical,
transition, action, an ALL-CAPS action line before a blank line).

## A4. Editor (`crates/app/src/editor.rs`)

Core already has Enter split, Backspace/Delete join, multi-paragraph
`delete_selection` and Tab `cycle_kind`; the editor gates them behind
`structural` (`editor.rs:169,226`). Remove the flag and the refusal notice:
structural editing everywhere (refusals from `DocError::Fixed` show as a
notice). Enter always splits; the review line-rewrite "done" moves to
Cmd/Ctrl+Enter (and Esc / moving off as today). (D7) The first time a writer
presses Enter in a line rewrite, show once: "Enter now starts a new line.
Press Cmd+Enter (Ctrl+Enter) when you're done." (remembered in settings).
Copy writes plain text plus JSON metadata
(`ClipboardItem::new_string_with_json_metadata`,
`{"scriptorium":1,"paras":[{kind,spans}]}`); paste uses `paste_fragment`
with the metadata, else with `classify` of the plain text. Copying to Final
Draft is plain text (types are lost); paste from Final Draft goes through
the classifier. Both documented in the user guide. Cut of a multi-paragraph
selection deletes it. Headless smoke test per AGENTS.md. Reviewed by a
strongest-model reviewer.

## A5. CLI parity (D5) (`crates/cli/src/commands.rs`)

New subcommands, each one transaction, saved under the session lock,
printing the affected paragraphs with ids; `--json` prints
`{"new":[ids],"removed":[ids],"paras":[{id,kind,text}]}`:
- `insert <script> (--after <id> | --start | --end) (--block '<TYPE: text
  lines>' | <TYPE> <text>)`: one or more typed paragraphs, the same `TYPE:
  text` grammar as `suggest --block` (B5; capitals per B1, the CLI also takes
  any case); the short form `<TYPE> <text>` is one paragraph. Text can't
  contain newlines. There is no `--before`: use `--after` the previous id.
- `remove <script> <id>[..<id>]`: `a..b` = from `a` to `b` in live order
  (inclusive), not a numeric range; refused if `b` is before `a`.
- `split <script> <id> (--at "<text>" | --offset <n>)`: `--at` splits before
  the first occurrence (refused if empty or absent); `--offset` counts
  characters, converted to bytes. Id rules as A3.
- `join <script> <first> <second>` (`second` must directly follow `first`)
- `retype <script> <id> <TYPE>`
- Each takes a trailing `--json` (only the last argument is the flag).

`TYPE` is an editable element name (case-insensitive). Wrapper members
refuse as in A3. MCP tools for these only when a writer asks (as with
`edit_paragraph`); none now.

## A6. Sync renumbering (D1, C3; was B4) (`crates/review/src/sessionfile/sync.rs`, `with_unsaved`; app reload)

Built as renumbering rather than the planned guard + set-aside file. When
the other program added ids since the host last synced
(`reloaded.next_id() > synced.next_id()`), every id the host gave out since
(`≥ synced.next_id()`; never ids brought back by undo or
`insert_para_with_id`) moves up by the same amount, onto ids the reloaded
document has never used (`Renumber`). The map applies to every unsaved
event: ops, selections, decision meta values (the grammar lives in
`crates/review/src/meta.rs`: `@` lists, `=>` ids, `after:`, `start`) and
`SetMeta` `from`; edits are checked and `net_changes` compares through it.
A paragraph added is placed by its neighbour ids in the reloaded document,
not the stored index: between the same two neighbours if both are still
there and adjacent, by the surviving one if only one is (an end of the
document, or the other program removed the other), else lost. The app
remaps the cursor and anything held by id, and says what another program
changed (including lines it removed). The CLI holds no unsaved work (it
runs under the lock), so this is app-only.

Tests: app splits while the CLI appends (same fresh id) → both kept, the
app's line renumbered; inserts placed by neighbours after renumbering;
neighbours moved apart → lost, not misplaced; one neighbour removed → placed
by the other; both gone → lost; undo/redo and merges of renumbered lines;
decision values on new lines renumbered.

## A7. Existing line suggestions under structural edits (D3) (`crates/review`)

- Pending, paragraph gone → the item keeps its number and place, status
  "line removed"; every decision refused (`can_apply`).
- Decided, paragraph gone → the B3 status rule, pulled forward for N = 1:
  `Status::Rewritten`, shown as "line cut" on every surface
  (`Session::status_label` / `line_label`); progress writes
  `STATUS: rewritten` + `MINE: [cut]`, which reopening never writes in as text.
- Split → the item stays on the head (the original id). It counts as a split
  only if the cut was strictly inside the line's text *as it was then* (the
  pass tracks each paragraph's length through its splices), so Enter at the
  end of a line shortened by a decision or by hand is not a split. A split
  joined back (or undone) is no change.
- Joined (the next line joined onto it) → the item stays; a join undone or
  split off again is no change. The joined-away line is "line removed" /
  "line cut".
- Retyped → outdated if pending; nothing refused; retyped back is no change.
- **Refusal rule** (split or joined): accepting is refused, and so is keeping
  the original over a decision (both would write a whole-line text into part
  of the line); keep-current, rewrite and undecide (text left as is) are
  allowed. `Session::check` returns a `Refusal`; each host phrases the way
  out: the app "press E to rewrite this line", the CLI "use decide <n>
  rewrite <text>", tools the neutral "rewrite it instead". `apply
  --accept-all` leaves such suggestions and lists each one skipped and why.
- Line changes for every paragraph come from one pass over the ops the log
  applied (`Document::for_each_applied_op`, undo/redo included), cached on the
  `Session` until the document changes.

Tests for each, plus join and retype of a suggested line.

# Phase B — block suggestions

**Shape.** Every suggestion replaces a contiguous run of N ≥ 1 live
paragraphs with M ≥ 0 typed paragraphs. 1→1 untyped is today's line
suggestion, unchanged. M = 0 is a cut. An **addition** is a replacement of the
anchor line with `[anchor unchanged, new lines]`. Join, split and retype are
just N→M shapes.

## B1. Review file (`crates/review/src/file.rs`)

Note: `parse_typed_lines(block, Case::Upper|Any)` (built in Phase A for the CLI's `insert --block`) is the seed of this block grammar. B1 grows it into `Line { kind: Option<ElementKind>, text }` (untyped lines allowed in BEFORE) rather than adding a second parser.

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
  (matched on text only; restored to the matched paragraph's own kind, C2),
  refused in AFTER/MINE (warning).
- Inside a block, a field line must start at column 0 to end the block
  (`FIELD_RE` today allows indentation, `file.rs:36`; outside block mode that
  tolerance stays, for pasted files).
- `AFTER: [cut]` is the empty option (a cut), only spelled explicitly.
- `PARA:` lists every id of the run. Repeated `AFTER:` are alternatives.

Types: `Line { kind: Option<ElementKind>, text }`; `Entry { para:
Vec<ParaId>, before: Vec<Line>, options: Vec<Vec<Line>>, why, status, chosen,
mine: Option<Vec<Line>>, invalid: Option<String> }`; `Entry::single(before,
options)` for the line form. An untyped one-line entry takes the
paragraph's own kind.

**Entry indices are decision keys (C4).** A block that has BEFORE and
AFTER/MINE is never dropped: if it doesn't parse, it stays in its slot as an
entry with `invalid: Some(reason)` (no item, shown in the unplaced list), so
old and new builds count entries the same. Unparseable blocks are written
back verbatim.

**Header.** A file with any block entry is written as `SCRIPT REVIEW v2`;
v1 files stay v1. `append_entries` (`session.rs:672`, B3's file) rewrites the header to
v2 when it appends a block entry; that change is owned by B3, not B1. Old builds reading a v2 file lose its
blocks (accepted).

## B2. `replace_run` (`crates/core/src/doc.rs`)

```rust
pub enum Run { Ids(Vec<ParaId>), Empty { after: Option<ParaId> } } // Empty: where a cut was
impl Edit<'_> {
    pub fn replace_run(&mut self, run: Run, new: &[NewPara]) -> Result<Vec<ParaId>, DocError>;
    pub fn restore_run(&mut self, run: Run, before: &[Paragraph]) -> Result<(), DocError>;
}
```
`replace_run(Run::Ids)`: the run must be live and consecutive (else
`Mismatch`). Pair old and new lines: only `diff::opcodes` Equal lines over
(kind, normalized text), or inside a Replace chunk same-kind lines in order
with `similarity ≥ FUZZY_THRESHOLD` (constant moved to `core::text`, next to
`similarity`, in this piece, before B3 touches `matching.rs`); paired lines get a common-ends splice and keep id and styles.
Everything else is `insert_para` + `remove_para`, inserts first. Returns the
run's ids afterwards (empty for a cut).
`replace_run(Run::Empty { after })`: inserts `new` after `after` (or at the
start for `None`), used to switch from a cut to another option; returns the
new ids.

`restore_run` (C8; keep mine / undecide after accept): no pairing. Target = the
`before` paragraphs with their own ids, by position. Run = the live
paragraphs from the first to the last live after id (including lines added
inside); none live → `Run::Empty` whose `after:` anchor, if gone, falls back
to the nearest live predecessor in original order. A before id still live
in the run is set in place (whole-text splice + `set_kind`); missing ones
are inserted with that id (`insert_para_with_id`, the A2 exception); run ids not in `before` are removed; inserts
first.

## B3. Matching and decisions (`crates/review/src/{matching,session}.rs`)

**Matching** (one N-line matcher; N = 1 behaves as today).
- **Decided first (C1):** if the entry's meta value exists and its `@before ids`
  equal the entry's PARA, the item is placed from that value: paras = the
  stored after ids (or the `Run::Empty` anchor), without requiring the
  before ids to be live. Its after ids are reserved (`Taken`/used) before
  other entries match.
- PARA pass: all ids live, consecutive, editable, unused; each line equals its
  original or live text (kinds where typed). Ids `≥ n` have no original:
  they match only on live text + kinds.
- BEFORE pass (C10), against the original: windows of exactly N paragraphs whose
  lines match. Exact first; for N ≥ 2, several exact windows →
  `Unplaced::Ambiguous`. Fuzzy requires **every** line ≥ threshold; with
  several fuzzy windows for N ≥ 2, the best (by mean similarity) wins only
  if it beats every other by ≥ 0.05, else Ambiguous. Matched ids must still
  be live and consecutive, else `Unplaced::Restructured`. BEFORE is the
  fallback for pasted files; tools always write PARA.
- Overlap: within a review items share no paragraph (`Taken`); across
  reviews allowed. An item whose lines another review removed is unmatched
  with status "removed by review X". `Session::can_apply` refuses accepting
  it, and refuses accepting a block over any line another review has
  decided (C9) ("decided in review X; undecide it there first").

`Item` gains `paras: Vec<ParaId>`, typed `before`/`options`, and
`before_paras: Vec<Paragraph>` (C2) = the entry's typed BEFORE lines (untyped
lines take the matched paragraph's own kind) with their
PARA ids (what the suggestion was made against). The original file text is
used only for fuzzy BEFORE matches (ids from the match). Never from a log
scan. Keep mine / undecide restore `before_paras`, so earlier hand edits the
suggestion was made on survive. `Item::para()` = first id.

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

Tests: accepted block reopens placed (before ids gone); its after ids block
another entry; keep mine after hand edits restores the BEFORE lines; cross-
review accept refused; fuzzy tie → Ambiguous, clear winner → placed.

## B4. Sync renumbering — merged into A6

Built as part of A6 (see there).

## B5. Tools, CLI, MCP (`crates/tools`, `crates/cli`, `crates/mcp`)

`suggest_change` / each `suggest_changes` item: exactly one target of
`paragraph_id` | `paragraph_ids` (`[first, last]` inclusive, or the full
list) | `insert_after` (sugar: becomes `[anchor, …options]` on that id).
`options`: strings (single-line, only with `paragraph_id`) or
`[[{kind, text}, …], …]`; the schema states `[]` = cut. Empty paragraphs
(`read_scene` hides them): trimmed at the run's edges, refused inside a
`[first, last]` run ("give the full id list"), allowed in an explicit list.
Validation: run consecutive and editable; no structural change to fixed
paragraphs (A3); ≤ 40 lines a side, ≤ 3 options; no line breaks; at least
one option visible; no overlap; `apply` re-checks the live run. Tools always
write PARA. Prompt: "Use the smallest unit the writer can judge on its own.
Group lines that only make sense together into one suggestion with
`paragraph_ids`. Use `insert_after` to add lines, `[]` to cut."

CLI: `suggest … --paras 412..415 | --insert-after N --block $'CHARACTER:
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
| A1 | reader fix, `Script::fixed`, reader version in session file, resume check, DualDialogue fixtures | `crates/fdx`, `crates/core/src/model.rs`, `sessionfile.rs`, `crates/cli/src/opened.rs`, `crates/app/src/script_view.rs` | – | strongest |
| A3 | `Edit` builder, `id_floor`, wrapper refusal, `insert_para_with_id`, split ids | `crates/core` | – | strongest |
| A2 | writer: id-diff, remove/insert/retype, templates, eol | `crates/fdx` | A1, A3 | strongest |
| A4 | editor: drop `structural`, clipboard, Cmd/Ctrl+Enter notice; `paste_fragment` and the paste classifier (these live in `crates/core`, as `paste_fragment` needs `Edit`'s private `delete_range`; the editor calls them) | `crates/app/src/editor.rs` | A3 | Sonnet + headless smoke; strongest reviewer |
| A5 | CLI `insert/remove/split/join/retype`, `--json` | `crates/cli` | A2, A3 | Sonnet |
| A6 | sync renumbering (B4 merged in), app reload | `sessionfile/sync.rs`, `review/src/meta.rs`, `crates/app/src/{script_view,disk}.rs` | A3 | strongest |
| A7 | line suggestions under structural edits | `crates/review` | A3 | strongest |
| B1 | `Line`/`Entry`, block grammar, placeholders, v2 reader/writer | `crates/review/src/file.rs` | – | strongest |
| B1b | migrate `Entry` callers | tools, cli, mcp, app | B1 | Sonnet |
| B2 | `replace_run`, `restore_run`, move `FUZZY_THRESHOLD` (first, before B3) | `crates/core/src/{doc,text}.rs`, `review::matching` (constant import only) | A3 | strongest |
| B3 | matcher, meta values, decide, status, `can_apply`, `append_entries` v2 header | `crates/review/src/{matching,session}.rs` | B1, B2, A7 | strongest |
| B4 | merged into A6 | – | – | – |
| B5 | tool validation/schema/prompt; CLI/MCP flags | tools (+ cli/mcp) | B3 | strongest + Sonnet |
| B6 | card, dots | `crates/app/src/script_view.rs` | B3 | Sonnet + headless smoke |

Order: A1 ‖ A3 → A2 ‖ A4 ‖ A6 ‖ A7 → A5 → **Final Draft hand test gates
the release**; then B1 ‖ B2 (B2 moves `FUZZY_THRESHOLD` before B3 starts) → B1b, B3 → B5 ‖ B6 (B4 is part of A6). Two reviewers per piece (correctness, design), fresh contexts.

# Final Draft hand test (D8; gate for the Phase A release)

For a non-technical Mac tester. Work on a copy of the script; take a
screenshot of each result.

1. Open the copy in Scriptorium and save without editing. Open it in Final
   Draft: it looks identical.
2. In Scriptorium: add a scene (heading, action, character, dialogue);
   delete a whole scene including its heading; split an action paragraph
   (Enter in the middle of a line); join two dialogue lines (Backspace at
   the start of the second); change an action line to dialogue (Tab);
   italicise a word in a new line. Save.
3. Open in Final Draft: new lines have the right font, indents and
   capitals; scene numbers renumber without gaps or duplicates; page count
   and page breaks look right; the italic is kept; title page, headers and
   Navigator are fine. Also check:
   - inserted lines' indents and spacing match their neighbours of the
     same type;
   - with revision colours on, the new and edited text shows normally;
   - an Action changed to Character takes the Character caps and indent;
   - a numbered Scene Heading changed to Action loses its scene number and
     summary; an Action changed to a Scene Heading gets a number from
     Final Draft;
   - an empty new paragraph opens cleanly (no error, no stray characters);
   - a script saved with Windows line endings (CRLF) comes back the same
     after step 1, and Final Draft opens it after step 2.
4. Save in Final Draft, reopen in Scriptorium: the text matches, no warning.
5. Copy three lines from Final Draft into Scriptorium; note any wrong
   types. Then copy three lines from Scriptorium into Final Draft and note
   the same.
6. With a script that has dual dialogue: editing text inside it works;
   deleting a line inside it is refused with a clear message.

## Open questions

None from the user. For the next review: the classifier's Character rule
(A3), the 0.05 fuzzy margin (B3). (Whether A6 needed renumbering is
settled: it is built as renumbering.)
