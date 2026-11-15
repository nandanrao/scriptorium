//! Thread titles and the review a thread owns.

use unicode_segmentation::UnicodeSegmentation;

/// Most words in a title.
pub const MAX_WORDS: usize = 6;
/// Most characters in a title (the ellipsis not counted), counted as
/// grapheme clusters: a letter with its combining marks, or an emoji
/// sequence, is one and is never split.
pub const MAX_CHARS: usize = 40;
/// Title of a thread whose first message has no words.
pub const UNTITLED: &str = "New conversation";
/// File-name suffix of a review file.
pub const SUFFIX: &str = ".review.txt";

/// The id the thread's review has in the session: `thread-<id>`.
pub fn review_id(thread_id: &str) -> String {
    format!("thread-{thread_id}")
}

/// The thread id a review id names, if it is one (`thread-t0001` gives `t0001`).
pub fn thread_of_review(review_id: &str) -> Option<&str> {
    review_id.strip_prefix("thread-").filter(|s| !s.is_empty())
}

/// A thread's title: the first words of its first message, at most
/// [`MAX_WORDS`] words and [`MAX_CHARS`] characters, ending in "…" when cut.
/// A first word longer than the limit is cut inside the word, between
/// grapheme clusters.
pub fn thread_title(first_message: &str) -> String {
    let len = |s: &str| s.graphemes(true).count();
    let words: Vec<&str> = first_message.split_whitespace().collect();
    if words.is_empty() {
        return UNTITLED.into();
    }
    let mut title = String::new();
    let mut cut = false;
    for (i, w) in words.iter().enumerate() {
        let sep = usize::from(i > 0);
        if i >= MAX_WORDS || len(&title) + sep + len(w) > MAX_CHARS {
            if i == 0 {
                title = w.graphemes(true).take(MAX_CHARS).collect();
            }
            cut = true;
            break;
        }
        if i > 0 {
            title.push(' ');
        }
        title.push_str(w);
    }
    if cut {
        title.push('…');
    }
    title
}

/// Characters a file name can't hold on some platform.
fn forbidden(c: char) -> bool {
    c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
}

/// Whether Windows reserves `stem` as a device name (CON, PRN, AUX, NUL,
/// COM1–COM9, LPT1–LPT9, in any case). It does so whatever the extension,
/// so `CON.review.txt` can't be created there.
fn reserved_on_windows(stem: &str) -> bool {
    let upper = stem.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && matches!(upper.as_bytes()[3], b'1'..=b'9'))
}

/// A title as a file name stem: forbidden characters become spaces, runs of
/// spaces collapse, the ellipsis and leading or trailing dots and spaces go,
/// and a Windows device name gets a `_` after it (`CON` → `CON_`).
fn stem(title: &str) -> String {
    let cleaned: String = title.trim_end_matches('…').chars().map(|c| if forbidden(c) { ' ' } else { c }).collect();
    let joined = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = joined.trim_matches(|c| c == '.' || c == ' ');
    if trimmed.is_empty() {
        "conversation".into()
    } else if reserved_on_windows(trimmed) {
        format!("{trimmed}_")
    } else {
        trimmed.to_string()
    }
}

