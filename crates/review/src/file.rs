//! The plain-text review file, `SCRIPT REVIEW v1` / `v2`.
//!
//! ```text
//! SCRIPT REVIEW v1
//! Script: name.fdx
//! Id: a-stable-token          (optional; identifies the review, see below)
//!
//! ---
//! BEFORE: whole original paragraph
//! PARA: 341            the paragraph's id (optional; written by tools)
//! AFTER: whole corrected paragraph
//! AFTER: optional alternative (repeat AFTER for 2-3 options; first = preferred)
//! WHY: short reason
//! STATUS: pending | accepted | rejected | rewritten     (written when saving progress)
//! CHOSEN: 2            which AFTER was accepted, 1-based (written when saving progress)
//! MINE: the writer's own rewrite                        (written when saving progress)
//! ```
//!
//! Parsing is forgiving because the file often comes from a chat AI: code
//! fences and `#` comments are ignored, keys are case-insensitive, a new
//! `BEFORE` starts a new block even without a `---`, and wrapped lines continue
//! the previous field. Ported from `script-review/src/core/reviewfile.js`.
//!
//! The `Id:` header (`[A-Za-z0-9._-]{1,64}`, honoured only before the first
//! block) names the review independently of its contents, so decisions stored
//! under it survive appends and hand edits. A file without one is identified
//! by a hash of its text (see [`crate::Review::id`]).
//!
//! # Blocks (v2)
//!
//! A suggestion can replace a run of several paragraphs with typed lines:
//!
//! ```text
//! ---
//! BEFORE:
//!   CHARACTER: MARIA
//!   DIALOGUE: I don't know what you want
//!     from me.                      (deeper indent: continues the line above)
//!   CHARACTER: TOMAS
//! PARA: 412, 413, 414               (every id of the run)
//! AFTER:
//!   CHARACTER: MARIA
//!   DIALOGUE: What do you want?
//! AFTER: [cut]                      (the empty option: remove the lines)
//! ```
//!
//! - `BEFORE:`/`AFTER:`/`MINE:` with an empty value start a block only when
//!   the next non-empty line is `TYPE: text`, TYPE in capitals (SCENE HEADING,
//!   ACTION, CHARACTER, PARENTHETICAL, DIALOGUE, TRANSITION, SHOT, GENERAL).
//!   Otherwise the field is a one-line field as in v1 (wrapping included).
//! - Inside a block a line indented deeper than the block's first line
//!   continues the line above. An untyped line at the block's indent is its
//!   own paragraph in `BEFORE` (matched on text; it keeps the matched
//!   paragraph's kind) and makes the entry invalid in `AFTER`/`MINE`.
//! - A block whose lines are not indented past its `BEFORE:`/`AFTER:`/`MINE:`
//!   line (a column-0 block, or a pasted file indented throughout) can't tell
//!   an untyped paragraph from a chat-wrapped line, so there an untyped line
//!   always continues the line above (as wrapping does in v1): a stray extra
//!   paragraph would silently change the run's length, a joined line only
//!   fails to match.
//! - A block ends where the v1 reader ends a field: at any field line
//!   (`BEFORE:`, `PARA:`, `WHY:`…), `---` or code fence, at any indent, so
//!   old and new builds split a file into the same entries (indented pasted
//!   files included). A field indented deeper than the line that opened the
//!   block is still a field, with a warning, never block text. Prose after a
//!   closing fence is not part of the block.
//! - `AFTER: [cut]` / `MINE: [cut]` is the empty list, and only that explicit
//!   spelling is; an empty `AFTER:` with nothing typed below is dropped.
//! - A one-line untyped `BEFORE`/`AFTER`/`MINE` means what it means in v1:
//!   the matched paragraph's own kind. Untyped lines are allowed only there
//!   (an untyped one-line `AFTER` only when `BEFORE` is one line; an untyped
//!   one-line `MINE` whatever `BEFORE`'s length) and as later lines of a
//!   `BEFORE` block. See [`Entry::check`].
//!
//! **Entry indices are decision keys.** A block with `BEFORE` and `AFTER` that
//! doesn't make sense (bad block line, `PARA:` count that doesn't fit,
//! untyped options for several lines, an unknown future form) is never
//! dropped: it stays in its slot as an [`Entry::invalid`] placeholder and is
//! written back verbatim, so every build counts entries the same. A v1 line
//! entry whose `PARA:` lists several ids is not a placeholder: as in v1, the
//! `PARA:` is ignored with a warning and the entry is reviewed.
//!
//! A file holding any valid block is written as `SCRIPT REVIEW v2`; one
//! without (placeholders don't count) stays v1, byte for byte as before.

use std::sync::LazyLock;

use regex::Regex;
use scriptorium_core::{ElementKind, ParaId};

pub const FORMAT_HEADER: &str = "SCRIPT REVIEW v1";
/// The header of a file holding block entries.
pub const FORMAT_HEADER_V2: &str = "SCRIPT REVIEW v2";

static FIELD_RE: LazyLock<Regex> =
    // `PARA:` only in capitals: "Para: ..." starts many a wrapped Spanish line.
    LazyLock::new(|| Regex::new(r"^\s*((?i:BEFORE|AFTER|WHY|STATUS|CHOSEN|MINE)|PARA)\s*:\s?(.*)$").unwrap());
static HEADER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\s*(Script|Reviewer|Id)\s*:\s*(.*)$").unwrap());
static ID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._-]{1,64}$").unwrap());
static SEPARATOR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*-{3,}\s*$").unwrap());

/// The marker for an empty list of lines (a cut).
const CUT: &str = "[cut]";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileStatus {
    #[default]
    Pending,
    Accepted,
    Rejected,
    Rewritten,
}

impl FileStatus {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "pending" => Some(Self::Pending),
            "accepted" => Some(Self::Accepted),
            "rejected" => Some(Self::Rejected),
            "rewritten" => Some(Self::Rewritten),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Rewritten => "rewritten",
        }
    }
}

/// One paragraph of a `BEFORE`, `AFTER` or `MINE`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Line {
    /// `None`: untyped, i.e. the matched paragraph's own kind (a one-line
    /// field, or an untyped line in a `BEFORE` block).
    pub kind: Option<ElementKind>,
    pub text: String,
}

impl Line {
    pub fn untyped(text: impl Into<String>) -> Self {
        Line { kind: None, text: text.into() }
    }

    pub fn typed(kind: ElementKind, text: impl Into<String>) -> Self {
        Line { kind: Some(kind), text: text.into() }
    }

    /// The lines' texts as one display string (`" / "` between lines); for
    /// a one-line field, its text.
    pub fn join(lines: &[Line]) -> String {
        lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" / ")
    }
}

/// One block of the file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Entry {
    /// The `PARA:` field: the ids of the paragraphs the suggestion is for,
    /// in order, when the writer of the file knew them (one per `BEFORE`
    /// line); empty if not given.
    pub para: Vec<ParaId>,
    pub before: Vec<Line>,
    /// The `AFTER`s, in order; the first is the suggester's preference. An
    /// empty option is a cut (`AFTER: [cut]`).
    pub options: Vec<Vec<Line>>,
    pub why: String,
    pub status: FileStatus,
    /// Which option was accepted, 0-based (the file is 1-based).
    pub chosen: Option<usize>,
    /// `Some(vec![])` is `MINE: [cut]` (the lines are gone).
    pub mine: Option<Vec<Line>>,
    /// Why the block couldn't be read; such an entry is a placeholder that
    /// keeps the later entries' indices (it gets no item), and the fields
    /// above hold whatever could be read.
    pub invalid: Option<String>,
    /// For an [`Self::invalid`] entry: the block as written (without its
    /// `---`), written back unchanged.
    pub verbatim: Option<String>,
}

