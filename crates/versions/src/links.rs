//! The links file: one readable sidecar per pair of versions,
//! `<b stem>.links.txt`, saying which paragraphs of one file line up with
//! which of the other, each paragraph's kind, and what it read when its row
//! was last in sync. Format and rules: `docs/plans/versions.md`, "The links
//! file".
//!
//! Ids are paragraph positions in the bytes a member's `hash:` names
//! (`FdxFile::to_script`); the links file only ever names file ids. This
//! module only parses, checks and writes the text; whether a member's hash
//! matches the bytes open is the caller's business.

use std::collections::{BTreeMap, HashSet};
use std::fmt;

use scriptorium_core::{ElementKind, ParaId};

/// First line of every links file this build reads and writes.
pub const MAGIC: &str = "SCRIPTORIUM LINKS v1";

/// What every links file starts with, whatever its version.
const MAGIC_PREFIX: &str = "SCRIPTORIUM LINKS";

/// The comment `write` puts above `---`, so a person opening the file can
/// read it.
const EXPLANATION: &str = "\
# One row per line: a ids | b ids. Under it, one line per paragraph: its
# kind, then its text when the row was last in sync (\"=\") or when it was
# made a placeholder (\"~\"). Neither: never in sync.
";

/// A parsed links file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Links {
    /// Stable token naming the pair (`[A-Za-z0-9._-]{1,64}`); review
    /// entries refer to it with `ROW:`.
    pub id: String,
    /// `a` and `b`, in order. A `Vec` so N members can come later; v1 holds
    /// exactly two (one pair per links file).
    pub members: Vec<Member>,
    /// In file order; no two rows cross on any side.
    pub rows: Vec<Row>,
    /// Per member (same index as `members`): every listed paragraph, with
    /// its kind and stamp. Exactly the ids the rows list.
    pub paras: Vec<BTreeMap<ParaId, Para>>,
    /// Header lines this build doesn't know, kept in order (key as written)
    /// and written back after the known ones.
    pub extra_headers: Vec<(String, String)>,
}

/// One file of the pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// Plain `.fdx` file name in the pair's folder (no path).
    pub file: String,
    /// Language code (`en`, `es`, …), if given.
    pub language: Option<String>,
    /// `sessionfile::hash` of the bytes the ids refer to.
    pub hash: String,
    /// Set (`version new`) on the file Scriptorium made: the side that is
    /// written in place and gets `ROW:` suggestions. At most one member.
    pub created: Option<String>,
}

/// One row: for each member, its paragraph ids (ascending; may be empty).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    pub cells: Vec<Vec<ParaId>>,
}

/// What the links file records about one listed paragraph. The kind is
/// kept so re-find can pair same-kind lines after the old bytes are gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Para {
    pub kind: ElementKind,
    /// `None`: never in sync.
    pub stamp: Option<Stamp>,
}

/// A paragraph's text as of a moment, normalized (`stamp_text`), one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stamp {
    /// `=`: the text when the row was last in sync.
    Synced(String),
    /// `~`: the text when the paragraph was made a placeholder.
    Placeholder(String),
}

impl Stamp {
    pub fn text(&self) -> &str {
        match self {
            Stamp::Synced(t) | Stamp::Placeholder(t) => t,
        }
    }
}

/// A paragraph's text as the links file stamps it (and as status, `ROW
/// TEXT` and re-find compare it): `core::text::normalize` (whitespace, quote
/// and dash style, ellipsis), upper-cased when `caps` (the paragraph's kind
/// is shown in capitals, `FdxFile::is_caps`), so a Final Draft re-save or
/// smart quotes never count as a change. One line, no edge spaces.
pub fn stamp_text(text: &str, caps: bool) -> String {
    let t = scriptorium_core::text::normalize(text);
    if caps { t.to_uppercase() } else { t }
}

/// Why a links file can't be read or written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinksError {
    /// Reading text: what's wrong and on which line (1-based).
    Parse { line: usize, message: String },
    /// A `Links` value that breaks the rules (from `validate`/`write`).
    Invalid(String),
}