/// The review file name for a thread titled `title`: `<title>.review.txt`,
/// or `<title> 2.review.txt`, `<title> 3.review.txt`… when `existing` (the
/// names of the other files in the folder) already holds it. Names compare
/// ignoring case (Unicode lowercase, so `É` and `é` clash), as on a
/// case-insensitive disk. They are not Unicode-normalised: an `é` typed as
/// one character and a name on disk spelled `e` + accent (as macOS may store
/// it) don't clash here, though the disk may treat them as one name.
///
/// So this is a first guess, not a reservation: the caller (C3c) must create
/// the file with `create_new` and, on `AlreadyExists` (another program got
/// there first, or the disk folds names this doesn't), list the folder again
/// and retry with the next name.
pub fn review_file_name<S: AsRef<str>>(title: &str, existing: &[S]) -> String {
    let base = stem(title);
    let existing: Vec<String> = existing.iter().map(|e| e.as_ref().to_lowercase()).collect();
    let taken = |name: &str| existing.contains(&name.to_lowercase());
    let first = format!("{base}{SUFFIX}");
    if !taken(&first) {
        return first;
    }
    (2..).map(|n| format!("{base} {n}{SUFFIX}")).find(|name| !taken(name)).expect("an unbounded range has a free name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        assert_eq!(review_id("t0001"), "thread-t0001");
        assert_eq!(thread_of_review("thread-t0001"), Some("t0001"));
        assert_eq!(thread_of_review("thread-"), None);
        assert_eq!(thread_of_review("dialogue"), None);
    }

    #[test]
    fn short_title_is_whole() {
        assert_eq!(thread_title("Tighten the kitchen scene"), "Tighten the kitchen scene");
        assert_eq!(thread_title("  tighten \n\t the   dialogue "), "tighten the dialogue");
    }

    #[test]
    fn six_words_then_ellipsis() {
        assert_eq!(thread_title("one two three four five six"), "one two three four five six");
        assert_eq!(thread_title("one two three four five six seven"), "one two three four five six…");
    }

    #[test]
    fn forty_chars() {
        let t = thread_title("extraordinarily incomprehensible unquestionably hyperbolic");
        assert_eq!(t, "extraordinarily incomprehensible…");
        assert!(t.chars().count() <= MAX_CHARS + 1);
        let long = "x".repeat(100);
        let t = thread_title(&long);
        assert_eq!(t.chars().count(), MAX_CHARS + 1);
        assert!(t.ends_with('…'));
        let exact = "y".repeat(MAX_CHARS);
        assert_eq!(thread_title(&exact), exact);
    }

    #[test]
    fn multibyte_is_safe() {
        let t = thread_title(&"é".repeat(60));
        assert_eq!(t.chars().count(), MAX_CHARS + 1);
    }

    #[test]
    fn combining_marks_and_emoji_are_not_split() {
        // "e" + combining acute, 60 times: cut to 40 whole letters.
        let t = thread_title(&"e\u{301}".repeat(60));
        assert_eq!(t, format!("{}…", "e\u{301}".repeat(MAX_CHARS)));
        // A family emoji is one cluster of several chars.
        let family = "👨\u{200d}👩\u{200d}👧";
        let t = thread_title(&family.repeat(50));
        assert_eq!(t, format!("{}…", family.repeat(MAX_CHARS)));
        // Words are measured the same way: 39 accented letters + space + 1 fits.
        let words = format!("{} x", "e\u{301}".repeat(MAX_CHARS - 2));
        assert_eq!(thread_title(&words), words);
    }

    #[test]
    fn empty_title() {
        assert_eq!(thread_title(""), UNTITLED);
        assert_eq!(thread_title(" \n "), UNTITLED);
    }

    #[test]
    fn file_name_plain() {
        let none: [&str; 0] = [];
        assert_eq!(review_file_name("Dialogue pass", &none), "Dialogue pass.review.txt");
        assert_eq!(review_file_name("one two…", &none), "one two.review.txt");
    }

    #[test]
    fn file_name_sanitised() {
        let none: [&str; 0] = [];
        assert_eq!(review_file_name("Fix: INT/EXT \"scenes\"?", &none), "Fix INT EXT scenes.review.txt");
        assert_eq!(review_file_name("..", &none), "conversation.review.txt");
        assert_eq!(review_file_name("  ", &none), "conversation.review.txt");
        for (title, name) in [
            ("CON", "CON_.review.txt"),
            ("nul", "nul_.review.txt"),
            ("Com1", "Com1_.review.txt"),
            ("LPT9.", "LPT9_.review.txt"),
            ("COM0", "COM0.review.txt"),
            ("COM10", "COM10.review.txt"),
            ("CONSOLE", "CONSOLE.review.txt"),
            ("con job", "con job.review.txt"),
        ] {
            assert_eq!(review_file_name(title, &none), name, "{title}");
        }
    }

    #[test]
    fn file_name_collisions() {
        let existing = ["Pass.review.txt", "pass 2.REVIEW.TXT", "other.review.txt"];
        assert_eq!(review_file_name("Pass", &existing), "Pass 3.review.txt");
        assert_eq!(review_file_name("Fresh", &existing), "Fresh.review.txt");
        let one = vec!["Pass.review.txt".to_string()];
        assert_eq!(review_file_name("Pass", &one), "Pass 2.review.txt");
        // Beyond ASCII too.
        assert_eq!(review_file_name("Été", &["ÉTÉ.REVIEW.TXT"]), "Été 2.review.txt");
    }
}
