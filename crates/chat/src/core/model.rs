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

/// Parses an RFC 3339 time (`2026-11-15T09:30:00Z`, `…T10:30:00.5+01:00`;
/// `t`/`z` in lower case too) to Unix seconds, as thread files store
/// `created` and `updated`. Fractions of a second are dropped. `None` for
/// anything else, impossible dates and times included, and for times before
/// 1970. A leap second (`:60`) counts as the next second.
pub fn parse_rfc3339(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let d = b.get(r)?;
        d.iter().all(u8::is_ascii_digit).then(|| d.iter().fold(0, |n, c| n * 10 + i64::from(c - b'0')))
    };
    let at = |i: usize, c: &[u8]| b.get(i).is_some_and(|x| c.contains(x));
    if !(at(4, b"-") && at(7, b"-") && at(10, b"Tt") && at(13, b":") && at(16, b":")) {
        return None;
    }
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if !(1..=12).contains(&month)
        || day < 1
        || day > month_days[month as usize - 1]
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut i = 19;
    if at(i, b".") {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return None;
        }
    }
    let offset = match b.get(i)? {
        b'Z' | b'z' if i + 1 == b.len() => 0,
        sign @ (b'+' | b'-') if at(i + 3, b":") && i + 6 == b.len() => {
            let (h, m) = (num(i + 1..i + 3)?, num(i + 4..i + 6)?);
            if h > 23 || m > 59 {
                return None;
            }
            let o = h * 3600 + m * 60;
            if *sign == b'-' { -o } else { o }
        }
        _ => return None,
    };
    // Civil date to days since 1970-01-01 (proleptic Gregorian), after
    // Howard Hinnant; the inverse of `clock::rfc3339_utc`.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hour * 3600 + minute * 60 + second - offset).ok()
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