impl Entry {
    /// The v1 line form: one untyped `BEFORE`, one untyped line per option.
    pub fn single<S: AsRef<str>>(before: &str, options: impl IntoIterator<Item = S>) -> Self {
        Entry {
            before: vec![Line::untyped(before)],
            options: options.into_iter().map(|o| vec![Line::untyped(o.as_ref())]).collect(),
            ..Entry::default()
        }
    }

    /// Whether this is a v1 line entry: valid, at most one `PARA:` id, and
    /// one untyped line in `BEFORE`, in each option and in `MINE` (which may
    /// also be `[cut]`, as v1 progress writes for a line that's gone).
    /// Anything else is a block, and makes the file v2.
    pub fn is_line(&self) -> bool {
        let one = |ls: &[Line]| matches!(ls, [Line { kind: None, .. }]);
        self.invalid.is_none()
            && self.para.len() <= 1
            && one(&self.before)
            && self.options.iter().all(|o| one(o))
            && self.mine.as_ref().is_none_or(|m| m.is_empty() || one(m))
    }

    /// Whether this entry makes its file `SCRIPT REVIEW v2`: a valid block.
    /// Placeholders ([`Self::invalid`]) don't, whatever they hold.
    fn needs_v2(&self) -> bool {
        self.invalid.is_none() && !self.is_line()
    }

