//! The app's side of noticing that another program (the `scriptorium` CLI,
//! the MCP server, a coding agent through the plugin) changed this script's
//! files while the app has them open. The file checks and the replay of the
//! app's unsaved work live in `scriptorium_review::sessionfile::sync`; this
//! module keeps what only the app needs: when to reload given a rewrite in
//! progress, what to tell the writer about a reload, and where to put a
//! session it couldn't save.

use scriptorium_core::Document;
use scriptorium_review::sessionfile::Disk;
use scriptorium_review::{Session, Status};
use scriptorium_tools::item_numbers;

/// What to do before saving (or on a periodic look at the files).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// Nothing changed on disk: saving is safe.
    Save,
    /// Someone else wrote: reload (and keep the app's unsaved changes on top).
    Reload,
    /// A rewrite is being typed: neither save nor reload until it is
    /// finished (finishing it saves, or reloads). Not saving halfway keeps the
    /// whole rewrite unsaved, so if another program writes meanwhile, the
    /// reload still merges all of it into one undo step (a merge can't reach
    /// past the other program's steps to a half saved earlier).
    Wait,
}

pub fn plan(session: &Disk, any_review_changed: bool, rewriting: bool) -> Plan {
    let changed = *session == Disk::Changed || any_review_changed;
    match (changed, rewriting) {
        (_, true) => Plan::Wait,
        (false, false) => Plan::Save,
        (true, false) => Plan::Reload,
    }
}

/// What a reload brought in, for the status bar. Reloads that come in quick
/// succession are merged into one notice ([`Summary::merge`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub attached: Vec<String>,
    pub detached: Vec<String>,
    /// (review name, how many) new suggestions.
    pub new_suggestions: Vec<(String, usize)>,
    /// (review name, suggestion number) decided by the other program.
    pub decided: Vec<(String, usize)>,
    pub undecided: Vec<(String, usize)>,
    /// Lines changed by the other program outside its decisions.
    pub edited: usize,
    /// The app's unsaved changes: how many were kept, and the text of those
    /// that couldn't be applied (also written to the log).
    pub kept: usize,
    pub lost: Vec<String>,
    pub notes: Vec<String>,
}

impl Summary {
    pub fn merge(&mut self, other: Summary) {
        fn add_all<T: PartialEq>(to: &mut Vec<T>, from: Vec<T>) {
            for x in from {
                if !to.contains(&x) {
                    to.push(x);
                }
            }
        }
        add_all(&mut self.attached, other.attached);
        add_all(&mut self.detached, other.detached);
        for (name, n) in other.new_suggestions {
            match self.new_suggestions.iter_mut().find(|(m, _)| *m == name) {
                Some((_, k)) => *k += n,
                None => self.new_suggestions.push((name, n)),
            }
        }
        for d in other.decided {
            self.undecided.retain(|u| *u != d);
            add_all(&mut self.decided, vec![d]);
        }
        for u in other.undecided {
            self.decided.retain(|d| *d != u);
            add_all(&mut self.undecided, vec![u]);
        }
        self.edited += other.edited;
        self.kept += other.kept;
        self.lost.extend(other.lost);
        add_all(&mut self.notes, other.notes);
    }