/// Where an account's requests go, as `scheme://host:port`, lowercased and
/// with the default port filled in: `https://API.Anthropic.com/v1` gives
/// `https://api.anthropic.com:443`. A URL without a scheme is taken as
/// https. `None` if there is no host.
pub fn origin_of(base_url: &str) -> Option<String> {
    let trimmed = base_url.trim();
    let scheme = trimmed.split_once("://").map_or("https".to_string(), |(s, _)| s.to_ascii_lowercase());
    let host = host_of(trimmed)?;
    // A port is what follows the last `:` after any IPv6 `]`.
    let after_v6 = host.rfind(']').map_or(0, |i| i + 1);
    let has_port =
        host[after_v6..].rsplit_once(':').is_some_and(|(_, p)| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    if has_port {
        return Some(format!("{scheme}://{host}"));
    }
    let host = host.trim_end_matches(':');
    let port = match scheme.as_str() {
        "http" => "80",
        _ => "443",
    };
    Some(format!("{scheme}://{host}:{port}"))
}

/// The key a first-use disclosure is remembered under: where the requests
/// go ([`origin_of`]: scheme, host and port) and the script's path, so a new
/// address or a new script asks again. `None` when the URL has no host.
pub fn disclosure_key(base_url: &str, script_path: &str) -> Option<String> {
    Some(format!("{}\u{1f}{script_path}", origin_of(base_url)?))
}

/// The disclosure shown before the first send: the app renders it as a card,
/// the CLI prints it. `host` is who the text goes to, as the writer should
/// read it: "Anthropic (api.anthropic.com)", or a bare host. `plain_http`:
/// the address is plain `http://` to another machine, so it says the key and
/// the script travel unencrypted.
pub fn disclosure_text(host: &str, script_name: &str, threads_folder: &str, plain_http: bool) -> String {
    let mut text = format!(
        "What gets sent. Your messages, and the parts of {script_name} the agent reads (scenes, search results, \
         sometimes the whole script), go to {host} with your key. So can the review files attached to the script, \
         including notes from collaborators. The agent writes its suggestions to a review file next to the script. \
         This conversation is saved in {threads_folder} next to the script, and contains excerpts of it."
    );
    if plain_http {
        text.push_str(&format!(
            " Warning: {host} is reached over plain http, so your key and the script travel unencrypted."
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, updated: u64) -> Entry {
        Entry { id: id.into(), title: id.into(), model: "m".into(), updated, suggestions: None, problem: None }
    }

    #[test]
    fn rfc3339_times() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2000-02-29T00:00:00Z"), Some(951_782_400));
        assert_eq!(parse_rfc3339("2026-11-15T09:30:45Z"), Some(1_794_735_045));
        assert_eq!(parse_rfc3339("2026-11-15t09:30:45.999z"), Some(1_794_735_045));
        assert_eq!(parse_rfc3339("2026-11-15T10:30:45+01:00"), Some(1_794_735_045));
        assert_eq!(parse_rfc3339("2026-11-15T04:00:45-05:30"), Some(1_794_735_045));
        assert_eq!(parse_rfc3339("2016-12-31T23:59:60Z"), parse_rfc3339("2017-01-01T00:00:00Z"));
        for bad in [
            "",
            "2026-11-15",
            "2026-11-15T09:30:00",
            "2026-11-15T09:30:00.Z",
            "2026-11-15T09:30Z",
            "2026-13-01T00:00:00Z",
            "2026-02-29T00:00:00Z",
            "1900-02-29T00:00:00Z",
            "2026-11-15T24:00:00Z",
            "2026-11-15T09:30:00+1:00",
            "2026-11-15T09:30:00Zx",
            "1969-12-31T23:59:59Z",
            "２026-11-15T09:30:00Z",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
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
    fn origins() {
        assert_eq!(origin_of("https://API.Anthropic.com/v1").as_deref(), Some("https://api.anthropic.com:443"));
        assert_eq!(origin_of("HTTP://localhost:11434/v1").as_deref(), Some("http://localhost:11434"));
        assert_eq!(origin_of("http://[::1]/v1").as_deref(), Some("http://[::1]:80"));
        assert_eq!(origin_of("http://[::1]:8080").as_deref(), Some("http://[::1]:8080"));
        assert_eq!(origin_of("example.org/v1").as_deref(), Some("https://example.org:443"));
        assert_eq!(origin_of(""), None);
    }

    #[test]
    fn keys_differ_by_scheme_host_port_and_script() {
        let a = disclosure_key("https://api.anthropic.com/v1", "/s/a.fdx");
        assert_eq!(a, disclosure_key("https://API.anthropic.com", "/s/a.fdx"));
        assert_eq!(a, disclosure_key("https://api.anthropic.com:443/v1", "/s/a.fdx"));
        assert_ne!(a, disclosure_key("http://api.anthropic.com/v1", "/s/a.fdx"));
        assert_ne!(a, disclosure_key("https://api.anthropic.com:8443/v1", "/s/a.fdx"));
        let local = disclosure_key("http://localhost:11434", "/s/a.fdx");
        assert_ne!(local, disclosure_key("http://localhost:8080", "/s/a.fdx"));
        assert_ne!(a, disclosure_key("https://api.openai.com/v1", "/s/a.fdx"));
        assert_ne!(a, disclosure_key("https://api.anthropic.com/v1", "/s/b.fdx"));
        assert_eq!(disclosure_key("", "/s/a.fdx"), None);
    }

    #[test]
    fn disclosure_names_everything() {
        let t = disclosure_text("api.anthropic.com", "PURPLE LIST.fdx", "PURPLE LIST.threads/", false);
        assert!(t.contains("api.anthropic.com") && t.contains("PURPLE LIST.fdx") && t.contains("PURPLE LIST.threads/"));
        assert!(
            t.contains("review files attached") && t.contains("collaborators") && t.contains("writes its suggestions")
        );
        assert!(!t.contains("plain http"));
        let t = disclosure_text("gpu.example.org", "a.fdx", "a.threads/", true);
        assert!(t.contains("plain http") && t.contains("unencrypted"), "{t}");
    }
}