    /// Whether the entry can be written to a review file and read back as
    /// itself; `Err` says why not (the same reasons the parser gives for a
    /// placeholder). Code building entries (tools, `append_entries`) must
    /// refuse an entry that fails this: [`ReviewFile::serialize`] and
    /// [`append_block`] write it anyway, and it reads back as something else.
    ///
    /// Refused: an empty `BEFORE` or no `AFTER`; a `PARA:` whose count isn't
    /// `BEFORE`'s; an untyped line in an `AFTER`/`MINE` of several lines
    /// (e.g. the insertion shape `[anchor untyped, new typed]`: the writer
    /// can't know the anchor's kind, so the caller must type it); an untyped
    /// one-line `AFTER` when `BEFORE` has several lines; an untyped first
    /// line in a several-line `BEFORE`; an untyped block line whose text
    /// would read as something else (empty, `TYPE: text`, a field, `---`, a
    /// fence or `#` comment); a one-line untyped `AFTER`/`MINE` that is empty
    /// or reads as `[cut]`.
    pub fn check(&self) -> Result<(), String> {
        if self.before.is_empty() {
            return Err("BEFORE has no lines".into());
        }
        if self.options.is_empty() {
            return Err("there is no AFTER".into());
        }
        if !self.para.is_empty() && self.para.len() != self.before.len() {
            return Err(format!("PARA lists {} ids but BEFORE has {} lines", self.para.len(), self.before.len()));
        }
        let unwritable = |l: &Line| {
            let t = one_line(&l.text);
            let t = t.trim();
            l.kind.is_none()
                && (t.is_empty()
                    || typed(t, Case::Upper).is_some()
                    || FIELD_RE.is_match(t)
                    || SEPARATOR_RE.is_match(t)
                    || skipped(t))
        };
        if let [first, rest @ ..] = self.before.as_slice()
            && !rest.is_empty()
        {
            if first.kind.is_none() {
                return Err("the first line of a several-line BEFORE needs a TYPE".into());
            }
            if let Some(l) = rest.iter().find(|l| unwritable(l)) {
                return Err(format!("untyped BEFORE line {:?} would read as something else", l.text));
            }
        }
        let lists = self.options.iter().map(|o| ("AFTER", o)).chain(self.mine.iter().map(|m| ("MINE", m)));
        for (name, lines) in lists {
            match lines.as_slice() {
                [Line { kind: None, text }] => {
                    if name == "AFTER" && self.before.len() > 1 {
                        return Err("every AFTER line needs a TYPE when BEFORE has several lines".into());
                    }
                    let t = one_line(text);
                    if (name == "AFTER" && t.trim().is_empty()) || t.trim() == CUT {
                        return Err(format!("{name} {text:?} would read back differently (use [cut] for a cut)"));
                    }
                }
                ls if ls.len() > 1 && ls.iter().any(|l| l.kind.is_none()) => {
                    return Err(format!("every line of a several-line {name} needs a TYPE"));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReviewFile {
    /// The `Script:` header: the file name the review was written for.
    pub script: String,
    /// The `Id:` header, if the file has a valid one.
    pub id: Option<String>,
    pub entries: Vec<Entry>,
    /// Problems found while parsing (skipped blocks, bad values), for the user.
    pub warnings: Vec<String>,
}

/// A `BEFORE`/`AFTER`/`MINE` value as read.
enum Field {
    /// A one-line field (with its wrapped lines).
    Text(String),
    /// A block (or `[cut]`), or why it couldn't be read.
    Lines(Result<Vec<Line>, String>),
}

#[derive(Default)]
struct Block {
    before: Option<Field>,
    para: Option<String>,
    after: Vec<Field>,
    why: Option<String>,
    status: Option<String>,
    chosen: Option<String>,
    mine: Option<Field>,
    /// The block's lines as written, for [`Entry::verbatim`].
    raw: Vec<String>,
}

impl Block {
    /// The one-line value a wrapped line continues, if `key` has one.
    fn text_mut(&mut self, key: &str) -> Option<&mut String> {
        let field = match key {
            "para" => return Some(self.para.get_or_insert_with(String::new)),
            "why" => return Some(self.why.get_or_insert_with(String::new)),
            "status" => return Some(self.status.get_or_insert_with(String::new)),
            "chosen" => return Some(self.chosen.get_or_insert_with(String::new)),
            "before" => self.before.as_mut(),
            "after" => self.after.last_mut(),
            _ => self.mine.as_mut(),
        };
        match field {
            Some(Field::Text(t)) => Some(t),
            _ => None,
        }
    }
}

/// Code fences and `#` comments: ignored everywhere (but a fence ends a
/// block, see [`ends_block`]).
fn skipped(line: &str) -> bool {
    is_fence(line) || is_comment(line)
}

fn is_fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// A line that ends a block: a field, `---` or code fence, at any indent —
/// the lines that end a field in the v1 reader, so both split a file into
/// the same entries.
fn ends_block(line: &str) -> bool {
    FIELD_RE.is_match(line) || SEPARATOR_RE.is_match(line) || is_fence(line)
}

impl ReviewFile {
    pub fn parse(text: &str) -> Self {
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
        let lines: Vec<&str> = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
        let mut out = ReviewFile::default();
        let mut cur: Option<Block> = None;
        let mut last_field: Option<String> = None;
        // Headers count only before the first field line.
        let mut in_body = false;

        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            i += 1;
            if skipped(line) {
                continue;
            }
            if SEPARATOR_RE.is_match(line) {
                out.finish(cur.take());
                last_field = None;
                continue;
            }
            if let Some(m) = FIELD_RE.captures(line) {
                in_body = true;
                let key = m[1].to_lowercase();
                let value = m[2].to_string();
                if key == "before" && cur.as_ref().is_some_and(|b| b.before.is_some()) {
                    out.finish(cur.take());
                }
                let block = cur.get_or_insert_with(Block::default);
                block.raw.push(line.to_string());
                last_field = Some(key.clone());
                let lines_key = matches!(key.as_str(), "before" | "after" | "mine");
                // A fence right after the field is not a block line.
                let next_typed = || {
                    lines[i..]
                        .iter()
                        .find(|l| !l.trim().is_empty() && !is_comment(l))
                        .is_some_and(|l| typed(l, Case::Upper).is_some())
                };
                let field = if lines_key && value.trim().is_empty() && next_typed() {
                    let opener = indent_of(line);
                    let start = i;
                    while i < lines.len() && !ends_block(lines[i]) {
                        i += 1;
                    }
                    if let Some(end) = lines.get(i)
                        && indent_of(end) > opener
                        && let Some(f) = FIELD_RE.captures(end)
                    {
                        out.warnings.push(format!(
                            "Block {}: the indented {}: line ends the {} block; write fields at the indent of {}:.",
                            out.entries.len() + 1,
                            &f[1],
                            key.to_uppercase(),
                            key.to_uppercase(),
                        ));
                    }
                    let body: Vec<&str> = lines[start..i].iter().copied().filter(|l| !is_comment(l)).collect();
                    block.raw.extend(body.iter().map(|l| l.to_string()));
                    last_field = None;
                    Field::Lines(read_lines(body, Case::Upper, key == "before", Some(opener)))
                } else if matches!(key.as_str(), "after" | "mine") && value.trim() == CUT {
                    last_field = None;
                    Field::Lines(Ok(vec![]))
                } else {
                    Field::Text(value)
                };
                match key.as_str() {
                    "before" => block.before = Some(field),
                    "after" => block.after.push(field),
                    "mine" => block.mine = Some(field),
                    _ => *block.text_mut(&key).expect("a text field") = value_of(field),
                }
                continue;
            }
            let Some(block) = cur.as_mut() else {
                if let Some(h) = HEADER_RE.captures(line) {
                    let value = h[2].trim();
                    if h[1].eq_ignore_ascii_case("script") {
                        out.script = value.to_string();
                    } else if h[1].eq_ignore_ascii_case("id") && !in_body {
                        if ID_RE.is_match(value) {
                            out.id = Some(value.to_string());
                        } else {
                            out.warnings.push(format!(
                                "Id \"{value}\" isn't a valid id (letters, digits, . _ -; up to 64); ignored."
                            ));
                        }
                    }
                }
                continue;
            };
            // Lines that are nothing to the reader (chat prose after a block
            // or a closing fence) aren't kept in the verbatim copy either.
            if last_field.is_some() || line.trim().is_empty() {
                block.raw.push(line.to_string());
            }
            // A wrapped line continues the previous field.
            if let Some(key) = &last_field
                && !line.trim().is_empty()
                && let Some(target) = block.text_mut(key)
            {
                target.push(' ');
                target.push_str(line.trim());
            }
        }
        out.finish(cur);
        out
    }

    fn finish(&mut self, block: Option<Block>) {
        let Some(b) = block else { return };
        let n = self.entries.len() + 1;
        let mut errors: Vec<String> = vec![];
        let lines_of = |name: &str, f: Field, errors: &mut Vec<String>| match f {
            Field::Text(t) => Some(vec![Line::untyped(t.trim())]),
            Field::Lines(Ok(ls)) => Some(ls),
            Field::Lines(Err(e)) => {
                errors.push(format!("{name}: {e}"));
                None
            }
        };
        let has_before = b.before.is_some();
        // An empty one-line AFTER is no option (as in v1).
        let afters: Vec<Field> =
            b.after.into_iter().filter(|f| !matches!(f, Field::Text(t) if t.trim().is_empty())).collect();
        if !has_before || afters.is_empty() {
            if has_before || !afters.is_empty() {
                let missing = if has_before { "AFTER" } else { "BEFORE" };
                self.warnings.push(format!("Block {n} is missing {missing}; skipped."));
            }
            return;
        }
        let before = b.before.and_then(|f| lines_of("BEFORE", f, &mut errors)).unwrap_or_default();
        let options: Vec<Vec<Line>> = afters.into_iter().filter_map(|f| lines_of("AFTER", f, &mut errors)).collect();
        let mine = b.mine.and_then(|f| lines_of("MINE", f, &mut errors));
        let mut para = b.para.as_deref().map_or_else(Vec::new, |p| {
            let ids: Option<Vec<ParaId>> = p
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.parse::<u64>().ok().map(ParaId))
                .collect();
            ids.unwrap_or_else(|| {
                self.warnings.push(format!("PARA \"{}\" is not a list of paragraph ids; ignored.", p.trim()));
                vec![]
            })
        });
        if errors.is_empty() {
            let line_entry =
                Entry { before: before.clone(), options: options.clone(), mine: mine.clone(), ..Entry::default() };
            if para.len() > 1 && line_entry.is_line() {
                // v1 read this as a bad PARA and reviewed the line anyway.
                self.warnings.push(format!(
                    "PARA \"{}\" lists several ids for one BEFORE line; ignored.",
                    b.para.as_deref().unwrap_or_default().trim()
                ));
                para.clear();
            }
            let candidate = Entry { para: para.clone(), ..line_entry };
            if let Err(e) = candidate.check() {
                errors.push(e);
            }
        }
        let why = b.why.unwrap_or_default().trim().to_string();
        if let Some(reason) = errors.into_iter().next() {
            self.warnings.push(format!("Block {n} can't be read ({reason}); kept as written, not reviewed."));
            let mut raw = b.raw;
            while raw.last().is_some_and(|l| l.trim().is_empty()) {
                raw.pop();
            }
            self.entries.push(Entry {
                para,
                before,
                options,
                why,
                invalid: Some(reason),
                verbatim: Some(raw.join("\n")),
                ..Entry::default()
            });
            return;
        }
        let status = match b.status.as_deref() {
            None => FileStatus::Pending,
            Some(s) => FileStatus::parse(s).unwrap_or_else(|| {
                self.warnings.push(format!("Unknown STATUS \"{}\"; treated as pending.", s.trim()));
                FileStatus::Pending
            }),
        };
        let chosen = b.chosen.as_deref().and_then(|c| {
            let n = c.trim().parse::<usize>().ok().filter(|n| (1..=options.len()).contains(n));
            if n.is_none() {
                self.warnings.push(format!("CHOSEN \"{}\" is not a valid option number; ignored.", c.trim()));
            }
            n.map(|n| n - 1)
        });
        self.entries.push(Entry { para, before, options, why, status, chosen, mine, invalid: None, verbatim: None });
    }

    /// Write the file back out. Round-trips through [`ReviewFile::parse`]
    /// (comments and wrapping are not kept; invalid blocks are written as
    /// they were read). `SCRIPT REVIEW v2` if any entry is a valid block (see
    /// [`Entry::is_line`]; placeholders don't count), else v1. Entries built
    /// in code must pass [`Entry::check`] to read back as themselves.
    pub fn serialize(&self) -> String {
        let header = if self.entries.iter().any(Entry::needs_v2) { FORMAT_HEADER_V2 } else { FORMAT_HEADER };
        let mut out = vec![header.to_string()];
        if !self.script.is_empty() {
            out.push(format!("Script: {}", self.script));
        }
        if let Some(id) = &self.id {
            out.push(format!("Id: {id}"));
        }
        out.push(String::new());
        for e in &self.entries {
            write_entry(e, &mut out);
        }
        out.push(String::new());
        out.join("\n")
    }
}

fn value_of(f: Field) -> String {
    match f {
        Field::Text(t) => t,
        Field::Lines(_) => String::new(),
    }
}

fn one_line(s: &str) -> String {
    s.split('\n').map(str::trim).collect::<Vec<_>>().join(" ")
}

/// `KEY: text` for one untyped line, `KEY: [cut]` for none, else `KEY:` and
/// one indented `TYPE: text` (or, in `BEFORE`, untyped `text`) per line.
///
/// Not every list can be written: an untyped line first in a block, or one
/// whose text reads as `TYPE: text`, a field, `---` or a `#` comment, reads
/// back differently. The parser never produces such lists; callers building
/// entries give typed lines.
fn write_lines(key: &str, lines: &[Line], out: &mut Vec<String>) {
    match lines {
        [] => out.push(format!("{key}: {CUT}")),
        [Line { kind: None, text }] => out.push(format!("{key}: {}", one_line(text))),
        _ => {
            out.push(format!("{key}:"));
            for l in lines {
                out.push(match &l.kind {
                    Some(k) => format!("  {}: {}", k.name().to_uppercase(), one_line(&l.text)),
                    None => format!("  {}", one_line(&l.text)),
                });
            }
        }
    }
}

/// One entry as written in the file, starting with its `---` separator.
fn write_entry(e: &Entry, out: &mut Vec<String>) {
    out.push("---".into());
    if let (Some(_), Some(v)) = (&e.invalid, &e.verbatim) {
        out.extend(v.split('\n').map(str::to_string));
        return;
    }
    if e.before.is_empty() {
        out.push("BEFORE: ".into());
    } else {
        write_lines("BEFORE", &e.before, out);
    }
    if !e.para.is_empty() {
        out.push(format!("PARA: {}", e.para.iter().map(|p| p.0.to_string()).collect::<Vec<_>>().join(", ")));
    }
    for o in &e.options {
        write_lines("AFTER", o, out);
    }
    if !e.why.is_empty() {
        out.push(format!("WHY: {}", one_line(&e.why)));
    }
    if e.status != FileStatus::Pending {
        out.push(format!("STATUS: {}", e.status.name()));
    }
    if let (FileStatus::Accepted, Some(c)) = (e.status, e.chosen) {
        out.push(format!("CHOSEN: {}", c + 1));
    }
    if let Some(m) = &e.mine {
        write_lines("MINE", m, out);
    }
}

/// Byte ranges of the header lines (before the first field), with their
/// text without the line ending, skipping fences and comments.
fn header_lines(body: &str) -> Vec<(usize, usize, &str)> {
    let mut out = vec![];
    let mut pos = 0;
    for line in body.split_inclusive('\n') {
        let end = pos + line.len();
        let bare = line.trim_end_matches(['\n', '\r']);
        if FIELD_RE.is_match(bare) {
            break;
        }
        if !skipped(bare) {
            out.push((pos, end, bare));
        }
        pos = end;
    }
    out
}

fn is_format_line(bare: &str) -> bool {
    let t = bare.trim();
    t.eq_ignore_ascii_case(FORMAT_HEADER) || t.eq_ignore_ascii_case(FORMAT_HEADER_V2)
}

/// `text` with an `Id: <id>` header line inserted, unless it already has an
/// `Id:` header. Not a reserialize: one line goes in after the `Script:`
/// header, else after the format line, else at the top; everything else
/// (comments, wrapping, fences, line endings) is kept as it was.
pub fn insert_id(text: &str, id: &str) -> String {
    if ReviewFile::parse(text).id.is_some() {
        return text.to_string();
    }
    let (bom, body) = match text.strip_prefix('\u{FEFF}') {
        Some(rest) => ("\u{FEFF}", rest),
        None => ("", text),
    };
    let eol = if body.contains("\r\n") { "\r\n" } else { "\n" };
    // Byte offsets just past each line in the header (before the first field).
    let (mut script_end, mut format_end) = (None, None);
    for (_, end, bare) in header_lines(body) {
        if let Some(h) = HEADER_RE.captures(bare)
            && h[1].eq_ignore_ascii_case("script")
        {
            script_end.get_or_insert(end);
        } else if is_format_line(bare) {
            format_end.get_or_insert(end);
        }
    }
    let at = script_end.or(format_end).unwrap_or(0);
    // The line before may lack its newline (a file that ends right there).
    let lead = if at > 0 && !body[..at].ends_with('\n') { eol } else { "" };
    format!("{bom}{}{lead}Id: {id}{eol}{}", &body[..at], &body[at..])
}

/// `text` with its format line saying `SCRIPT REVIEW v2`: a v1 line is
/// replaced in place, a file without one gets it at the top. Nothing else
/// changes.
fn upgrade_header(text: &str) -> String {
    let (bom, body) = match text.strip_prefix('\u{FEFF}') {
        Some(rest) => ("\u{FEFF}", rest),
        None => ("", text),
    };
    let eol = if body.contains("\r\n") { "\r\n" } else { "\n" };
    match header_lines(body).into_iter().find(|(_, _, bare)| is_format_line(bare)) {
        Some((_, _, bare)) if bare.trim().eq_ignore_ascii_case(FORMAT_HEADER_V2) => text.to_string(),
        Some((start, _, bare)) => {
            let end = start + bare.len();
            format!("{bom}{}{FORMAT_HEADER_V2}{}", &body[..start], &body[end..])
        }
        None => format!("{bom}{FORMAT_HEADER_V2}{eol}{body}"),
    }
}

/// `text` (a review file's contents) with `entry` added as one block at the
/// end. The rest of the text is kept as it is (comments, wrapping, fences);
/// an `Id: <id>` header is inserted first if the file has none (see
/// [`insert_id`]), and a valid block entry (see [`Entry::is_line`]) turns the
/// format line into `SCRIPT REVIEW v2`. An empty `text` gets the format line
/// first. `entry` must pass [`Entry::check`] to read back as itself.
pub fn append_block(text: &str, entry: &Entry, id: &str) -> String {
    let block = entry.needs_v2();
    let base = match (text.trim().is_empty(), block) {
        (true, false) => format!("{FORMAT_HEADER}\n\n"),
        (true, true) => format!("{FORMAT_HEADER_V2}\n\n"),
        (false, true) => upgrade_header(text),
        (false, false) => text.to_string(),
    };
    let mut out = insert_id(&base, id);
    let eol = if out.contains("\r\n") { "\r\n" } else { "\n" };
    if !out.ends_with('\n') {
        out.push_str(eol);
    }
    let mut lines = vec![];
    write_entry(entry, &mut lines);
    for l in lines {
        out.push_str(&l);
        out.push_str(eol);
    }
    out
}

/// How strictly typed lines read the TYPE before the colon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Case {
    /// Capitals only (`SCENE HEADING`): the review-file grammar.
    Upper,
    /// Any case, spaces or dashes (`scene-heading`, `SceneHeading`): the command line.
    Any,
}

/// `line` as `TYPE: text`, if it is one.
fn typed(line: &str, case: Case) -> Option<(ElementKind, String)> {
    line.trim_start().split_once(':').and_then(|(t, text)| {
        let kind = ElementKind::parse_editable(t)?;
        (case == Case::Any || t == kind.name().to_uppercase()).then(|| (kind, text.trim().to_string()))
    })
}

/// The paragraphs of a block body: one `TYPE: text` per paragraph; blank
/// lines skipped; a line indented deeper than the first continues the
/// previous paragraph (joined with a space); an untyped line at the first
/// line's indent (or less) is its own untyped paragraph if `untyped_ok`,
/// else refused. A body with no paragraphs is refused.
///
/// `opener`: the indent of the `BEFORE:`/`AFTER:`/`MINE:` line, in a review
/// file. When the first line is indented no deeper (a flush block), an
/// untyped line can't be told from a wrapped one, and continues the line
/// above instead.
fn read_lines<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    case: Case,
    untyped_ok: bool,
    opener: Option<usize>,
) -> Result<Vec<Line>, String> {
    let mut out: Vec<Line> = vec![];
    let mut base = None;
    for line in lines.into_iter().filter(|l| !l.trim().is_empty()) {
        let indent = indent_of(line);
        let base = *base.get_or_insert(indent);
        let flush = opener.is_some_and(|o| base <= o);
        if (indent > base || (flush && typed(line, case).is_none()))
            && let Some(last) = out.last_mut()
        {
            if !last.text.is_empty() {
                last.text.push(' ');
            }
            last.text.push_str(line.trim());
            continue;
        }
        match typed(line, case) {
            Some((kind, text)) => out.push(Line::typed(kind, text)),
            None if untyped_ok => out.push(Line::untyped(line.trim())),
            None => {
                let t = line.trim_start().split_once(':').map(|(t, _)| t.trim());
                let capitals = if case == Case::Upper { " in capitals" } else { "" };
                return Err(match t {
                    Some(t) if !t.is_empty() && ElementKind::parse_editable(t).is_none() => {
                        format!("unknown element type {t:?} in block line {:?}", line.trim())
                    }
                    _ => format!(
                        "block line {:?} should be `TYPE: text` with TYPE{capitals} (SCENE HEADING, ACTION, \
                         CHARACTER, PARENTHETICAL, DIALOGUE, TRANSITION, SHOT, GENERAL)",
                        line.trim()
                    ),
                });
            }
        }
    }
    if out.is_empty() {
        return Err("the block has no paragraphs".into());
    }
    Ok(out)
}

/// Parse a standalone block of typed lines (the review file's block grammar,
/// outside a review file): one `TYPE: text` per paragraph, `TYPE` one of
/// SCENE HEADING, ACTION, CHARACTER, PARENTHETICAL, DIALOGUE, TRANSITION,
/// SHOT, GENERAL (capitals only for `Case::Upper`; any case, spaces or dashes
/// for `Case::Any`). Blank lines are skipped; a line indented deeper than the
/// first typed line continues the previous paragraph (joined with a space);
/// an untyped line at the typed indent is refused. `[cut]` alone is the empty
/// list. A block with no paragraphs is refused.
pub fn parse_typed_lines(block: &str, case: Case) -> Result<Vec<(ElementKind, String)>, String> {
    let mut lines = block.lines().filter(|l| !l.trim().is_empty()).peekable();
    if lines.peek().is_some_and(|l| l.trim() == CUT) {
        lines.next();
        return match lines.next() {
            None => Ok(vec![]),
            Some(extra) => Err(format!("[cut] stands alone, but the block goes on with {:?}", extra.trim())),
        };
    }
    let lines = read_lines(lines, case, false, None)?;
    Ok(lines.into_iter().map(|l| (l.kind.expect("untyped lines refused"), l.text)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASIC: &str = "SCRIPT REVIEW v1
Script: PURPLE LIST.fdx

---
BEFORE: I am go to the town tomorrow.
AFTER: I am going to town tomorrow.
WHY: verb form
---
BEFORE: They are playing truco (Argentinian cards game).
AFTER: They are playing truco (Argentinian card game).
";

    fn entry(before: &str, options: &[&str]) -> Entry {
        Entry::single(before, options)
    }

    fn texts(lines: &[Line]) -> Vec<&str> {
        lines.iter().map(|l| l.text.as_str()).collect()
    }

    fn opts(e: &Entry) -> Vec<String> {
        e.options.iter().map(|o| Line::join(o)).collect()
    }

    #[test]
    fn parses_header_and_blocks() {
        let r = ReviewFile::parse(BASIC);
        assert_eq!(r.script, "PURPLE LIST.fdx");
        assert_eq!(r.entries.len(), 2);
        assert_eq!(
            r.entries[0],
            Entry {
                why: "verb form".into(),
                ..entry("I am go to the town tomorrow.", &["I am going to town tomorrow."])
            }
        );
        assert_eq!(r.entries[1].why, "");
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn tolerates_fences_comments_crlf_lowercase_missing_separators_wrapping() {
        let messy = "```\r\nSCRIPT REVIEW v1\r\n# a comment\r\nbefore: One line\r\nafter : Fixed\r\n  continued here\r\nBEFORE: Two\r\nAFTER: Too\r\n# CONTINUE FROM: Three\r\n```\r\n";
        let r = ReviewFile::parse(messy);
        let got: Vec<_> = r.entries.iter().map(|e| (Line::join(&e.before), opts(e))).collect();
        assert_eq!(
            got,
            [("One line".to_string(), vec!["Fixed continued here".to_string()]), ("Two".into(), vec!["Too".into()])]
        );
    }

    #[test]
    fn warns_about_incomplete_blocks_and_unknown_statuses() {
        let r = ReviewFile::parse("---\nBEFORE: only before\n---\nBEFORE: a\nAFTER: b\nSTATUS: maybe\n");
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.entries[0].status, FileStatus::Pending);
        assert_eq!(r.warnings.len(), 2);
    }

    #[test]
    fn reads_saved_progress_fields() {
        let r = ReviewFile::parse(
            "---\nBEFORE: a\nAFTER: b\nSTATUS: Rewritten\nMINE: c\n---\nBEFORE: d\nAFTER: e\nSTATUS: accepted\n",
        );
        let got: Vec<_> = r.entries.iter().map(|e| (e.status, e.mine.clone())).collect();
        assert_eq!(got, [(FileStatus::Rewritten, Some(vec![Line::untyped("c")])), (FileStatus::Accepted, None)]);
    }

    #[test]
    fn serialize_round_trips() {
        let data = ReviewFile {
            script: "X.fdx".into(),
            entries: vec![
                Entry { why: "w".into(), ..entry("a", &["b"]) },
                Entry { status: FileStatus::Rewritten, mine: Some(vec![Line::untyped("mine")]), ..entry("c", &["d"]) },
                Entry { status: FileStatus::Rejected, ..entry("e", &["f"]) },
                Entry { status: FileStatus::Accepted, chosen: Some(2), ..entry("g", &["h1", "h2", "h3"]) },
                Entry {
                    status: FileStatus::Rewritten,
                    mine: Some(vec![]),
                    para: vec![ParaId(4)],
                    ..entry("i", &["j"])
                },
            ],
            id: None,
            warnings: vec![],
        };
        let text = data.serialize();
        assert!(text.starts_with("SCRIPT REVIEW v1\nScript: X.fdx\n"));
        assert!(text.contains("MINE: [cut]\n"), "{text}");
        assert_eq!(ReviewFile::parse(&text), data);
    }

    /// A v1 file comes out exactly as the v1 writer wrote it.
    #[test]
    fn v1_serializes_byte_for_byte() {
        let text = "SCRIPT REVIEW v1\nScript: X.fdx\nId: k\n\n---\nBEFORE: a\nPARA: 7\nAFTER: b\nAFTER: \
                    c\nWHY: w\nSTATUS: accepted\nCHOSEN: 2\n---\nBEFORE: d\nAFTER: e\nSTATUS: rewritten\nMINE: \
                    [cut]\n---\nBEFORE: f\nAFTER: g\nSTATUS: rewritten\nMINE: h\n";
        assert_eq!(ReviewFile::parse(text).serialize(), text);
        assert_eq!(append_block("", &entry("a", &["b"]), "k"), "SCRIPT REVIEW v1\nId: k\n\n---\nBEFORE: a\nAFTER: b\n");
    }

    #[test]
    fn serialize_flattens_newlines() {
        let f = ReviewFile { entries: vec![entry("a\nb", &["c"])], ..ReviewFile::default() };
        assert!(f.serialize().contains("BEFORE: a b\n"));
    }

    #[test]
    fn repeated_after_are_options_and_chosen_is_one_based() {
        let r = ReviewFile::parse(
            "---\nBEFORE: a\nAFTER: one\nAFTER: two\n  wrapped\nAFTER: three\nWHY: w\nSTATUS: accepted\nCHOSEN: 2\n",
        );
        assert_eq!(opts(&r.entries[0]), ["one", "two wrapped", "three"]);
        assert_eq!(r.entries[0].chosen, Some(1));
    }

    #[test]
    fn invalid_chosen_ignored_and_empty_after_dropped() {
        let r = ReviewFile::parse("---\nBEFORE: a\nAFTER:\nAFTER: b\nCHOSEN: 5\n");
        assert_eq!(opts(&r.entries[0]), ["b"]);
        assert_eq!(r.entries[0].chosen, None);
        assert_eq!(r.warnings.len(), 1);
    }

    const BLOCK: &str = "SCRIPT REVIEW v2
Script: X.fdx

---
BEFORE:
  CHARACTER: MARIA
  DIALOGUE: I don't know what
    you want from me.
  CHARACTER: TOMAS
  DIALOGUE: The truth.
PARA: 412, 413, 414, 415
AFTER:
  CHARACTER: MARIA
  DIALOGUE: What do you want?
AFTER: [cut]
WHY: The exchange restates scene 11.
";

    #[test]
    fn typed_block_parses_and_round_trips() {
        use ElementKind::{Character, Dialogue};
        let r = ReviewFile::parse(BLOCK);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let e = &r.entries[0];
        assert_eq!(e.para, [ParaId(412), ParaId(413), ParaId(414), ParaId(415)]);
        assert_eq!(
            e.before,
            [
                Line::typed(Character, "MARIA"),
                Line::typed(Dialogue, "I don't know what you want from me."),
                Line::typed(Character, "TOMAS"),
                Line::typed(Dialogue, "The truth."),
            ]
        );
        assert_eq!(
            e.options,
            [vec![Line::typed(Character, "MARIA"), Line::typed(Dialogue, "What do you want?")], vec![]]
        );
        assert_eq!(e.why, "The exchange restates scene 11.");
        assert!(!e.is_line());
        let text = r.serialize();
        assert!(text.starts_with("SCRIPT REVIEW v2\n"), "{text}");
        assert!(text.contains("\n  DIALOGUE: I don't know what you want from me.\n"), "{text}");
        assert!(text.contains("PARA: 412, 413, 414, 415\n"), "{text}");
        assert!(text.contains("AFTER: [cut]\n"), "{text}");
        assert_eq!(ReviewFile::parse(&text), r);
        assert_eq!(ReviewFile::parse(&text).serialize(), text);
    }

    #[test]
    fn cut_only_when_explicit() {
        let r = ReviewFile::parse("---\nBEFORE: a\nAFTER: [cut]\n---\nBEFORE: b\nAFTER:\nAFTER: c\n");
        assert_eq!(r.entries[0].options, [Vec::<Line>::new()]);
        assert!(!r.entries[0].is_line(), "a cut is a block");
        assert_eq!(opts(&r.entries[1]), ["c"], "an empty AFTER is no option, not a cut");
        let text = r.serialize();
        assert!(text.starts_with(FORMAT_HEADER_V2));
        assert_eq!(ReviewFile::parse(&text).entries, r.entries);
    }

    #[test]
    fn insertion_replaces_the_anchor() {
        let text = "---\nBEFORE: She waits.\nPARA: 22\nAFTER:\n  ACTION: She waits.\n  CHARACTER: TOMAS\n  \
                    DIALOGUE: Well?\n";
        let r = ReviewFile::parse(text);
        let e = &r.entries[0];
        assert_eq!(
            (e.para.as_slice(), e.before.as_slice()),
            ([ParaId(22)].as_slice(), [Line::untyped("She waits.")].as_slice())
        );
        assert_eq!(texts(&e.options[0]), ["She waits.", "TOMAS", "Well?"]);
        assert_eq!(e.options[0][1].kind, Some(ElementKind::Character));
        let out = r.serialize();
        assert!(out.contains("BEFORE: She waits.\nPARA: 22\nAFTER:\n  ACTION: She waits.\n"), "{out}");
        assert_eq!(ReviewFile::parse(&out), r);
    }

    #[test]
    fn mine_block_status_and_chosen() {
        let text = "SCRIPT REVIEW v2\n\n---\nBEFORE:\n  ACTION: a\n  ACTION: b\nPARA: 1, 2\nAFTER:\n  ACTION: \
                    c\nAFTER:\n  ACTION: d\n  SHOT: e\nSTATUS: accepted\nCHOSEN: 2\n---\nBEFORE:\n  ACTION: \
                    f\nAFTER:\n  ACTION: g\nSTATUS: rewritten\nMINE:\n  ACTION: h\n  PARENTHETICAL: (i)\n---\nBEFORE: \
                    j\nAFTER:\n  ACTION: k\nSTATUS: rewritten\nMINE: [cut]\n";
        let r = ReviewFile::parse(text);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!((r.entries[0].status, r.entries[0].chosen), (FileStatus::Accepted, Some(1)));
        assert_eq!(r.entries[0].options.len(), 2);
        let mine = r.entries[1].mine.as_ref().unwrap();
        assert_eq!(*mine, [Line::typed(ElementKind::Action, "h"), Line::typed(ElementKind::Parenthetical, "(i)")]);
        assert_eq!(r.entries[2].mine, Some(vec![]));
        assert_eq!(r.serialize(), text);
    }

    #[test]
    fn untyped_lines_in_before_blocks_are_paragraphs() {
        let r = ReviewFile::parse(
            "---\nBEFORE:\n  CHARACTER: ANA\n  Hola, qué tal.\n    sigue aquí\n  BANANA: x\nAFTER:\n  CHARACTER: ANA\n",
        );
        let e = &r.entries[0];
        assert_eq!(e.invalid, None);
        assert_eq!(texts(&e.before), ["ANA", "Hola, qué tal. sigue aquí", "BANANA: x"]);
        assert_eq!(e.before[1].kind, None);
        assert_eq!(ReviewFile::parse(&r.serialize()).entries, r.entries);
    }

    /// Block lines are never mistaken for fields of the file, and a field is
    /// never mistaken for block text: an indented field inside a block ends
    /// it (as in v1, which reads every field line as a field) with a warning.
    #[test]
    fn fields_end_blocks_at_any_indent_with_a_warning_when_deeper() {
        let r = ReviewFile::parse(
            "---\nBEFORE:\n  ACTION: a\n  WHY: not text\nAFTER:\n  ACTION: b\n  PARA: 9\nWHY: w\n  wrapped\n",
        );
        let e = &r.entries[0];
        assert_eq!(e.invalid, None, "{:?}", r.warnings);
        assert_eq!(texts(&e.before), ["a"]);
        assert_eq!(texts(&e.options[0]), ["b"]);
        assert_eq!(e.para, [ParaId(9)]);
        assert_eq!(e.why, "w wrapped");
        assert_eq!(r.warnings.len(), 2, "{:?}", r.warnings);
        assert!(r.warnings[0].contains("indented WHY: line ends the BEFORE block"), "{:?}", r.warnings);
        // Outside blocks, indented fields still count (pasted files).
        let r = ReviewFile::parse("  BEFORE: x\n  AFTER: y\n");
        assert_eq!(r.entries, [entry("x", &["y"])]);
    }

    /// A pasted file indented throughout (fields at 2, block lines at 4):
    /// every block ends at the next field or `---`, as the v1 reader splits it.
    #[test]
    fn indented_pasted_file_keeps_every_entry() {
        let text = "  ---\n  BEFORE:\n    ACTION: a\n  PARA: 1\n  AFTER:\n    ACTION: b\n  ---\n  BEFORE: c\n  AFTER: \
                    d\n  ---\n  BEFORE:\n    CHARACTER: ANA\n    DIALOGUE: Hola.\n  AFTER: [cut]\n  WHY: w\n";
        let r = ReviewFile::parse(text);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(r.entries.len(), 3);
        assert_eq!(r.entries[0].before, [Line::typed(ElementKind::Action, "a")]);
        assert_eq!(r.entries[0].para, [ParaId(1)]);
        assert_eq!(r.entries[1], entry("c", &["d"]));
        assert_eq!(texts(&r.entries[2].before), ["ANA", "Hola."]);
        assert_eq!((r.entries[2].options.as_slice(), r.entries[2].why.as_str()), ([vec![]].as_slice(), "w"));
        // Block lines only one level deeper than a flush-left file still work.
        let r = ReviewFile::parse("BEFORE:\n ACTION: a\n  more\nAFTER:\n ACTION: b\n");
        assert_eq!(r.entries[0].before, [Line::typed(ElementKind::Action, "a more")]);
    }

    /// v1 ignored a `PARA:` it couldn't read and still reviewed the line.
    #[test]
    fn v1_entry_with_several_para_ids_is_still_a_line() {
        let r = ReviewFile::parse("SCRIPT REVIEW v1\n\n---\nBEFORE: a\nPARA: 3, 4\nAFTER: b\n");
        assert_eq!(r.entries, [entry("a", &["b"])]);
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert!(r.serialize().starts_with(FORMAT_HEADER), "stays v1");
        // A block with a PARA count that doesn't fit is still a placeholder.
        let r = ReviewFile::parse("---\nBEFORE: a\nPARA: 3, 4\nAFTER:\n  ACTION: a\n  ACTION: b\n");
        assert!(r.entries[0].invalid.as_ref().unwrap().contains("PARA lists 2 ids"));
    }

    #[test]
    fn check_refuses_entries_that_dont_read_back() {
        use ElementKind::{Action, Character};
        let anchor = Entry {
            para: vec![ParaId(22)],
            before: vec![Line::untyped("She waits.")],
            options: vec![vec![Line::untyped("She waits."), Line::typed(Character, "TOMAS")]],
            ..Entry::default()
        };
        let err = anchor.check().unwrap_err();
        assert!(err.contains("several-line AFTER needs a TYPE"), "{err}");
        // Typing the anchor makes it writable, and it reads back as itself.
        let typed_anchor = Entry {
            options: vec![vec![Line::typed(Action, "She waits."), Line::typed(Character, "TOMAS")]],
            ..anchor.clone()
        };
        assert_eq!(typed_anchor.check(), Ok(()));
        assert_eq!(ReviewFile::parse(&append_block("", &typed_anchor, "k")).entries, [typed_anchor]);
        // An untyped one-line MINE is fine whatever BEFORE's length; an
        // untyped one-line AFTER only for one BEFORE line.
        let two = vec![Line::typed(Action, "a"), Line::typed(Action, "b")];
        let mine = Entry {
            before: two.clone(),
            options: vec![vec![Line::typed(Action, "c")]],
            status: FileStatus::Rewritten,
            mine: Some(vec![Line::untyped("a b")]),
            ..Entry::default()
        };
        assert_eq!(mine.check(), Ok(()));
        assert_eq!(
            ReviewFile::parse(&ReviewFile { entries: vec![mine.clone()], ..Default::default() }.serialize()).entries,
            [mine]
        );
        let join = Entry { before: two.clone(), options: vec![vec![Line::untyped("a b")]], ..Entry::default() };
        assert!(join.check().unwrap_err().contains("needs a TYPE"));
        let bad = [
            Entry { before: vec![Line::untyped("x"), Line::typed(Action, "y")], ..join.clone() },
            Entry { before: vec![Line::typed(Action, "x"), Line::untyped("WHY: y")], ..join.clone() },
            Entry { before: vec![Line::typed(Action, "x"), Line::untyped("ACTION: y")], ..join.clone() },
            Entry { before: vec![Line::typed(Action, "x"), Line::untyped("---")], ..join.clone() },
            entry("a", &["[cut]"]),
            entry("a", &[" "]),
            Entry { para: vec![ParaId(1), ParaId(2)], ..entry("a", &["b"]) },
            Entry { options: vec![], ..entry("a", &["b"]) },
        ];
        for e in bad {
            assert!(e.check().is_err(), "{e:?}");
        }
    }

    #[test]
    fn placeholders_dont_make_the_file_v2() {
        let text = "SCRIPT REVIEW v1\n\n---\nBEFORE: a\nAFTER: b\n---\nBEFORE:\n  ACTION: c\nAFTER:\n  ACTION: \
                    d\n  untyped\n";
        let r = ReviewFile::parse(text);
        assert!(r.entries[1].invalid.is_some());
        let out = r.serialize();
        assert!(out.starts_with("SCRIPT REVIEW v1\n"), "{out}");
        assert_eq!(ReviewFile::parse(&out).entries, r.entries);
        assert!(append_block("SCRIPT REVIEW v1\n", &r.entries[1], "k").starts_with("SCRIPT REVIEW v1\n"));
    }

    /// `---` and code fences end a block at any indent, as they end a field
    /// in v1; chat prose after the closing fence stays out of the block.
    #[test]
    fn separators_and_fences_end_blocks() {
        let r = ReviewFile::parse("---\nBEFORE:\n  ACTION: a\n  ---\nAFTER:\n  ACTION: b\n");
        assert_eq!(r.entries.len(), 0, "the v1 reader splits here too: {:?}", r.entries);
        let r = ReviewFile::parse("```\n---\nBEFORE: a\nAFTER:\n  ACTION: b\n```\nHope this helps!\n");
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.entries[0].options, [vec![Line::typed(ElementKind::Action, "b")]]);
        // Nor does the prose land in a placeholder's verbatim copy.
        let r = ReviewFile::parse("```\nBEFORE: a\nAFTER:\n  ACTION: b\n  oops\n```\nHope this helps!\n");
        assert_eq!(r.entries[0].verbatim.as_deref(), Some("BEFORE: a\nAFTER:\n  ACTION: b\n  oops"));
        // A fence right after AFTER: is no block line.
        let r = ReviewFile::parse("BEFORE: a\nAFTER:\n```\n  ACTION: b\n");
        assert_eq!(opts(&r.entries[0]), ["ACTION: b"]);
    }

    /// In a block not indented past its field line, an untyped line is a
    /// chat-wrapped continuation, never a new paragraph.
    #[test]
    fn flush_blocks_join_wrapped_lines() {
        use ElementKind::{Action, Character, Dialogue};
        let r = ReviewFile::parse(
            "BEFORE:\nACTION: Laura walks into the\nkitchen and stops.\nCHARACTER: ANA\nDIALOGUE: Hola,\nqué tal.\nAFTER:\nACTION: \
             Laura enters\nthe kitchen.\n",
        );
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let e = &r.entries[0];
        assert_eq!(
            e.before,
            [
                Line::typed(Action, "Laura walks into the kitchen and stops."),
                Line::typed(Character, "ANA"),
                Line::typed(Dialogue, "Hola, qué tal."),
            ]
        );
        assert_eq!(e.options, [vec![Line::typed(Action, "Laura enters the kitchen.")]]);
        assert_eq!(ReviewFile::parse(&r.serialize()).entries, r.entries);
    }

    #[test]
    fn malformed_blocks_keep_their_slot() {
        let text = "SCRIPT REVIEW v1\n\n---\nBEFORE: a\nAFTER: b\n---\nBEFORE:\n  ACTION: c\nAFTER:\n  ACTION: d\n  \
                    untyped\n# a comment\n\n---\nBEFORE:\n  ACTION: e\n  ACTION: f\nPARA: 3\nAFTER:\n  ACTION: \
                    g\n---\nBEFORE:\n  ACTION: h\n  ACTION: i\nAFTER: j\n---\nBEFORE: k\nAFTER:\n  LYRICS: \
                    la\n  ACTION: l\n---\nBEFORE: m\nAFTER: n\n";
        let r = ReviewFile::parse(text);
        assert_eq!(r.entries.len(), 6, "every block keeps its index");
        let invalid: Vec<bool> = r.entries.iter().map(|e| e.invalid.is_some()).collect();
        assert_eq!(invalid, [false, true, true, true, false, false]);
        assert_eq!(r.warnings.len(), 3, "{:?}", r.warnings);
        assert!(r.warnings[0].contains("Block 2"), "{:?}", r.warnings);
        assert!(r.entries[2].invalid.as_ref().unwrap().contains("PARA lists 1 ids but BEFORE has 2"));
        assert!(r.entries[3].invalid.as_ref().unwrap().contains("needs a TYPE"));
        assert_eq!(r.entries[1].verbatim.as_deref(), Some("BEFORE:\n  ACTION: c\nAFTER:\n  ACTION: d\n  untyped"));
        // An unknown TYPE in an AFTER that doesn't start a block is plain text (v1 meaning).
        assert_eq!(opts(&r.entries[4]), ["LYRICS: la ACTION: l"]);
        assert_eq!(r.entries[5], entry("m", &["n"]));
        // Written back verbatim, in place; placeholders alone keep the file v1.
        let out = r.serialize();
        assert!(out.starts_with("SCRIPT REVIEW v1\n"));
        assert!(out.contains("---\nBEFORE:\n  ACTION: c\nAFTER:\n  ACTION: d\n  untyped\n---\n"), "{out}");
        let again = ReviewFile::parse(&out);
        assert_eq!(again.entries, r.entries);
        assert_eq!(again.serialize(), out);
    }

    #[test]
    fn unknown_future_block_round_trips() {
        let text = "---\nBEFORE:\n  ACTION: a\nAFTER:\n  ACTION: b\n  LYRICS: la la\nWHY: new\n";
        let r = ReviewFile::parse(text);
        assert!(r.entries[0].invalid.as_ref().unwrap().contains("LYRICS"));
        assert_eq!(r.entries[0].why, "new");
        assert!(
            r.serialize().ends_with("\n---\nBEFORE:\n  ACTION: a\nAFTER:\n  ACTION: b\n  LYRICS: la la\nWHY: new\n")
        );
    }

    #[test]
    fn chat_pasted_wrapped_before_still_reads() {
        let text = "```\nSCRIPT REVIEW v1\nScript: X.fdx\n\nBEFORE: Laura walks into the\nkitchen and stops.\nAFTER: \
                    Laura enters the kitchen.\nBEFORE:\n  Two words\nAFTER:\nMore words\n  AFTER: Fine\n```\n";
        let r = ReviewFile::parse(text);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert_eq!(
            r.entries,
            [
                entry("Laura walks into the kitchen and stops.", &["Laura enters the kitchen."]),
                entry("Two words", &["More words", "Fine"]),
            ]
        );
        assert!(r.serialize().starts_with(FORMAT_HEADER));
    }

    #[test]
    fn append_block_upgrades_the_header_for_blocks() {
        let block = Entry {
            para: vec![ParaId(1), ParaId(2)],
            before: vec![Line::typed(ElementKind::Action, "a"), Line::typed(ElementKind::Action, "b")],
            options: vec![vec![]],
            ..Entry::default()
        };
        let out = append_block("SCRIPT REVIEW v1\r\nId: k\r\n\r\n---\r\nBEFORE: x\r\nAFTER: y\r\n", &block, "k");
        assert_eq!(
            out,
            "SCRIPT REVIEW v2\r\nId: k\r\n\r\n---\r\nBEFORE: x\r\nAFTER: y\r\n---\r\nBEFORE:\r\n  ACTION: a\r\n  ACTION: \
             b\r\nPARA: 1, 2\r\nAFTER: [cut]\r\n"
        );
        assert_eq!(ReviewFile::parse(&out).entries[1], block);
        assert!(append_block("", &block, "k").starts_with("SCRIPT REVIEW v2\nId: k\n"));
        // A headerless file gets the header on top; the id stays pinned.
        let out = append_block("BEFORE: x\nAFTER: y\n", &block, "k");
        assert!(out.starts_with("SCRIPT REVIEW v2\nId: k\nBEFORE: x\n"), "{out}");
        // A line entry leaves the header alone.
        let v1 = "SCRIPT REVIEW v1\nId: k\n\n";
        assert!(append_block(v1, &entry("a", &["b"]), "k").starts_with(v1));
    }
}

#[cfg(test)]
mod typed_lines_tests {
    use super::*;

    #[test]
    fn typed_lines() {
        let got = parse_typed_lines("CHARACTER: MARIA\n\n  \nDIALOGUE: Hi: there.\n", Case::Upper).unwrap();
        assert_eq!(got, [(ElementKind::Character, "MARIA".into()), (ElementKind::Dialogue, "Hi: there.".into())]);
        let got = parse_typed_lines("  ACTION: one\n    more\n  SCENE HEADING: INT. X", Case::Upper).unwrap();
        assert_eq!(got[0].1, "one more");
        assert_eq!(got[1].0, ElementKind::SceneHeading);
    }

    #[test]
    fn refusals_and_cut() {
        assert!(parse_typed_lines("Dialogue: lowercase", Case::Upper).is_err());
        assert!(parse_typed_lines("DIALOGUE: ok\nuntyped", Case::Upper).is_err());
        assert!(parse_typed_lines("BANANA: x", Case::Upper).is_err());
        assert!(parse_typed_lines("  \n", Case::Upper).is_err());
        assert_eq!(parse_typed_lines("[cut]", Case::Upper).unwrap(), vec![]);
        assert!(parse_typed_lines("[cut]\nACTION: x", Case::Upper).is_err());
    }

    #[test]
    fn any_case_accepts_spellings_and_names_unknown_types() {
        let got = parse_typed_lines(
            "  action: a\ndialogue: b\nscene-heading: c\nSCENEHEADING: d\nScene Heading: e",
            Case::Any,
        )
        .unwrap();
        let kinds: Vec<_> = got.iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(
            kinds,
            [
                ElementKind::Action,
                ElementKind::Dialogue,
                ElementKind::SceneHeading,
                ElementKind::SceneHeading,
                ElementKind::SceneHeading
            ]
        );
        assert_eq!(got[1].1, "b");
        let err = parse_typed_lines("BANANA: x", Case::Any).unwrap_err();
        assert!(err.contains("unknown element type \"BANANA\""), "{err}");
        assert!(parse_typed_lines("action: x", Case::Upper).is_err());
    }
}