impl fmt::Display for LinksError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinksError::Parse { line, message } => write!(f, "line {line}: {message}"),
            LinksError::Invalid(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for LinksError {}

/// Members per links file in v1: one pair.
const MEMBERS: usize = 2;

/// The header letter of member `index` (`a`, `b`, …).
pub fn member_letter(index: usize) -> char {
    (b'a' + index as u8) as char
}

impl Member {
    /// What the app and tools call this file: its language's name, or the
    /// file stem when it has no language.
    pub fn label(&self) -> String {
        match &self.language {
            Some(code) => language_name(code).to_string(),
            None if self.file.len() >= 4
                && self.file.as_bytes()[self.file.len() - 4..].eq_ignore_ascii_case(b".fdx") =>
            {
                self.file[..self.file.len() - 4].to_string()
            }
            None => self.file.clone(),
        }
    }
}

impl Links {
    /// Index of the member Scriptorium made (`created:`), if any.
    pub fn made(&self) -> Option<usize> {
        self.members.iter().position(|m| m.created.is_some())
    }

    /// Index of the member whose file is `file` (case-insensitive, as on
    /// macOS disks).
    pub fn member_of(&self, file: &str) -> Option<usize> {
        let file = file.to_lowercase();
        self.members.iter().position(|m| m.file.to_lowercase() == file)
    }
}

/// The display name of a language code (`es` → `Español`, `es-MX` →
/// `Español`: only the part before `-` or `_` is looked up). An unknown code
/// is shown as given.
pub fn language_name(code: &str) -> &str {
    let primary = code.trim().split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
    (match primary.as_str() {
        "en" => "English",
        "es" => "Español",
        "fr" => "Français",
        "de" => "Deutsch",
        "it" => "Italiano",
        "pt" => "Português",
        "ca" => "Català",
        "eu" => "Euskara",
        "gl" => "Galego",
        "nl" => "Nederlands",
        "sv" => "Svenska",
        "da" => "Dansk",
        "no" | "nb" => "Norsk",
        "fi" => "Suomi",
        "pl" => "Polski",
        "cs" => "Čeština",
        "ro" => "Română",
        "el" => "Ελληνικά",
        "tr" => "Türkçe",
        "ru" => "Русский",
        "uk" => "Українська",
        "ar" => "العربية",
        "he" => "עברית",
        "hi" => "हिन्दी",
        "ja" => "日本語",
        "ko" => "한국어",
        "zh" => "中文",
        _ => return code,
    }) as _
}

fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The built-in kinds' tokens in text lines. Any other kind is its style
/// name in double quotes.
const KINDS: [(&str, ElementKind); 8] = [
    ("heading", ElementKind::SceneHeading),
    ("action", ElementKind::Action),
    ("character", ElementKind::Character),
    ("parenthetical", ElementKind::Parenthetical),
    ("dialogue", ElementKind::Dialogue),
    ("transition", ElementKind::Transition),
    ("shot", ElementKind::Shot),
    ("general", ElementKind::General),
];

fn kind_token(kind: &ElementKind) -> String {
    if let ElementKind::Other(name) = kind {
        let mut out = String::from("\"");
        for c in name.chars() {
            if matches!(c, '"' | '\\') {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        return out;
    }
    KINDS.iter().find(|(_, k)| k == kind).map(|(t, _)| t.to_string()).unwrap_or_default()
}

/// Read a kind token from the start of `s`; returns it and the rest.
fn parse_kind<'a>(s: &'a str, name: &str) -> Result<(ElementKind, &'a str), String> {
    if let Some(quoted) = s.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = quoted.char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => return Ok((ElementKind::Other(out), &quoted[i + 1..])),
                '\\' => match chars.next() {
                    Some((_, e @ ('"' | '\\'))) => out.push(e),
                    _ => return Err(format!("{name}'s kind: only `\\\"` and `\\\\` may follow `\\`")),
                },
                c => out.push(c),
            }
        }
        return Err(format!("{name}'s kind has no closing `\"`"));
    }
    let len = s.bytes().take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')).count();
    let token = s[..len].to_ascii_lowercase();
    if token.is_empty() {
        return Err(format!("expected {name}'s kind (heading, action, character, dialogue, …) after its id"));
    }
    match KINDS.iter().find(|(t, _)| *t == token) {
        Some((_, k)) => Ok((k.clone(), &s[len..])),
        None => Err(format!(
            "unknown kind `{token}`: heading, action, character, parenthetical, dialogue, transition, shot, \
             general, or another style name in double quotes"
        )),
    }
}

