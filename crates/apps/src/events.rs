//! Change events for apps' pages, found by comparing snapshots (as the
//! disk watch does: no hooks inside the script view), paced, and written
//! as Server-Sent Events.
//!
//! The host takes a [`Snapshot`] of the open view every 250 ms (its light
//! part also with no page listening, so a page's first changes after
//! `hello` are found); [`diff`] gives the events; a [`Pacer`] holds
//! `script` and `writer` back so typing doesn't make every page re-read on
//! each key.
//! The server stamps each app's files every second; [`Quiet`] turns a
//! change followed by a second of quiet into `reload`.
//!
//! Times are milliseconds from any fixed start, passed in by the caller.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;

/// The open view, as far as apps' pages care.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// The open folder's project key; `None` when no script is open.
    pub project: Option<String>,
    /// The open script's file name.
    pub script: Option<String>,
    /// The document's event-log length: it grows on every edit.
    pub log_len: usize,
    /// Each attached review, in the session's order. Shared: the host
    /// hands on the same list while nothing in it changed, and [`diff`]
    /// skips comparing it then.
    pub reviews: Arc<Vec<ReviewState>>,
    /// Where the writer is.
    pub writer: Option<Writer>,
}

/// One attached review: its name and each suggestion's state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewState {
    pub name: String,
    pub items: Vec<ItemState>,
}

/// One suggestion: `index`, which stays the same while the review is open
/// (its place in the review's list), is what [`diff`] matches by; `number`
/// is the one tools show now (by script position: adding a suggestion
/// above renumbers those below); `status` in words (`pending`, …).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemState {
    pub index: usize,
    pub number: usize,
    pub status: String,
}

/// The writer's place: the scene heading and the paragraph id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Writer {
    pub scene: String,
    pub paragraph: u64,
}

/// An event sent to every listening page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// First on every stream.
    Hello { project: Option<String>, script: Option<String> },
    /// The text or the suggestions changed.
    Script,
    /// Suggestions in `review` were decided.
    Decided { review: String, numbers: Vec<usize> },
    /// The writer moved.
    Writer(Writer),
    /// `app`'s files changed: its pages reload.
    Reload { app: String },
    /// Another script (or none) is open.
    Open { project: Option<String>, script: Option<String> },
}

impl Event {
    /// The event's name on the stream.
    pub fn name(&self) -> &'static str {
        match self {
            Event::Hello { .. } => "hello",
            Event::Script => "script",
            Event::Decided { .. } => "decided",
            Event::Writer(_) => "writer",
            Event::Reload { .. } => "reload",
            Event::Open { .. } => "open",
        }
    }

    /// The event's data, as JSON.
    pub fn data(&self) -> serde_json::Value {
        match self {
            Event::Hello { project, script } => {
                json!({"project": project, "script": script, "version": crate::VERSION})
            }
            Event::Script => json!({}),
            Event::Decided { review, numbers } => json!({"review": review, "numbers": numbers}),
            Event::Writer(w) => json!({"scene": w.scene, "paragraph": w.paragraph}),
            Event::Reload { app } => json!({"app": app}),
            Event::Open { project, script } => json!({"project": project, "script": script}),
        }
    }

    /// The SSE frame: `event: <name>\ndata: <json>\n\n`. The JSON is on one
    /// line (serde never writes raw newlines), so one `data:` line suffices.
    pub fn frame(&self) -> String {
        format!("event: {}\ndata: {}\n\n", self.name(), self.data())
    }
}

/// The keep-alive comment, sent every 15 s; a failed write finds closed tabs.
pub const PING: &str = ": ping\n\n";

/// What changed from `prev` to `now`. Another folder or script open gives
/// only `open`: everything else about the old script no longer matters.
pub fn diff(prev: &Snapshot, now: &Snapshot) -> Vec<Event> {
    if prev.project != now.project || prev.script != now.script {
        return vec![Event::Open { project: now.project.clone(), script: now.script.clone() }];
    }
    let mut events = Vec::new();
    let mut script = prev.log_len != now.log_len;
    let mut decided = Vec::new();
    let same_reviews = Arc::ptr_eq(&prev.reviews, &now.reviews);
    let old: BTreeMap<&str, &ReviewState> =
        if same_reviews { BTreeMap::new() } else { prev.reviews.iter().map(|r| (r.name.as_str(), r)).collect() };
    if !same_reviews && old.len() != now.reviews.len() {
        script = true;
    }
    let reviews: &[ReviewState] = if same_reviews { &[] } else { &now.reviews };
    for review in reviews {
        let Some(before) = old.get(review.name.as_str()) else {
            script = true;
            continue;
        };
        let before: BTreeMap<usize, &str> = before.items.iter().map(|it| (it.index, it.status.as_str())).collect();
        if before.len() != review.items.len() {
            script = true;
        }
        let mut numbers = Vec::new();
        for item in &review.items {
            match before.get(&item.index) {
                None => script = true,
                Some(was) if *was != item.status => numbers.push(item.number),
                Some(_) => {}
            }
        }
        if !numbers.is_empty() {
            // A decision changes the suggestions (and maybe the text): pages
            // that only follow `script` see it too.
            script = true;
            decided.push(Event::Decided { review: review.name.clone(), numbers });
        }
    }
    if script {
        events.push(Event::Script);
    }
    events.extend(decided);
    if prev.writer != now.writer
        && let Some(w) = &now.writer
    {
        events.push(Event::Writer(w.clone()));
    }
    events
}

