//! What the thread list shows, and the keys first-use disclosures are
//! remembered under. Times are Unix seconds passed in; nothing here reads a clock.

/// Why a thread can't be opened; it is listed greyed with this reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The file can't be read or parsed.
    Unreadable(String),
    /// Written by a newer version of Scriptorium.
    Newer(String),
}

impl Problem {
    /// The reason, as the list shows it.
    pub fn reason(&self) -> String {
        match self {
            Self::Unreadable(why) => format!("Can't read this conversation: {why}"),
            Self::Newer(why) => format!("Made by a newer Scriptorium: {why}"),
        }
    }
}

/// One thread in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub title: String,
    /// The model it last ran on; empty if none yet.
    pub model: String,
    /// Last change, Unix seconds.
    pub updated: u64,
    /// Suggestions in its review; `None` if it has none or it can't be read.
    pub suggestions: Option<usize>,
    pub problem: Option<Problem>,
}

impl Entry {
    /// Greyed out: it can't be opened.
    pub fn greyed(&self) -> bool {
        self.problem.is_some()
    }

    /// The second line of its row: the reason when greyed, else
    /// "claude-opus-5-5 · 5 min ago · 3 suggestions".
    pub fn detail(&self, now: u64) -> String {
        if let Some(p) = &self.problem {
            return p.reason();
        }
        let mut parts = Vec::new();
        if !self.model.is_empty() {
            parts.push(self.model.clone());
        }
        parts.push(ago(now, self.updated));
        if let Some(n) = self.suggestions {
            parts.push(format!("{n} {}", if n == 1 { "suggestion" } else { "suggestions" }));
        }
        parts.join(" · ")
    }
}

/// The list newest first (ties by id, descending, so the order is stable).
pub fn sorted(mut entries: Vec<Entry>) -> Vec<Entry> {
    entries.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| b.id.cmp(&a.id)));
    entries
}

/// "just now", "5 min ago", "2 h ago", "3 days ago"; a time in the future is "just now".
pub fn ago(now: u64, then: u64) -> String {
    let s = now.saturating_sub(then);
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86_400 => format!("{} h ago", s / 3600),
        86_400..172_800 => "1 day ago".into(),
        _ => format!("{} days ago", s / 86_400),
    }
}

/// The host of an account's base URL, lowercased, with any port:
/// `https://API.Anthropic.com/v1` gives `api.anthropic.com`. `None` if there is none.
pub fn host_of(base_url: &str) -> Option<String> {
    let rest = base_url.trim().split_once("://").map_or(base_url.trim(), |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// The key a first-use disclosure is remembered under: the provider host and
/// the script's path, so a new host or a new script asks again. `None` when
/// the URL has no host.
pub fn disclosure_key(base_url: &str, script_path: &str) -> Option<String> {
    Some(format!("{}\u{1f}{script_path}", host_of(base_url)?))
}

/// The disclosure shown before the first send: the app renders it as a card,
/// the CLI prints it.
pub fn disclosure_text(host: &str, script_name: &str, threads_folder: &str) -> String {
    format!(
        "What gets sent. Your messages, and the parts of {script_name} the agent reads (scenes, search results, \
         sometimes the whole script), go to {host} with your key. This conversation is saved in {threads_folder} \
         next to the script, and contains excerpts of it."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, updated: u64) -> Entry {
        Entry { id: id.into(), title: id.into(), model: "m".into(), updated, suggestions: None, problem: None }
    }

    #[test]
    fn newest_first_stable() {
        let list = sorted(vec![entry("t1", 10), entry("t3", 30), entry("t2", 30), entry("t0", 5)]);
        let ids: Vec<&str> = list.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["t3", "t2", "t1", "t0"]);
    }

    #[test]
    fn times() {
        assert_eq!(ago(100, 100), "just now");
        assert_eq!(ago(100, 500), "just now");
        assert_eq!(ago(159, 100), "just now");
        assert_eq!(ago(100 + 300, 100), "5 min ago");
        assert_eq!(ago(100 + 7200, 100), "2 h ago");
        assert_eq!(ago(100 + 86_400, 100), "1 day ago");
        assert_eq!(ago(100 + 3 * 86_400, 100), "3 days ago");
    }

    #[test]
    fn detail_line() {
        let mut e = entry("t1", 100);
        assert_eq!(e.detail(400), "m · 5 min ago");
        e.suggestions = Some(1);
        assert_eq!(e.detail(400), "m · 5 min ago · 1 suggestion");
        e.suggestions = Some(0);
        e.model.clear();
        assert_eq!(e.detail(400), "5 min ago · 0 suggestions");
        assert!(!e.greyed());
    }

    #[test]
    fn greyed_shows_reason() {
        let mut e = entry("t1", 1);
        e.problem = Some(Problem::Unreadable("not JSON".into()));
        assert!(e.greyed());
        assert_eq!(e.detail(9), "Can't read this conversation: not JSON");
        e.problem = Some(Problem::Newer("version 3".into()));
        assert_eq!(e.detail(9), "Made by a newer Scriptorium: version 3");
    }

    #[test]
    fn hosts() {
        assert_eq!(host_of("https://API.Anthropic.com/v1").as_deref(), Some("api.anthropic.com"));
        assert_eq!(host_of("http://localhost:11434/v1").as_deref(), Some("localhost:11434"));
        assert_eq!(host_of("https://user:pw@example.org?x=1").as_deref(), Some("example.org"));
        assert_eq!(host_of("example.org/v1").as_deref(), Some("example.org"));
        assert_eq!(host_of(""), None);
        assert_eq!(host_of("https:///x"), None);
    }

    #[test]
    fn keys_differ_by_host_and_script() {
        let a = disclosure_key("https://api.anthropic.com/v1", "/s/a.fdx");
        assert_eq!(a, disclosure_key("https://API.anthropic.com", "/s/a.fdx"));
        assert_ne!(a, disclosure_key("https://api.openai.com/v1", "/s/a.fdx"));
        assert_ne!(a, disclosure_key("https://api.anthropic.com/v1", "/s/b.fdx"));
        assert_eq!(disclosure_key("", "/s/a.fdx"), None);
    }

    #[test]
    fn disclosure_names_everything() {
        let t = disclosure_text("api.anthropic.com", "PURPLE LIST.fdx", "PURPLE LIST.threads/");
        assert!(t.contains("api.anthropic.com") && t.contains("PURPLE LIST.fdx") && t.contains("PURPLE LIST.threads/"));
    }
}