/// A known header key: `Id` or a member field. `None` for an unknown key.
/// A key whose first word is one letter is a member key: bare (`x:`) or
/// with a known field (`x hash:`), it is reserved, never kept as unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Id,
    Member(char, Field),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    File,
    Language,
    Hash,
    Created,
}

fn known_key(key: &str) -> Option<Key> {
    let lower = key.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split_whitespace().collect();
    if parts == ["id"] {
        return Some(Key::Id);
    }
    let letter = match parts.first().map(|p| p.as_bytes()) {
        Some([c]) if c.is_ascii_lowercase() => *c as char,
        _ => return None,
    };
    let field = match parts.get(1..) {
        Some([]) => Field::File,
        Some(["language"]) => Field::Language,
        Some(["hash"]) => Field::Hash,
        Some(["created"]) => Field::Created,
        _ => return None,
    };
    Some(Key::Member(letter, field))
}

#[derive(Default)]
struct PartialMember {
    first_line: usize,
    lines: [usize; 4],
    file: Option<String>,
    language: Option<String>,
    hash: Option<String>,
    created: Option<String>,
}

fn field_index(f: Field) -> usize {
    match f {
        Field::File => 0,
        Field::Language => 1,
        Field::Hash => 2,
        Field::Created => 3,
    }
}

fn err(line: usize, message: impl Into<String>) -> LinksError {
    LinksError::Parse { line, message: message.into() }
}