/// How long the log must be still before `script` is sent.
pub const SCRIPT_STILL_MS: u64 = 300;
/// The least time between two `script` events.
pub const SCRIPT_EVERY_MS: u64 = 1000;
/// The least time between two `writer` events.
pub const WRITER_EVERY_MS: u64 = 250;

/// Holds `script` until the text has been still for 300 ms and to at most
/// one a second, and `writer` to at most four a second (the latest place
/// wins). Other events pass straight through.
#[derive(Debug, Default)]
pub struct Pacer {
    /// When the last `script` change was seen, while one waits to be sent.
    script_changed: Option<u64>,
    script_sent: Option<u64>,
    writer_held: Option<Writer>,
    writer_sent: Option<u64>,
}

impl Pacer {
    pub fn new() -> Pacer {
        Pacer::default()
    }

    /// Takes this tick's events (from [`diff`], maybe none) at `now` and
    /// returns what to send now. Call it every tick, even with no events,
    /// so held events go out.
    pub fn step(&mut self, now: u64, events: Vec<Event>) -> Vec<Event> {
        let mut out = Vec::new();
        for event in events {
            match event {
                Event::Script => self.script_changed = Some(now),
                Event::Writer(w) => self.writer_held = Some(w),
                Event::Open { .. } => {
                    // A new script: whatever was held was about the old one.
                    self.script_changed = None;
                    self.writer_held = None;
                    out.push(event);
                }
                other => out.push(other),
            }
        }
        if let Some(changed) = self.script_changed
            && now.saturating_sub(changed) >= SCRIPT_STILL_MS
            && self.script_sent.is_none_or(|sent| now.saturating_sub(sent) >= SCRIPT_EVERY_MS)
        {
            self.script_changed = None;
            self.script_sent = Some(now);
            out.push(Event::Script);
        }
        if self.writer_held.is_some() && self.writer_sent.is_none_or(|sent| now.saturating_sub(sent) >= WRITER_EVERY_MS)
        {
            self.writer_sent = Some(now);
            out.extend(self.writer_held.take().map(Event::Writer));
        }
        out
    }
}

/// How long an app's files must stay unchanged before `reload`.
pub const QUIET_MS: u64 = 1000;

/// Turns each app's file stamps over time into `reload` once its files
/// changed and then stayed still for a second. The first stamps seen are
/// the baseline; an app that appears or goes away reloads nothing (no
/// page of it can be open yet, or any more).
///
/// The shell stamps only while a page listens, so it must reset this
/// (`Quiet::new()`) when listeners drop to 0: otherwise the first stamps
/// after a pause compare with ones from before it, and every app edited in
/// between reloads at once.
#[derive(Debug, Default)]
pub struct Quiet {
    stamps: Option<BTreeMap<String, u64>>,
    /// Apps changed and not yet reloaded: when they last changed.
    changed: BTreeMap<String, u64>,
}

impl Quiet {
    pub fn new() -> Quiet {
        Quiet::default()
    }

    /// Takes every app's stamp at `now` (see [`stamp`]) and returns the
    /// `reload` events due.
    pub fn step(&mut self, now: u64, stamps: BTreeMap<String, u64>) -> Vec<Event> {
        if let Some(prev) = &self.stamps {
            for (app, stamp) in &stamps {
                if prev.get(app).is_some_and(|old| old != stamp) {
                    self.changed.insert(app.clone(), now);
                }
            }
        }
        self.changed.retain(|app, _| stamps.contains_key(app));
        self.stamps = Some(stamps);
        let due: Vec<String> = self
            .changed
            .iter()
            .filter(|(_, at)| now.saturating_sub(**at) >= QUIET_MS)
            .map(|(app, _)| app.clone())
            .collect();
        due.into_iter()
            .map(|app| {
                self.changed.remove(&app);
                Event::Reload { app }
            })
            .collect()
    }
}

