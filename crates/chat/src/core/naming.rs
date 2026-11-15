//! Thread titles and the review a thread owns.

/// Most words in a title.
pub const MAX_WORDS: usize = 6;
/// Most characters in a title (the ellipsis not counted).
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
/// A first word longer than the limit is cut inside the word.
pub fn thread_title(first_message: &str) -> String {
    let words: Vec<&str> = first_message.split_whitespace().collect();
    if words.is_empty() {
        return UNTITLED.into();
    }
    let mut title = String::new();
    let mut cut = false;
    for (i, w) in words.iter().enumerate() {
        let sep = usize::from(i > 0);
        if i >= MAX_WORDS || title.chars().count() + sep + w.chars().count() > MAX_CHARS {
            if i == 0 {
                title = w.chars().take(MAX_CHARS).collect();
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

/// A title as a file name stem: forbidden characters become spaces, runs of
/// spaces collapse, the ellipsis and leading or trailing dots and spaces go.
fn stem(title: &str) -> String {
    let cleaned: String = title.trim_end_matches('…').chars().map(|c| if forbidden(c) { ' ' } else { c }).collect();
    let joined = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = joined.trim_matches(|c| c == '.' || c == ' ');
    if trimmed.is_empty() { "conversation".into() } else { trimmed.to_string() }
}

/// The review file name for a thread titled `title`: `<title>.review.txt`,
/// or `<title> 2.review.txt`, `<title> 3.review.txt`… when `existing` (the
/// names of the other files in the folder) already holds it. Names compare
/// ignoring case, as on a case-insensitive disk.
pub fn review_file_name<S: AsRef<str>>(title: &str, existing: &[S]) -> String {
    let base = stem(title);
    let taken = |name: &str| existing.iter().any(|e| e.as_ref().eq_ignore_ascii_case(name));
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
    }

    #[test]
    fn file_name_collisions() {
        let existing = ["Pass.review.txt", "pass 2.REVIEW.TXT", "other.review.txt"];
        assert_eq!(review_file_name("Pass", &existing), "Pass 3.review.txt");
        assert_eq!(review_file_name("Fresh", &existing), "Fresh.review.txt");
        let one = vec!["Pass.review.txt".to_string()];
        assert_eq!(review_file_name("Pass", &one), "Pass 2.review.txt");
    }
}
