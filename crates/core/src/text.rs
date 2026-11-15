//! Text helpers for comparing and matching paragraphs.

/// Comparison key: ignores surrounding/duplicate whitespace, quote and dash
/// style, and ellipsis form.
pub fn normalize(s: &str) -> String {
    let mut mapped = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{2032}' | '`' => mapped.push('\''),
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{2033}' => mapped.push('"'),
            '\u{2026}' => mapped.push_str("..."),
            '\u{2013}' | '\u{2014}' => mapped.push('-'),
            c => mapped.push(c),
        }
    }
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Would two versions of a paragraph look the same on the page? Edge
/// whitespace never shows; letter case doesn't show in all-caps elements.
pub fn looks_same(a: &str, b: &str, caps: bool) -> bool {
    if caps { a.trim().to_uppercase() == b.trim().to_uppercase() } else { a.trim() == b.trim() }
}

/// Give `text` the leading/trailing whitespace of `original`.
pub fn keep_edge_whitespace(original: &str, text: &str) -> String {
    let t = text.trim();
    if original.trim().is_empty() {
        return t.to_string();
    }
    let lead = &original[..original.len() - original.trim_start().len()];
    let trail = &original[original.trim_end().len()..];
    format!("{lead}{t}{trail}")
}

/// [`similarity`] at or above which two versions of a line count as the same
/// line edited (fuzzy review matching; pairing lines in `Edit::replace_run`).
pub const FUZZY_THRESHOLD: f64 = 0.85;

/// Sørensen–Dice similarity over character bigrams of normalized text, 0..=1.
pub fn similarity(a: &str, b: &str) -> f64 {
    let x: Vec<char> = normalize(a).to_lowercase().chars().collect();
    let y: Vec<char> = normalize(b).to_lowercase().chars().collect();
    if x == y {
        return 1.0;
    }
    if x.len() < 2 || y.len() < 2 {
        return 0.0;
    }
    let mut grams: std::collections::HashMap<(char, char), usize> = Default::default();
    for w in x.windows(2) {
        *grams.entry((w[0], w[1])).or_default() += 1;
    }
    let mut hits = 0usize;
    for w in y.windows(2) {
        if let Some(c) = grams.get_mut(&(w[0], w[1]))
            && *c > 0
        {
            hits += 1;
            *c -= 1;
        }
    }
    (2 * hits) as f64 / (x.len() - 1 + y.len() - 1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_ignores_whitespace_quotes_dashes_ellipsis() {
        assert_eq!(normalize("  “Hi” — it’s   me… "), "\"Hi\" - it's me...");
        assert_eq!(normalize("a\u{00A0}b"), "a b");
    }

    #[test]
    fn keep_edge_whitespace_cases() {
        assert_eq!(keep_edge_whitespace("Laura smiles. ", "Laura smiles happily."), "Laura smiles happily. ");
        assert_eq!(keep_edge_whitespace("  Hi", " Hello "), "  Hello");
        assert_eq!(keep_edge_whitespace("", " new "), "new");
    }

    #[test]
    fn similarity_cases() {
        assert_eq!(similarity("It’s me.", "It's  me."), 1.0);
        assert!(similarity("The cat sat on the mat today.", "The cat sat on a mat today.") > 0.85);
        assert!(similarity("Laura smiles.", "The truck drives off into the night.") < 0.4);
        assert_eq!(similarity("a", "b"), 0.0);
    }

    #[test]
    fn looks_same_cases() {
        assert!(looks_same("cesar ", "CESAR", true));
        assert!(!looks_same("cesar", "CESAR", false));
        assert!(looks_same(" Rose.", "Rose.", false));
        assert!(!looks_same("rose", "Rose", false));
        assert!(!looks_same("CESAR", "CARLOS", true));
    }
}