/// Read a links file. Every rule is checked (as `validate` does); errors
/// carry the line they're on.
pub fn parse(text: &str) -> Result<Links, LinksError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines().enumerate().map(|(i, l)| (i + 1, l));

    match lines.next() {
        Some((_, first)) if first.trim() == MAGIC => {}
        Some((_, first)) if first.trim().starts_with(MAGIC_PREFIX) => {
            return Err(err(1, format!("this build reads `{MAGIC}`, not `{}`", first.trim())));
        }
        _ => return Err(err(1, format!("not a links file: the first line must be `{MAGIC}`"))),
    }

    // Header.
    let mut id: Option<String> = None;
    let mut partial: BTreeMap<char, PartialMember> = BTreeMap::new();
    let mut extra_headers = Vec::new();
    let mut separator = None;
    for (n, line) in lines.by_ref() {
        let t = line.trim();
        if t == "---" {
            separator = Some(n);
            break;
        }
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some((key, value)) = t.split_once(':') else {
            return Err(err(n, format!("expected `key: value` or `---`, found `{t}`")));
        };
        let (key, value) = (key.trim(), value.trim());
        if key.is_empty() {
            return Err(err(n, "a header line with no key before `:`"));
        }
        match known_key(key) {
            Some(Key::Id) => {
                if id.is_some() {
                    return Err(err(n, "a second `Id:` line"));
                }
                if !valid_id(value) {
                    return Err(err(n, format!("`{value}` is not a valid id (letters, digits, `.`, `_`, `-`; 1–64)")));
                }
                id = Some(value.to_string());
            }
            Some(Key::Member(letter, field)) => {
                let known = partial.len();
                if !partial.contains_key(&letter) {
                    if letter > member_letter(MEMBERS - 1) && letter == member_letter(known) {
                        return Err(err(n, format!("one pair per links file for now: `{letter}` names a third file")));
                    }
                    if letter > member_letter(MEMBERS - 1) {
                        return Err(err(n, format!("unknown member letter `{letter}`")));
                    }
                }
                let m = partial.entry(letter).or_insert_with(|| PartialMember { first_line: n, ..Default::default() });
                m.lines[field_index(field)] = n;
                let slot = match field {
                    Field::File => &mut m.file,
                    Field::Language => &mut m.language,
                    Field::Hash => &mut m.hash,
                    Field::Created => &mut m.created,
                };
                if slot.is_some() {
                    return Err(err(n, format!("a second `{key}:` line")));
                }
                *slot = Some(value.to_string());
            }
            None => extra_headers.push((key.to_string(), value.to_string())),
        }
    }
    let Some(separator) = separator else {
        let last = text.lines().count().max(1);
        return Err(err(last, "no `---` line after the header"));
    };
    let Some(id) = id else {
        return Err(err(separator, "the header has no `Id:` line"));
    };

    let mut members = Vec::new();
    let mut member_lines = Vec::new();
    for i in 0..MEMBERS {
        let letter = member_letter(i);
        let Some(m) = partial.remove(&letter) else {
            return Err(err(separator, format!("the header has no `{letter}:` file name")));
        };
        let Some(file) = m.file.filter(|f| !f.is_empty()) else {
            return Err(err(m.first_line, format!("member `{letter}` has no `{letter}:` file name")));
        };
        let Some(hash) = m.hash.filter(|h| !h.is_empty()) else {
            return Err(err(m.first_line, format!("member `{letter}` has no `{letter} hash:` line")));
        };
        members.push(Member {
            file,
            language: m.language.filter(|l| !l.is_empty()),
            hash,
            created: m.created.filter(|c| !c.is_empty()),
        });
        member_lines.push(m.lines);
    }
    check_members(&members).map_err(|(i, f, m)| err(member_lines[i][field_index(f)], m))?;

    // Rows and text lines.
    let mut rows: Vec<Row> = Vec::new();
    let mut row_lines: Vec<usize> = Vec::new();
    let mut paras: Vec<BTreeMap<ParaId, Para>> = vec![BTreeMap::new(); members.len()];
    for (n, line) in lines {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with(|c: char| c.is_ascii_alphabetic()) {
            let (m, pid, para) = parse_text_line(t, members.len()).map_err(|m| err(n, m))?;
            let Some(row) = rows.last() else {
                return Err(err(n, "a text line must come under a row"));
            };
            let name = format!("{}{}", member_letter(m), pid.0);
            if !row.cells[m].contains(&pid) {
                return Err(err(n, format!("{name} isn't in the row above")));
            }
            if paras[m].insert(pid, para).is_some() {
                return Err(err(n, format!("{name} has two text lines")));
            }
        } else {
            rows.push(parse_row_line(t, members.len()).map_err(|m| err(n, m))?);
            row_lines.push(n);
        }
    }
    check_rows(&rows, members.len()).map_err(|(r, m)| err(row_lines[r], m))?;
    for (r, row) in rows.iter().enumerate() {
        for (m, cell) in row.cells.iter().enumerate() {
            if let Some(pid) = cell.iter().find(|p| !paras[m].contains_key(p)) {
                let name = format!("{}{}", member_letter(m), pid.0);
                return Err(err(row_lines[r], format!("{name} has no text line under its row (it gives the kind)")));
            }
        }
    }

    Ok(Links { id, members, rows, paras, extra_headers })
}

/// `a15 action = text`, `b18 dialogue ~`, `a19 "End of Act"` → (member,
/// id, para).
fn parse_text_line(t: &str, members: usize) -> Result<(usize, ParaId, Para), String> {
    let letter = t.as_bytes()[0].to_ascii_lowercase() as char;
    let m = (letter as u8).wrapping_sub(b'a') as usize;
    if m >= members {
        return Err(format!("no member `{letter}` in this file"));
    }
    let rest = &t[1..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return Err(format!("expected a paragraph id after `{letter}`, as in `{letter}15 action = …`"));
    }
    let pid: u64 = rest[..digits].parse().map_err(|_| format!("`{}` is too large for an id", &rest[..digits]))?;
    let name = format!("{letter}{pid}");
    let (kind, rest) = parse_kind(rest[digits..].trim_start(), &name)?;
    let rest = rest.trim_start();
    let text = |s: &str| s.trim().to_string();
    let stamp = if rest.is_empty() {
        None
    } else if let Some(s) = rest.strip_prefix('=') {
        Some(Stamp::Synced(text(s)))
    } else if let Some(s) = rest.strip_prefix('~') {
        Some(Stamp::Placeholder(text(s)))
    } else {
        return Err(format!("expected `=`, `~` or the end of the line after {name}'s kind"));
    };
    Ok((m, ParaId(pid), Para { kind, stamp }))
}

