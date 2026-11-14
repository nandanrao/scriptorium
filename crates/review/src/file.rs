//! The plain-text review file, `SCRIPT REVIEW v1`.
//!
//! ```text
//! SCRIPT REVIEW v1
//! Script: name.fdx
//!
//! ---
//! BEFORE: whole original paragraph
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

use std::sync::LazyLock;

use regex::Regex;

pub const FORMAT_HEADER: &str = "SCRIPT REVIEW v1";

static FIELD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*(BEFORE|AFTER|WHY|STATUS|CHOSEN|MINE)\s*:\s?(.*)$").unwrap());
static HEADER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\s*(Script|Reviewer)\s*:\s*(.*)$").unwrap());
static SEPARATOR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*-{3,}\s*$").unwrap());

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

/// One block of the file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Entry {
    pub before: String,
    /// The `AFTER` texts, in order; the first is the suggester's preference.
    pub options: Vec<String>,
    pub why: String,
    pub status: FileStatus,
    /// Which option was accepted, 0-based (the file is 1-based).
    pub chosen: Option<usize>,
    pub mine: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReviewFile {
    /// The `Script:` header: the file name the review was written for.
    pub script: String,
    pub entries: Vec<Entry>,
    /// Problems found while parsing (skipped blocks, bad values), for the user.
    pub warnings: Vec<String>,
}

#[derive(Default)]
struct Block {
    before: Option<String>,
    after: Vec<String>,
    why: Option<String>,
    status: Option<String>,
    chosen: Option<String>,
    mine: Option<String>,
}

impl Block {
    fn field(&mut self, key: &str) -> &mut String {
        let slot = match key {
            "before" => &mut self.before,
            "why" => &mut self.why,
            "status" => &mut self.status,
            "chosen" => &mut self.chosen,
            _ => &mut self.mine,
        };
        slot.get_or_insert_with(String::new)
    }
}

impl ReviewFile {
    pub fn parse(text: &str) -> Self {
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
        let mut out = ReviewFile::default();
        let mut cur: Option<Block> = None;
        let mut last_field: Option<String> = None;

        for line in text.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let t = line.trim_start();
            if t.starts_with("```") || t.starts_with('#') {
                continue;
            }
            if SEPARATOR_RE.is_match(line) {
                out.finish(cur.take());
                last_field = None;
                continue;
            }
            if let Some(m) = FIELD_RE.captures(line) {
                let key = m[1].to_lowercase();
                let value = m[2].to_string();
                if key == "before" && cur.as_ref().is_some_and(|b| b.before.is_some()) {
                    out.finish(cur.take());
                }
                let block = cur.get_or_insert_with(Block::default);
                if key == "after" {
                    block.after.push(value);
                } else {
                    *block.field(&key) = value;
                }
                last_field = Some(key);
                continue;
            }
            let Some(block) = cur.as_mut() else {
                if let Some(h) = HEADER_RE.captures(line)
                    && h[1].eq_ignore_ascii_case("script")
                {
                    out.script = h[2].trim().to_string();
                }
                continue;
            };
            // A wrapped line continues the previous field.
            if let Some(key) = &last_field
                && !line.trim().is_empty()
            {
                let target =
                    if key == "after" { block.after.last_mut().expect("after pushed") } else { block.field(key) };
                target.push(' ');
                target.push_str(line.trim());
            }
        }
        out.finish(cur);
        out
    }

    fn finish(&mut self, block: Option<Block>) {
        let Some(b) = block else { return };
        let options: Vec<String> = b.after.iter().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
        let has_before = b.before.is_some();
        if !has_before || options.is_empty() {
            if has_before || !options.is_empty() {
                let missing = if has_before { "AFTER" } else { "BEFORE" };
                self.warnings.push(format!("Block {} is missing {missing}; skipped.", self.entries.len() + 1));
            }
            return;
        }
        let before = b.before.unwrap_or_default();
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
        self.entries.push(Entry {
            before: before.trim().to_string(),
            options,
            why: b.why.unwrap_or_default().trim().to_string(),
            status,
            chosen,
            mine: b.mine.map(|m| m.trim().to_string()),
        });
    }

    /// Write the file back out. Round-trips through [`ReviewFile::parse`]
    /// (comments and wrapping are not kept).
    pub fn serialize(&self) -> String {
        let one_line = |s: &str| s.split('\n').map(str::trim).collect::<Vec<_>>().join(" ");
        let mut out = vec![FORMAT_HEADER.to_string()];
        if !self.script.is_empty() {
            out.push(format!("Script: {}", self.script));
        }
        out.push(String::new());
        for e in &self.entries {
            out.push("---".into());
            out.push(format!("BEFORE: {}", one_line(&e.before)));
            for o in &e.options {
                out.push(format!("AFTER: {}", one_line(o)));
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
                out.push(format!("MINE: {}", one_line(m)));
            }
        }
        out.push(String::new());
        out.join("\n")
    }
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
        Entry { before: before.into(), options: options.iter().map(|s| s.to_string()).collect(), ..Entry::default() }
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
        let got: Vec<_> = r.entries.iter().map(|e| (e.before.as_str(), e.options.clone())).collect();
        assert_eq!(got, [("One line", vec!["Fixed continued here".to_string()]), ("Two", vec!["Too".to_string()])]);
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
        assert_eq!(got, [(FileStatus::Rewritten, Some("c".into())), (FileStatus::Accepted, None)]);
    }

    #[test]
    fn serialize_round_trips() {
        let data = ReviewFile {
            script: "X.fdx".into(),
            entries: vec![
                Entry { why: "w".into(), ..entry("a", &["b"]) },
                Entry { status: FileStatus::Rewritten, mine: Some("mine".into()), ..entry("c", &["d"]) },
                Entry { status: FileStatus::Rejected, ..entry("e", &["f"]) },
                Entry { status: FileStatus::Accepted, chosen: Some(2), ..entry("g", &["h1", "h2", "h3"]) },
            ],
            warnings: vec![],
        };
        let text = data.serialize();
        assert!(text.starts_with("SCRIPT REVIEW v1\nScript: X.fdx\n"));
        assert_eq!(ReviewFile::parse(&text), data);
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
        assert_eq!(r.entries[0].options, ["one", "two wrapped", "three"]);
        assert_eq!(r.entries[0].chosen, Some(1));
    }

    #[test]
    fn invalid_chosen_ignored_and_empty_after_dropped() {
        let r = ReviewFile::parse("---\nBEFORE: a\nAFTER:\nAFTER: b\nCHOSEN: 5\n");
        assert_eq!(r.entries[0].options, ["b"]);
        assert_eq!(r.entries[0].chosen, None);
        assert_eq!(r.warnings.len(), 1);
    }
}