/// One app's stamp from its files' (path relative to the app, size,
/// modification time in any fixed unit), `data/` already left out by the
/// caller. Order doesn't matter.
pub fn stamp<'a>(files: impl IntoIterator<Item = (&'a str, u64, u64)>) -> u64 {
    let mut rows: Vec<(&str, u64, u64)> = files.into_iter().collect();
    rows.sort_unstable();
    let mut bytes = Vec::new();
    for (path, size, mtime) in rows {
        bytes.extend_from_slice(path.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&size.to_le_bytes());
        bytes.extend_from_slice(&mtime.to_le_bytes());
    }
    crate::paths::fnv1a(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(log_len: usize) -> Snapshot {
        Snapshot {
            project: Some("00112233445566aa".into()),
            script: Some("PURPLE LIST.fdx".into()),
            log_len,
            reviews: Arc::new(vec![ReviewState { name: "a.review.txt".into(), items: vec![item(0, 1, "pending")] }]),
            writer: None,
        }
    }

    fn item(index: usize, number: usize, status: &str) -> ItemState {
        ItemState { index, number, status: status.into() }
    }

    fn writer(p: u64) -> Writer {
        Writer { scene: "INT. KITCHEN - NIGHT".into(), paragraph: p }
    }

    #[test]
    fn nothing_changed_nothing_sent() {
        assert!(diff(&snap(3), &snap(3)).is_empty());
        // The same shared list: not compared at all.
        let a = snap(3);
        let mut b = a.clone();
        assert!(Arc::ptr_eq(&a.reviews, &b.reviews));
        b.log_len = 4;
        assert_eq!(diff(&a, &b), vec![Event::Script]);
    }

    #[test]
    fn an_edit_is_script() {
        assert_eq!(diff(&snap(3), &snap(4)), vec![Event::Script]);
    }

    #[test]
    fn a_decision_is_decided_and_script() {
        let mut now = snap(3);
        Arc::make_mut(&mut now.reviews)[0].items[0].status = "accepted".into();
        assert_eq!(
            diff(&snap(3), &now),
            vec![Event::Script, Event::Decided { review: "a.review.txt".into(), numbers: vec![1] }]
        );
    }

    #[test]
    fn new_suggestions_and_reviews_are_script() {
        let mut now = snap(3);
        Arc::make_mut(&mut now.reviews)[0].items.push(item(1, 2, "pending"));
        assert_eq!(diff(&snap(3), &now), vec![Event::Script]);
        // Added above the first: it is renumbered, not decided.
        let mut now = snap(3);
        Arc::make_mut(&mut now.reviews)[0].items = vec![item(0, 2, "pending"), item(1, 1, "pending")];
        assert_eq!(diff(&snap(3), &now), vec![Event::Script]);
        // Then decided: reported by its number now.
        let mut later = now.clone();
        Arc::make_mut(&mut later.reviews)[0].items[0].status = "kept".into();
        assert_eq!(
            diff(&now, &later),
            vec![Event::Script, Event::Decided { review: "a.review.txt".into(), numbers: vec![2] }]
        );
        let mut now = snap(3);
        Arc::make_mut(&mut now.reviews).push(ReviewState { name: "b.review.txt".into(), items: vec![] });
        assert_eq!(diff(&snap(3), &now), vec![Event::Script]);
        let mut now = snap(3);
        Arc::make_mut(&mut now.reviews).clear();
        assert_eq!(diff(&snap(3), &now), vec![Event::Script]);
    }

    #[test]
    fn another_script_is_only_open() {
        let mut now = snap(9);
        now.script = Some("OTHER.fdx".into());
        now.writer = Some(writer(1));
        assert_eq!(diff(&snap(3), &now), vec![Event::Open { project: now.project.clone(), script: now.script }]);
        let none = Snapshot::default();
        assert_eq!(diff(&snap(3), &none), vec![Event::Open { project: None, script: None }]);
    }

    #[test]
    fn the_writer_moving() {
        let mut now = snap(3);
        now.writer = Some(writer(41));
        assert_eq!(diff(&snap(3), &now), vec![Event::Writer(writer(41))]);
    }

    #[test]
    fn a_burst_of_typing_gives_one_script_after_the_pause() {
        let mut pacer = Pacer::new();
        let mut sent = Vec::new();
        // A key every 250 ms tick for 3 s.
        for t in (0..3000).step_by(250) {
            sent.extend(pacer.step(t, vec![Event::Script]).into_iter().map(|e| (t, e)));
        }
        assert!(sent.is_empty(), "{sent:?}");
        // Then still: sent at the first tick 300 ms after the last key.
        for t in (3000..6000).step_by(250) {
            sent.extend(pacer.step(t, vec![]).into_iter().map(|e| (t, e)));
        }
        assert_eq!(sent, vec![(3250, Event::Script)]);
    }

    #[test]
    fn script_at_most_once_a_second() {
        let mut pacer = Pacer::new();
        let mut sent = Vec::new();
        // A change every third tick: each followed by 500 ms of stillness,
        // so only the once-a-second rule holds some back.
        for t in (0..4000).step_by(250) {
            let events = if t % 750 == 0 { vec![Event::Script] } else { vec![] };
            sent.extend(pacer.step(t, events).into_iter().map(|_| t));
        }
        for pair in sent.windows(2) {
            assert!(pair[1] - pair[0] >= SCRIPT_EVERY_MS, "{sent:?}");
        }
        assert!(sent.len() >= 3, "{sent:?}");
    }

    #[test]
    fn held_writer_moves_send_the_latest() {
        let mut pacer = Pacer::new();
        assert_eq!(pacer.step(0, vec![Event::Writer(writer(1))]), vec![Event::Writer(writer(1))]);
        // Two moves within 250 ms of the last send: held, the latest wins.
        assert!(pacer.step(100, vec![Event::Writer(writer(2))]).is_empty());
        assert!(pacer.step(200, vec![Event::Writer(writer(3))]).is_empty());
        assert_eq!(pacer.step(250, vec![]), vec![Event::Writer(writer(3))]);
        assert!(pacer.step(500, vec![]).is_empty());
    }

    #[test]
    fn open_passes_and_drops_what_was_held() {
        let mut pacer = Pacer::new();
        pacer.step(0, vec![Event::Writer(writer(1))]);
        pacer.step(100, vec![Event::Script, Event::Writer(writer(2))]);
        let open = Event::Open { project: None, script: None };
        assert_eq!(pacer.step(150, vec![open.clone()]), vec![open]);
        assert!(pacer.step(2000, vec![]).is_empty());
        let decided = Event::Decided { review: "r".into(), numbers: vec![1] };
        assert_eq!(pacer.step(2001, vec![decided.clone()]), vec![decided]);
    }

    fn stamps(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
        pairs.iter().map(|(a, s)| (a.to_string(), *s)).collect()
    }

    #[test]
    fn quiet_reloads_a_second_after_the_last_change() {
        let mut quiet = Quiet::new();
        let mut sent = Vec::new();
        // Stamped every second; edits seen at 1 s, 2 s (three edits in two
        // seconds collapse into what each stamp sees).
        let seq = [(0, 1), (1000, 2), (2000, 3), (3000, 3), (4000, 3)];
        for (t, s) in seq {
            sent.extend(quiet.step(t, stamps(&[("scene-list", s), ("other", 7)])).into_iter().map(|e| (t, e)));
        }
        assert_eq!(sent, vec![(3000, Event::Reload { app: "scene-list".into() })]);
    }

    #[test]
    fn quiet_baseline_new_and_removed_apps() {
        let mut quiet = Quiet::new();
        assert!(quiet.step(0, stamps(&[("a", 1)])).is_empty());
        assert!(quiet.step(1000, stamps(&[("a", 1), ("b", 1)])).is_empty());
        assert!(quiet.step(2000, stamps(&[("b", 2)])).is_empty());
        assert!(quiet.step(3000, stamps(&[])).is_empty());
        assert!(quiet.step(4000, stamps(&[("b", 9)])).is_empty());
    }

    #[test]
    fn stamps_ignore_order_and_see_changes() {
        let a = stamp([("index.html", 10, 5), ("app.js", 3, 5)]);
        assert_eq!(a, stamp([("app.js", 3, 5), ("index.html", 10, 5)]));
        assert_ne!(a, stamp([("app.js", 3, 6), ("index.html", 10, 5)]));
        assert_ne!(a, stamp([("app.js", 4, 5), ("index.html", 10, 5)]));
        assert_ne!(a, stamp([("app.js", 3, 5)]));
    }

    #[test]
    fn frames() {
        assert_eq!(Event::Script.frame(), "event: script\ndata: {}\n\n");
        assert_eq!(
            Event::Writer(Writer { scene: "INT. A\nB".into(), paragraph: 41 }).frame(),
            "event: writer\ndata: {\"scene\":\"INT. A\\nB\",\"paragraph\":41}\n\n"
        );
        let hello = Event::Hello { project: Some("k".into()), script: None }.frame();
        assert!(hello.starts_with("event: hello\ndata: {\"project\":\"k\",\"script\":null,\"version\":"), "{hello}");
        assert_eq!(
            Event::Decided { review: "r".into(), numbers: vec![3, 4] }.frame(),
            "event: decided\ndata: {\"review\":\"r\",\"numbers\":[3,4]}\n\n"
        );
    }
}