/// `15 | 15 16`, `18 |`, `| 19`.
fn parse_row_line(t: &str, members: usize) -> Result<Row, String> {
    let parts: Vec<&str> = t.split('|').collect();
    if parts.len() != members {
        return Err(format!("a row has {members} sides separated by `|`, as in `14 | 14`; found {}", parts.len()));
    }
    let mut cells = Vec::with_capacity(members);
    for part in parts {
        let mut cell = Vec::new();
        for word in part.split_whitespace() {
            let n: u64 = word.parse().map_err(|_| format!("`{word}` is not a paragraph id"))?;
            cell.push(ParaId(n));
        }
        cells.push(cell);
    }
    Ok(Row { cells })
}

/// A member's file must be a plain `.fdx` name in the pair's folder (as
/// `plain_review_name` is for reviews): no `/`, `\` or `:`, no leading `.`.
fn check_file_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.trim() != name {
        return Err("a file name can't be empty or start or end with spaces".to_string());
    }
    if name.contains(['/', '\\', ':']) || name.starts_with('.') {
        return Err(format!("`{name}` must be a file name in this folder, not a path"));
    }
    if !name.to_lowercase().ends_with(".fdx") {
        return Err(format!("`{name}` is not an .fdx file"));
    }
    Ok(())
}

/// On error: the member, the header field to point at, the message.
fn check_members(members: &[Member]) -> Result<(), (usize, Field, String)> {
    let mut files = HashSet::new();
    let mut made = false;
    for (i, m) in members.iter().enumerate() {
        check_file_name(&m.file).map_err(|e| (i, Field::File, e))?;
        // Case-insensitive: macOS disks are.
        if !files.insert(m.file.to_lowercase()) {
            return Err((i, Field::File, format!("`{}` is named twice", m.file)));
        }
        if m.hash.trim().is_empty() {
            return Err((i, Field::Hash, format!("member `{}` has no hash", member_letter(i))));
        }
        if m.created.is_some() {
            if made {
                return Err((i, Field::Created, "only one file can be marked `created:`".to_string()));
            }
            made = true;
        }
    }
    Ok(())
}

/// Every row has one cell per member and something in it; per member, ids
/// strictly ascend through the whole file (ascending in a row, each id once,
/// no crossing). On error: the row's index and the message.
fn check_rows(rows: &[Row], members: usize) -> Result<(), (usize, String)> {
    let mut seen: Vec<HashSet<ParaId>> = vec![HashSet::new(); members];
    let mut last: Vec<Option<ParaId>> = vec![None; members];
    for (r, row) in rows.iter().enumerate() {
        if row.cells.len() != members {
            return Err((r, format!("a row has {} sides; this file has {members} members", row.cells.len())));
        }
        if row.cells.iter().all(Vec::is_empty) {
            return Err((r, "a row has no ids on any side".to_string()));
        }
        for (m, cell) in row.cells.iter().enumerate() {
            let letter = member_letter(m);
            for (k, &pid) in cell.iter().enumerate() {
                if !seen[m].insert(pid) {
                    return Err((r, format!("{letter}{} is listed twice", pid.0)));
                }
                if let Some(prev) = last[m]
                    && prev > pid
                {
                    let message = if k > 0 {
                        format!("ids out of order in a row: {letter}{} before {letter}{}", prev.0, pid.0)
                    } else {
                        format!("rows cross: {letter}{} comes after {letter}{} in an earlier row", pid.0, prev.0)
                    };
                    return Err((r, message));
                }
                last[m] = Some(pid);
            }
        }
    }
    Ok(())
}

fn one_line(s: &str) -> bool {
    !s.contains(['\n', '\r'])
}