    /// One short line, or None if there's nothing worth saying.
    pub fn render(&self) -> Option<String> {
        let mut parts: Vec<String> = vec![];
        parts.extend(self.attached.iter().map(|n| format!("{n} attached from outside the app")));
        for (name, n) in &self.new_suggestions {
            parts.push(format!("{n} new suggestion{} in {name} (from outside the app)", plural(*n)));
        }
        let numbers = |list: &[(String, usize)]| {
            let one_review = list.iter().all(|(n, _)| *n == list[0].0);
            let nums: Vec<String> = list
                .iter()
                .map(|(name, k)| if one_review { format!("#{k}") } else { format!("{name} #{k}") })
                .collect();
            nums.join(", ")
        };
        if !self.decided.is_empty() {
            parts.push(format!("Decided outside the app: {}", numbers(&self.decided)));
        }
        if !self.undecided.is_empty() {
            parts.push(format!("Undone outside the app: {}", numbers(&self.undecided)));
        }
        if self.edited > 0 {
            parts.push(format!("{} line{} edited outside the app", self.edited, plural(self.edited)));
        }
        parts.extend(self.detached.iter().map(|n| format!("{n} detached from outside the app")));
        if let Some(first) = self.lost.first() {
            let n = self.lost.len();
            let kept = match self.kept {
                0 => String::new(),
                k => format!("{k} change{} kept, ", plural(k)),
            };
            parts.push(format!(
                "reloaded: {kept}{n} couldn't apply (\u{201c}{}\u{201d}) \u{2014} text saved in log",
                excerpt(first, 30)
            ));
        }
        parts.extend(self.notes.iter().cloned());
        (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn excerpt(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}\u{2026}", s.chars().take(max).collect::<String>())
    }
}

/// What another program changed from (`old`, `before`) — the reviews and the
/// document as the app last synced them — to (`new`, `after`), as reloaded.
pub fn summarize(old: &Session, before: &Document, new: &Session, after: &Document) -> Summary {
    let mut s = Summary::default();
    let mut decided_paras = std::collections::HashSet::new();
    for (r, review) in new.reviews.iter().enumerate() {
        let Some(o) = old.reviews.iter().position(|x| x.id == review.id) else {
            s.attached.push(review.name.clone());
            continue;
        };
        let numbers = item_numbers(after, &review.items);
        let mut fresh = 0;
        for (i, item) in review.items.iter().enumerate() {
            let Some(oi) = old.reviews[o].items.iter().position(|x| x.entry == item.entry) else {
                fresh += 1;
                continue;
            };
            let (was, now) = (old.status(before, o, oi), new.status(after, r, i));
            if was != now {
                decided_paras.insert(item.para);
                let at = (review.name.clone(), numbers[i]);
                if now == Status::Pending { s.undecided.push(at) } else { s.decided.push(at) }
            }
        }
        if fresh > 0 {
            s.new_suggestions.push((review.name.clone(), fresh));
        }
    }
    s.detached =
        old.reviews.iter().filter(|o| !new.reviews.iter().any(|r| r.id == o.id)).map(|o| o.name.clone()).collect();
    let was: std::collections::HashMap<_, _> = before.paragraphs().iter().map(|p| (p.id, p)).collect();
    s.edited = after
        .paragraphs()
        .iter()
        .filter(|p| !decided_paras.contains(&p.id) && was.get(&p.id).is_none_or(|w| *w != *p))
        .count();
    s
}

/// `secs` since the Unix epoch as `YYYYMMDD-HHMMSS` (UTC), for file names.
pub fn timestamp(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil from days (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Where to put a session file set aside (one that can't be used, or work
/// that couldn't be saved): `X.scriptorium.set-aside-<UTC time>.json` for
/// script `X.fdx` in `dir`, with `-2`, `-3`... if that name is taken so an
/// earlier one is never overwritten. The reason goes in the log and notice.
pub fn set_aside_path(dir: &std::path::Path, script_file_name: &str) -> std::path::PathBuf {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    set_aside_path_at(dir, script_file_name, secs)
}

fn set_aside_path_at(dir: &std::path::Path, script_file_name: &str, secs: u64) -> std::path::PathBuf {
    let stem = script_file_name.rsplit_once('.').map_or(script_file_name, |(stem, _)| stem);
    let base = format!("{stem}.scriptorium.set-aside-{}", timestamp(secs));
    (1..)
        .map(|n| match n {
            1 => dir.join(format!("{base}.json")),
            n => dir.join(format!("{base}-{n}.json")),
        })
        .find(|p| !p.exists())
        .expect("some name is free")
}

#[cfg(test)]
mod tests {
    use scriptorium_core::{ElementKind, ParaId, Paragraph, Script, Selection};
    use scriptorium_review::{Decision, Review};

    use super::*;

    #[test]
    fn plan_waits_for_a_rewrite_and_otherwise_reloads() {
        use scriptorium_review::sessionfile::Seen;
        assert_eq!(plan(&Disk::Unchanged, false, true), Plan::Wait);
        assert_eq!(plan(&Disk::Touched(Seen::default()), false, false), Plan::Save);
        assert_eq!(plan(&Disk::Changed, false, false), Plan::Reload);
        assert_eq!(plan(&Disk::Unchanged, true, false), Plan::Reload);
        assert_eq!(plan(&Disk::Changed, false, true), Plan::Wait);
    }

    #[test]
    fn timestamps_and_aside_names() {
        assert_eq!(timestamp(0), "19700101-000000");
        assert_eq!(timestamp(1_794_667_830), "20261114-145030");
        let dir = std::env::temp_dir().join(format!("scriptorium-aside-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = set_aside_path_at(&dir, "film.fdx", 0);
        assert_eq!(first.file_name().unwrap(), "film.scriptorium.set-aside-19700101-000000.json");
        std::fs::write(&first, "").unwrap();
        let second = set_aside_path_at(&dir, "film.fdx", 0);
        assert_eq!(second.file_name().unwrap(), "film.scriptorium.set-aside-19700101-000000-2.json");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn notices_are_short_and_merge() {
        let mut a = Summary { new_suggestions: vec![("dialogue-pass".into(), 1)], ..Summary::default() };
        assert_eq!(a.render().unwrap(), "1 new suggestion in dialogue-pass (from outside the app)");
        a.merge(Summary { new_suggestions: vec![("dialogue-pass".into(), 1)], ..Summary::default() });
        a.merge(Summary { decided: vec![("dialogue-pass".into(), 3)], ..Summary::default() });
        assert_eq!(
            a.render().unwrap(),
            "2 new suggestions in dialogue-pass (from outside the app) · Decided outside the app: #3"
        );
        // Decided then undone in the next reload: only the latest state is said.
        a.merge(Summary { undecided: vec![("dialogue-pass".into(), 3)], ..Summary::default() });
        assert_eq!(
            a.render().unwrap(),
            "2 new suggestions in dialogue-pass (from outside the app) · Undone outside the app: #3"
        );
        let lost = Summary { kept: 1, lost: vec!["I am going to town tomorrow.".into()], ..Summary::default() };
        assert_eq!(
            lost.render().unwrap(),
            "reloaded: 1 change kept, 1 couldn't apply (“I am going to town tomorrow.”) — text saved in log"
        );
        assert_eq!(Summary::default().render(), None);
    }

    const REVIEW: &str = "SCRIPT REVIEW v1\nScript: film.fdx\nId: aaaaaaaaaaaa\n\n---\nBEFORE: two\nPARA: 1\nAFTER: TWO\n---\nBEFORE: three\nPARA: 2\nAFTER: THREE\n";

    fn script() -> Script {
        let paragraphs = ["one", "two", "three"]
            .iter()
            .enumerate()
            .map(|(i, l)| Paragraph::plain(ParaId(i as u64), ElementKind::Action, *l))
            .collect();
        Script { paragraphs }
    }

    fn session(text: &str, doc: &Document) -> Session {
        let caps = |_: &ElementKind| false;
        Session::new(vec![Review::open_live("dialogue-pass", text, &script(), doc, caps, "film.fdx")])
    }

    #[test]
    fn a_reload_is_described_by_what_changed() {
        let before = Document::new(script());
        let old = session(REVIEW, &before);
        // Another program added a suggestion, decided #2 (line "three"), and edited line 0.
        let more = format!("{REVIEW}---\nBEFORE: one\nPARA: 0\nAFTER: ONE\n");
        let mut after = before.clone();
        let new = session(&more, &after);
        let sel = Selection::cursor(after.start());
        let three = new.reviews[0].items.iter().position(|it| it.para == ParaId(2)).unwrap();
        new.decide(&mut after, sel, 0, three, Decision::Accept(0)).unwrap();
        after.replace_text(sel, ParaId(0), "uno", &[]).unwrap();
        let s = summarize(&old, &before, &new, &after);
        assert_eq!(s.new_suggestions, [("dialogue-pass".to_string(), 1)]);
        assert_eq!(s.decided, [("dialogue-pass".to_string(), 3)], "numbered in script order");
        assert_eq!(s.edited, 1);
        assert_eq!(
            s.render().unwrap(),
            "1 new suggestion in dialogue-pass (from outside the app) · Decided outside the app: #3 · 1 line edited outside the app"
        );
    }

    #[test]
    fn a_reload_that_dropped_the_apps_saved_decision_puts_it_back() {
        use scriptorium_review::sessionfile::{rebuild, still_synced, with_unsaved};
        // The app decides #1 (line "two") and saves it. A CLI command that
        // read the file before that save decides #2 and writes, without #1.
        let shared = Document::new(script());
        let s = session(REVIEW, &shared);
        let item = |p: u64| s.reviews[0].items.iter().position(|it| it.para == ParaId(p)).unwrap();
        let sel = Selection::cursor(shared.start());
        let mut app = shared.clone();
        s.decide(&mut app, sel, 0, item(1), Decision::Accept(0)).unwrap();
        let saved = app.log().len();
        let mut theirs = shared.clone();
        s.decide(&mut theirs, sel, 0, item(2), Decision::Accept(0)).unwrap();

        // As `ScriptView::reload` does it.
        let synced = still_synced(saved, app.log(), theirs.log());
        let start = rebuild(&app, synced).unwrap();
        let replayed = with_unsaved(start.clone(), &app.log()[synced..], theirs.clone());
        let summary = summarize(&s, &start, &s, &theirs);

        let texts: Vec<String> = replayed.doc.paragraphs().iter().map(|p| p.text()).collect();
        assert_eq!(texts, ["one", "TWO", "THREE"], "the app's decision is back on top of theirs");
        assert!(replayed.lost.is_empty());
        assert!(!replayed.kept.is_empty());
        assert_eq!(s.status(&replayed.doc, 0, item(1)), Status::Accepted(0));
        // Only their decision is theirs; the app's own isn't "undone outside".
        assert_eq!(summary.decided, [("dialogue-pass".to_string(), 2)]);
        assert!(summary.undecided.is_empty());
    }
}
