//! Diff helpers.
//!
//! - [`opcodes`]: LCS opcodes over slices (used to carry styles across edits).
//! - [`word_diff`]: word-level ops for display, grouped so each changed stretch
//!   reads as one deletion followed by one insertion.

use std::sync::LazyLock;

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    Equal,
    Replace,
    Insert,
    Delete,
}

/// `a[i1..i2]` becomes `b[j1..j2]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opcode {
    pub tag: Tag,
    pub i1: usize,
    pub i2: usize,
    pub j1: usize,
    pub j2: usize,
}

/// Longest-common-subsequence opcodes turning `a` into `b`. Adjacent
/// deletes/inserts are merged into a single `Replace`. Common prefix and suffix
/// are trimmed first, so typical paragraph edits are cheap.
pub fn opcodes<T: PartialEq>(a: &[T], b: &[T]) -> Vec<Opcode> {
    let (n, m) = (a.len(), b.len());
    let mut pre = 0;
    while pre < n && pre < m && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < n - pre && suf < m - pre && a[n - 1 - suf] == b[m - 1 - suf] {
        suf += 1;
    }
    let (aa, bb) = (&a[pre..n - suf], &b[pre..m - suf]);
    let (nn, mm) = (aa.len(), bb.len());
    // l[i][j] = LCS length of aa[i..] and bb[j..], stored row-major.
    let w = mm + 1;
    let mut l = vec![0u32; (nn + 1) * w];
    for i in (0..nn).rev() {
        for j in (0..mm).rev() {
            l[i * w + j] =
                if aa[i] == bb[j] { l[(i + 1) * w + j + 1] + 1 } else { l[(i + 1) * w + j].max(l[i * w + j + 1]) };
        }
    }

    let mut ops: Vec<Opcode> = Vec::new();
    let mut add = |tag: Tag, i1, i2, j1, j2| {
        if let Some(last) = ops.last_mut()
            && last.i2 == i1
            && last.j2 == j1
            && (last.tag == tag || (tag != Tag::Equal && last.tag != Tag::Equal))
        {
            if last.tag != tag {
                last.tag = Tag::Replace;
            }
            last.i2 = i2;
            last.j2 = j2;
            return;
        }
        ops.push(Opcode { tag, i1, i2, j1, j2 });
    };

    if pre > 0 {
        add(Tag::Equal, 0, pre, 0, pre);
    }
    let (mut i, mut j) = (0, 0);
    while i < nn && j < mm {
        let (pi, pj) = (pre + i, pre + j);
        if aa[i] == bb[j] {
            add(Tag::Equal, pi, pi + 1, pj, pj + 1);
            i += 1;
            j += 1;
        } else if l[(i + 1) * w + j] >= l[i * w + j + 1] {
            add(Tag::Delete, pi, pi + 1, pj, pj);
            i += 1;
        } else {
            add(Tag::Insert, pi, pi, pj, pj + 1);
            j += 1;
        }
    }
    while i < nn {
        add(Tag::Delete, pre + i, pre + i + 1, pre + j, pre + j);
        i += 1;
    }
    while j < mm {
        add(Tag::Insert, pre + i, pre + i, pre + j, pre + j + 1);
        j += 1;
    }
    if suf > 0 {
        add(Tag::Equal, n - suf, n, m - suf, m);
    }
    ops
}

static TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+|[\p{L}\p{N}'’]+|[^\s\p{L}\p{N}]").unwrap());

/// Split into words (keeping apostrophes), whitespace runs and single punctuation marks.
pub fn tokenize(s: &str) -> Vec<&str> {
    TOKEN_RE.find_iter(s).map(|m| m.as_str()).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordOp {
    Eq,
    Del,
    Ins,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordChunk {
    pub op: WordOp,
    pub text: String,
}

fn chunk(op: WordOp, text: impl Into<String>) -> WordChunk {
    WordChunk { op, text: text.into() }
}

/// Word diff for display. Whitespace sandwiched between two changes is folded
/// into the change, and each changed stretch is emitted as one `Del` then one
/// `Ins`, so "a b" → "c d" reads as one replacement.
pub fn word_diff(a: &str, b: &str) -> Vec<WordChunk> {
    let (ta, tb) = (tokenize(a), tokenize(b));
    let mut raw = Vec::new();
    for op in opcodes(&ta, &tb) {
        let old = ta[op.i1..op.i2].concat();
        let new = tb[op.j1..op.j2].concat();
        match op.tag {
            Tag::Equal => raw.push(chunk(WordOp::Eq, old)),
            Tag::Delete => raw.push(chunk(WordOp::Del, old)),
            Tag::Insert => raw.push(chunk(WordOp::Ins, new)),
            Tag::Replace => {
                raw.push(chunk(WordOp::Del, old));
                raw.push(chunk(WordOp::Ins, new));
            }
        }
    }
    // Fold whitespace-only equal runs between two changes into the change.
    let mut folded = Vec::with_capacity(raw.len());
    for k in 0..raw.len() {
        let o = &raw[k];
        let between = k > 0 && k + 1 < raw.len() && raw[k - 1].op != WordOp::Eq && raw[k + 1].op != WordOp::Eq;
        if o.op == WordOp::Eq && between && o.text.chars().all(char::is_whitespace) {
            folded.push(chunk(WordOp::Del, o.text.clone()));
            folded.push(chunk(WordOp::Ins, o.text.clone()));
        } else {
            folded.push(o.clone());
        }
    }
    let mut out = Vec::new();
    let (mut del, mut ins) = (String::new(), String::new());
    let flush = |out: &mut Vec<WordChunk>, del: &mut String, ins: &mut String| {
        if !del.is_empty() {
            out.push(chunk(WordOp::Del, std::mem::take(del)));
        }
        if !ins.is_empty() {
            out.push(chunk(WordOp::Ins, std::mem::take(ins)));
        }
    };
    for o in folded {
        match o.op {
            WordOp::Del => del.push_str(&o.text),
            WordOp::Ins => ins.push_str(&o.text),
            WordOp::Eq => {
                flush(&mut out, &mut del, &mut ins);
                out.push(o);
            }
        }
    }
    flush(&mut out, &mut del, &mut ins);
    out
}

/// Number of changed words: a rough "size" of an edit, for sorting and triage.
pub fn change_size(chunks: &[WordChunk]) -> usize {
    chunks
        .iter()
        .filter(|c| c.op != WordOp::Eq)
        .map(|c| tokenize(&c.text).iter().filter(|t| !t.trim().is_empty()).count())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(a: &[char], b: &[char], ops: &[Opcode]) -> String {
        ops.iter()
            .map(|o| match o.tag {
                Tag::Equal => a[o.i1..o.i2].iter().collect::<String>(),
                _ => b[o.j1..o.j2].iter().collect(),
            })
            .collect()
    }

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn opcodes_reconstruct_target() {
        for (a, b) in [
            ("", ""),
            ("abc", "abc"),
            ("", "xyz"),
            ("xyz", ""),
            ("kitten", "sitting"),
            ("cards game", "card game"),
            ("abcdef", "azced"),
        ] {
            let (ca, cb) = (chars(a), chars(b));
            assert_eq!(apply(&ca, &cb, &opcodes(&ca, &cb)), b, "{a} -> {b}");
        }
    }

    #[test]
    fn opcodes_merge_adjacent_changes_and_cover_both_sides() {
        let ops = opcodes(&chars("cat"), &chars("dog"));
        assert_eq!(ops, [Opcode { tag: Tag::Replace, i1: 0, i2: 3, j1: 0, j2: 3 }]);
        let ops = opcodes(&chars("the cat sat"), &chars("the dog sat"));
        assert_eq!(ops[0].tag, Tag::Equal);
        assert_eq!((ops.last().unwrap().i2, ops.last().unwrap().j2), (11, 11));
    }

    #[test]
    fn tokenize_words_spaces_punctuation() {
        assert_eq!(tokenize("Don't go, Laura—now!"), ["Don't", " ", "go", ",", " ", "Laura", "—", "now", "!"]);
        assert_eq!(tokenize("Señora Pérez"), ["Señora", " ", "Pérez"]);
    }

    fn side(chunks: &[WordChunk], keep: WordOp) -> String {
        chunks.iter().filter(|c| c.op == WordOp::Eq || c.op == keep).map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn word_diff_groups_replacements_and_reconstructs() {
        let (a, b) = ("I am go to the town.", "I am going to town.");
        let d = word_diff(a, b);
        assert_eq!(d[..3], [chunk(WordOp::Eq, "I am "), chunk(WordOp::Del, "go"), chunk(WordOp::Ins, "going")]);
        let dels: Vec<&str> = d.iter().filter(|c| c.op == WordOp::Del).map(|c| c.text.trim()).collect();
        assert_eq!(dels, ["go", "the"]);
        assert_eq!(side(&d, WordOp::Del), a);
        assert_eq!(side(&d, WordOp::Ins), b);
    }

    #[test]
    fn word_diff_folds_whitespace_between_changes() {
        assert_eq!(
            word_diff("big red dog", "small blue dog"),
            [chunk(WordOp::Del, "big red"), chunk(WordOp::Ins, "small blue"), chunk(WordOp::Eq, " dog")]
        );
        assert_eq!(word_diff("Same.", "Same."), [chunk(WordOp::Eq, "Same.")]);
    }

    #[test]
    fn change_size_counts_words() {
        assert_eq!(change_size(&word_diff("big red dog", "small blue dog")), 4);
        assert_eq!(change_size(&word_diff("a", "a")), 0);
    }
}