/// Check a `Links` value against every rule of the format (what `parse`
/// enforces on text). A row with no ids on any side is refused, so a step
/// that removes ids (re-find, `after_write`) must drop rows left empty.
pub fn validate(links: &Links) -> Result<(), LinksError> {
    let invalid = |m: String| LinksError::Invalid(m);
    if !valid_id(&links.id) {
        return Err(invalid(format!("`{}` is not a valid links id", links.id)));
    }
    if links.members.len() != MEMBERS {
        return Err(invalid("one pair per links file for now: exactly two files".to_string()));
    }
    for m in &links.members {
        let values = [Some(&m.file), m.language.as_ref(), Some(&m.hash), m.created.as_ref()];
        if values.into_iter().flatten().any(|v| !one_line(v) || v.trim() != v) {
            return Err(invalid(format!("a header value of `{}` has a line break or edge spaces", m.file)));
        }
        if [&m.language, &m.created].into_iter().flatten().any(String::is_empty) {
            return Err(invalid(format!("an empty language or created value for `{}`", m.file)));
        }
    }
    check_members(&links.members).map_err(|(_, _, m)| invalid(m))?;
    for (key, value) in &links.extra_headers {
        let bad_key = key.is_empty()
            || key.trim() != key
            || key.contains(':')
            || key.starts_with('#')
            || !one_line(key)
            || known_key(key).is_some();
        if bad_key || !one_line(value) || value.trim() != value {
            return Err(invalid(format!("header `{key}` can't be written")));
        }
    }
    check_rows(&links.rows, links.members.len()).map_err(|(r, m)| invalid(format!("row {}: {m}", r + 1)))?;
    if links.paras.len() != links.members.len() {
        return Err(invalid("one paragraph table per member".to_string()));
    }
    for (m, paras) in links.paras.iter().enumerate() {
        let listed: HashSet<ParaId> = links.rows.iter().flat_map(|r| r.cells[m].iter().copied()).collect();
        if let Some(pid) = listed.iter().find(|p| !paras.contains_key(p)) {
            return Err(invalid(format!("{}{} is listed but has no kind", member_letter(m), pid.0)));
        }
        for (pid, para) in paras {
            let name = format!("{}{}", member_letter(m), pid.0);
            if !listed.contains(pid) {
                return Err(invalid(format!("{name} has a text line but no row")));
            }
            if let ElementKind::Other(kind) = &para.kind
                && !one_line(kind)
            {
                return Err(invalid(format!("{name}'s kind has a line break")));
            }
            if let Some(stamp) = &para.stamp
                && (!one_line(stamp.text()) || stamp.text().trim() != stamp.text())
            {
                return Err(invalid(format!("{name}'s text has a line break or edge spaces")));
            }
        }
    }
    Ok(())
}

/// The file's text: header (known keys, then unknown ones as read), the
/// explanation comment, `---`, then each row with its paragraphs' text lines
/// under it (member by member, ids ascending). Refuses what `validate`
/// refuses (so a bad file is never written), including a row left with no
/// ids: callers that remove ids drop such rows first.
pub fn write(links: &Links) -> Result<String, LinksError> {
    validate(links)?;
    let mut out = String::new();
    out.push_str(MAGIC);
    out.push('\n');
    out.push_str(&format!("Id: {}\n", links.id));
    for (i, m) in links.members.iter().enumerate() {
        let l = member_letter(i);
        out.push_str(&format!("{l}: {}\n", m.file));
        if let Some(lang) = &m.language {
            out.push_str(&format!("{l} language: {lang}\n"));
        }
        out.push_str(&format!("{l} hash: {}\n", m.hash));
        if let Some(created) = &m.created {
            out.push_str(&format!("{l} created: {created}\n"));
        }
    }
    for (key, value) in &links.extra_headers {
        out.push_str(format!("{key}: {value}").trim_end());
        out.push('\n');
    }
    out.push('\n');
    out.push_str(EXPLANATION);
    out.push_str("---\n");
    for row in &links.rows {
        let sides: Vec<String> =
            row.cells.iter().map(|c| c.iter().map(|p| p.0.to_string()).collect::<Vec<_>>().join(" ")).collect();
        out.push_str(sides.join(" | ").trim());
        out.push('\n');
        for (m, cell) in row.cells.iter().enumerate() {
            for pid in cell {
                let para = &links.paras[m][pid];
                let mut line = format!("  {}{} {}", member_letter(m), pid.0, kind_token(&para.kind));
                match &para.stamp {
                    Some(Stamp::Synced(t)) => line.push_str(&format!(" = {t}")),
                    Some(Stamp::Placeholder(t)) => line.push_str(&format!(" ~ {t}")),
                    None => {}
                }
                out.push_str(line.trim_end());
                out.push('\n');
            }
        }
    }
    Ok(out)
}
