//! Keeping a host's open session in step with the session file when another
//! program (the `scriptorium` CLI, the MCP server, an agent) writes it while
//! the host has it open: noticing the change, and putting the host's unsaved
//! work back on top of what was reloaded.
//!
//! Pure: the shell stats and reads the files and passes what it found in.
//! - The session file is compared by contents (a [`hash`] of what the host
//!   last read or wrote); its modification time only spares re-reading a
//!   file whose time hasn't moved since its contents were last checked.
//! - A review file is compared with `Review::text()`, the raw text the host
//!   holds for it, as the host contract asks.
//! - A file that is missing now is not a change: there is nothing to reload,
//!   and the host's next save recreates the session file.

use std::collections::{BTreeSet, HashMap};
use std::time::SystemTime;

use scriptorium_core::doc::{DocError, Event, Group, Op, Transaction};
use scriptorium_core::{Document, ParaId, Paragraph};

use super::hash;

/// What the host last knew about the session file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Seen {
    /// Hash of the contents the host last read or wrote (None: no file).
    hash: Option<String>,
    /// The file's modification time when its contents were last found to be
    /// `hash`, if known. Unknown right after the host writes the file (the
    /// time read after a write might already be another writer's).
    mtime: Option<SystemTime>,
}

impl Seen {
    /// The file as just read: `mtime` taken *before* reading `contents`, so a
    /// write in between shows up as a newer time on the next check.
    pub fn read(mtime: Option<SystemTime>, contents: Option<&str>) -> Self {
        Self { hash: contents.map(hash), mtime: contents.and(mtime) }
    }

    /// The file as the host just wrote it.
    pub fn wrote(contents: &str) -> Self {
        Self { hash: Some(hash(contents)), mtime: None }
    }
}

/// The session file compared with what the host last knew.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disk {
    Unchanged,
    /// Same contents, but now the time is known: remember this.
    Touched(Seen),
    Changed,
}

/// Check the session file: `mtime` is its modification time now (None:
/// missing or unreadable); `read` reads its contents, and is only called
/// when the time doesn't settle it.
pub fn check(seen: &Seen, mtime: Option<SystemTime>, read: impl FnOnce() -> Option<String>) -> Disk {
    if mtime.is_none() || (seen.mtime.is_some() && seen.mtime == mtime) {
        return Disk::Unchanged;
    }
    let Some(now) = read() else { return Disk::Unchanged };
    let h = hash(&now);
    if seen.hash.as_deref() == Some(h.as_str()) { Disk::Touched(Seen { hash: Some(h), mtime }) } else { Disk::Changed }
}

/// Has a review file changed from the text the host holds for it? A file
/// that can't be read now is left alone (the next reload says so if it's gone).
pub fn review_changed(held: &str, on_disk: Option<&str>) -> bool {
    on_disk.is_some_and(|d| d != held)
}

/// How much of the host's log the session file still holds after another
/// program wrote it: the host last synced `synced` events (`host_log`'s
/// first `synced`), and `reloaded_log` is the log resumed from the file now.
///
/// Normally the other program read the host's events and appended its own,
/// so this is `synced`. But a program that read the file before the host's
/// last save, and wrote after it, drops the host's newest saved events: then
/// only the events both logs share are synced, and the host's events after
/// them are its unsaved work again (replayed by [`with_unsaved`], so they
/// aren't lost, and aren't taken for the other program undoing them).
pub fn still_synced(synced: usize, host_log: &[Event], reloaded_log: &[Event]) -> usize {
    synced.min(common_prefix(host_log, reloaded_log))
}

/// How many events `a` and `b` start with in common.
pub fn common_prefix(a: &[Event], b: &[Event]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// `doc` as it was when its log was `len` events long, with the undo history
/// it had then (replayed from its starting point).
pub fn rebuild(doc: &Document, len: usize) -> Result<Document, DocError> {
    let log = doc.log();
    Document::with_meta(doc.initial().clone(), doc.initial_meta().clone()).replay_onto(&log[..len.min(log.len())])
}

/// A paragraph or a metadata value (a decision) the host changed since it
/// last synced with the session file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The paragraph, and its text as the host has it ("" if the host removed it).
    Para(ParaId, String),
    /// A metadata key (`review/<id>/<entry>` for a decision).
    Meta(String),
}

/// The host's unsaved work put back on top of a reloaded document.
#[derive(Debug, Clone)]
pub struct Replayed {
    pub doc: Document,
    /// The host's changes that `doc` has.
    pub kept: Vec<Change>,
    /// The host's changes `doc` doesn't have: another program changed the
    /// same paragraph or decision first. The text is the host's, to recover.
    pub lost: Vec<Change>,
}

/// Replay the host's `unsaved` events on top of `reloaded` (the document
/// resumed from the session file another program wrote). `synced` is the
/// host's document as the session file last held it, with its undo history
/// ([`rebuild`] at the log length last read or written).
///
/// Event by event, so every one that still fits is kept:
/// - An edit applies only if every paragraph (and decision) it touches is,
///   in `reloaded`, exactly as the host had it just before the edit, so
///   typing never lands at an offset in text another program changed.
///   Edits that depend on a dropped one are dropped too.
/// - An undo or redo is replayed as the concrete change it made to the
///   host's document (so it reverts exactly what it reverted then, never
///   the other program's newest step), recorded as a new edit.
/// - A merge of undo steps (a finished rewrite) merges only the host's own
///   steps replayed here, never steps another program added meanwhile.
pub fn with_unsaved(synced: Document, unsaved: &[Event], reloaded: Document) -> Replayed {
    let start = synced.clone();
    let mut sim = synced;
    let mut target = reloaded;
    // Undo steps at or above `clean_since` in `sim` match those at
    // `+ offset` in `target`, one for one (see the end of the loop).
    let mut clean_since = sim.undo_depth();
    let mut offset = target.undo_depth() as isize - clean_since as isize;
    for e in unsaved {
        let (s0, t0) = (sim.undo_depth(), target.undo_depth());
        let mut regular = true;
        match e {
            Event::Edit(t) => {
                // The host's own log: it always replays on its own document.
                let Ok(next) = sim.clone().replay_onto(std::slice::from_ref(e)) else { break };
                regular = applies(&sim, &target, t) && try_apply(&mut target, e.clone());
                sim = next;
            }
            Event::Undo | Event::Redo => {
                let mut next = sim.clone();
                let done = if matches!(e, Event::Redo) { next.redo() } else { next.undo() };
                if let Some(sel) = done {
                    regular = false;
                    if let Some(ops) = ops_between(&sim, &next).filter(|o| !o.is_empty()) {
                        let t = Transaction { ops, before: sel, after: sel, group: Group::Other };
                        if applies(&sim, &target, &t) {
                            try_apply(&mut target, Event::Edit(t));
                        }
                    }
                }
                sim = next;
            }
            Event::MergeUndo { depth } => {
                let from = (*depth).max(clean_since) as isize + offset;
                sim.merge_undo_since(*depth);
                if from >= 0 {
                    target.merge_undo_since(from as usize);
                }
                regular = *depth >= clean_since;
            }
        }
        let (s1, t1) = (sim.undo_depth(), target.undo_depth());
        if !regular || s1 as isize - s0 as isize != t1 as isize - t0 as isize {
            clean_since = s1;
            offset = t1 as isize - s1 as isize;
        }
    }
    let (kept, lost) = net_changes(&start, &sim, &target);
    Replayed { doc: target, kept, lost }
}

fn try_apply(doc: &mut Document, e: Event) -> bool {
    match doc.clone().replay_onto(&[e]) {
        Ok(d) => {
            *doc = d;
            true
        }
        Err(_) => false,
    }
}

/// Is everything `t` touches the same in `target` as in `sim` (the host's
/// document just before `t`)?
fn applies(sim: &Document, target: &Document, t: &Transaction) -> bool {
    let same = |id: ParaId| sim.paragraph(id) == target.paragraph(id);
    let id_at = |d: &Document, ix: usize| d.paragraphs().get(ix).map(|p| p.id);
    let around =
        |ix: usize| (ix == 0 || id_at(sim, ix - 1) == id_at(target, ix - 1)) && id_at(sim, ix) == id_at(target, ix);
    t.ops.iter().all(|op| match op {
        Op::Splice { para, .. } | Op::Split { para, .. } | Op::SetKind { para, .. } => same(*para),
        Op::Merge { para, next, .. } => same(*para) && same(*next),
        Op::InsertPara { index, para } | Op::RemovePara { index, para } => same(para.id) && around(*index),
        Op::SetMeta { key, .. } => sim.meta(key) == target.meta(key),
    })
}

/// The ops that turn `before` into `after` (two states of one document):
/// paragraphs added, then removed, then changed in kind or text (each whole
/// paragraph's text replaced), then metadata. None if paragraphs that are
/// in both changed order.
fn ops_between(before: &Document, after: &Document) -> Option<Vec<Op>> {
    let (b, a) = (before.paragraphs(), after.paragraphs());
    let in_b: BTreeSet<ParaId> = b.iter().map(|p| p.id).collect();
    let in_a: BTreeSet<ParaId> = a.iter().map(|p| p.id).collect();
    let mut work: Vec<Paragraph> = b.to_vec();
    let mut ops = vec![];
    for (ai, p) in a.iter().enumerate() {
        if in_b.contains(&p.id) {
            continue;
        }
        let index = match ai {
            0 => 0,
            _ => work.iter().position(|w| w.id == a[ai - 1].id).map_or(0, |k| k + 1),
        };
        work.insert(index, p.clone());
        ops.push(Op::InsertPara { index, para: p.clone() });
    }
    for k in (0..work.len()).rev() {
        if !in_a.contains(&work[k].id) {
            let para = work.remove(k);
            ops.push(Op::RemovePara { index: k, para });
        }
    }
    if !work.iter().map(|p| p.id).eq(a.iter().map(|p| p.id)) {
        return None;
    }
    for (w, p) in work.iter().zip(a) {
        if w.kind != p.kind {
            ops.push(Op::SetKind { para: p.id, from: w.kind.clone(), to: p.kind.clone() });
        }
        if w.spans != p.spans {
            ops.push(Op::Splice { para: p.id, at: 0, removed: w.spans.clone(), inserted: p.spans.clone() });
        }
    }
    let keys: BTreeSet<&String> = before.all_meta().keys().chain(after.all_meta().keys()).collect();
    for key in keys {
        let (from, to) = (before.meta(key), after.meta(key));
        if from != to {
            ops.push(Op::SetMeta { key: key.clone(), from: from.map(Into::into), to: to.map(Into::into) });
        }
    }
    Some(ops)
}

/// What the host changed from `start` to `host`, split by whether `target`
/// ended up with it.
fn net_changes(start: &Document, host: &Document, target: &Document) -> (Vec<Change>, Vec<Change>) {
    let by_id =
        |d: &Document| -> HashMap<ParaId, Paragraph> { d.paragraphs().iter().map(|p| (p.id, p.clone())).collect() };
    let (s, h, t) = (by_id(start), by_id(host), by_id(target));
    let ids: BTreeSet<ParaId> = s.keys().chain(h.keys()).copied().collect();
    let (mut kept, mut lost) = (vec![], vec![]);
    for id in ids {
        let mine = h.get(&id);
        if s.get(&id) == mine {
            continue;
        }
        let change = Change::Para(id, mine.map(|p| p.text()).unwrap_or_default());
        if t.get(&id) == mine { kept.push(change) } else { lost.push(change) }
    }
    let keys: BTreeSet<&String> = start.all_meta().keys().chain(host.all_meta().keys()).collect();
    for key in keys {
        let mine = host.meta(key);
        if start.meta(key) == mine {
            continue;
        }
        let change = Change::Meta(key.clone());
        if target.meta(key) == mine { kept.push(change) } else { lost.push(change) }
    }
    (kept, lost)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use scriptorium_core::{ElementKind, Script, Selection};

    use super::*;

    fn t(s: u64) -> Option<SystemTime> {
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(s))
    }

    #[test]
    fn same_time_is_not_reread() {
        let seen = Seen::read(t(1), Some("a"));
        assert_eq!(check(&seen, t(1), || panic!("read")), Disk::Unchanged);
    }

    #[test]
    fn new_time_same_contents_is_touched_then_settled() {
        let seen = Seen::read(t(1), Some("a"));
        let Disk::Touched(now) = check(&seen, t(2), || Some("a".into())) else { panic!() };
        assert_eq!(check(&now, t(2), || panic!("read")), Disk::Unchanged);
    }

    #[test]
    fn other_contents_are_a_change() {
        let seen = Seen::read(t(1), Some("a"));
        assert_eq!(check(&seen, t(2), || Some("b".into())), Disk::Changed);
    }

    #[test]
    fn after_the_host_writes_the_contents_decide() {
        // The time after our write is unknown, so the next check reads: our
        // own bytes are not a change, another writer's are.
        let seen = Seen::wrote("ours");
        assert!(matches!(check(&seen, t(5), || Some("ours".into())), Disk::Touched(_)));
        assert_eq!(check(&seen, t(5), || Some("theirs".into())), Disk::Changed);
    }

    #[test]
    fn a_file_appearing_is_a_change_and_one_going_away_is_not() {
        let none = Seen::read(t(1), None);
        assert_eq!(check(&none, t(3), || Some("new".into())), Disk::Changed);
        let seen = Seen::read(t(1), Some("a"));
        assert_eq!(check(&seen, None, || panic!("read")), Disk::Unchanged);
        assert_eq!(check(&seen, t(2), || None), Disk::Unchanged);
    }

    #[test]
    fn review_files_compare_raw_text() {
        assert!(!review_changed("a\n", Some("a\n")));
        assert!(review_changed("a\n", Some("a\r\n")));
        assert!(!review_changed("a\n", None));
    }

    // ---- replaying unsaved work ---------------------------------------------

    fn doc(lines: &[&str]) -> Document {
        let paragraphs = lines
            .iter()
            .enumerate()
            .map(|(i, l)| Paragraph::plain(ParaId(i as u64), ElementKind::Action, *l))
            .collect();
        Document::new(Script { paragraphs, ..Script::default() })
    }

    /// A decision as the review session makes one: the line's text and a key, one step.
    fn set(d: &mut Document, i: u64, text: &str, key: &str) {
        let sel = Selection::cursor(d.start());
        d.replace_text(sel, ParaId(i), text, &[(key, Some("x"))]).unwrap();
    }

    fn texts(d: &Document) -> Vec<String> {
        d.paragraphs().iter().map(|p| p.text()).collect()
    }

    /// The session file's view: the host (`app`) and another program
    /// (`disk`) both started from `shared`, which was last synced.
    fn fork(shared: &Document) -> (Document, Document, usize) {
        (shared.clone(), shared.clone(), shared.log().len())
    }

    fn replay(app: &Document, synced: usize, disk: Document) -> Replayed {
        with_unsaved(rebuild(app, synced).unwrap(), &app.log()[synced..], disk)
    }

    #[test]
    fn common_prefixes() {
        let (u, r) = (Event::Undo, Event::Redo);
        let host = [u.clone(), r.clone(), u.clone()];
        assert_eq!(common_prefix(&[], &host), 0);
        assert_eq!(common_prefix(&host[..2], &host), 2);
        assert_eq!(common_prefix(&host, &[u.clone(), u.clone()]), 1);
        // The other program appended to everything the host saved: all synced.
        assert_eq!(still_synced(2, &host, &[u.clone(), r.clone(), r.clone()]), 2);
        // It wrote over the host's last save: only the shared part is.
        assert_eq!(still_synced(3, &host, &[u.clone(), u]), 1);
    }

    #[test]
    fn saved_events_another_program_dropped_are_replayed_as_unsaved() {
        // The host saved decision A; a program that read the file before
        // that save then wrote its own decision B over it, without A.
        let shared = doc(&["one", "two"]);
        let mut app = shared.clone();
        set(&mut app, 0, "ONE", "a");
        let synced = app.log().len();
        let mut disk = shared.clone();
        set(&mut disk, 1, "TWO", "b");
        let synced = still_synced(synced, app.log(), disk.log());
        assert_eq!(synced, 0);
        let r = replay(&app, synced, disk);
        assert!(r.lost.is_empty());
        assert_eq!(texts(&r.doc), ["ONE", "TWO"], "A is back, B is kept");
        assert_eq!(r.doc.meta("a"), Some("x"));
        assert_eq!(r.doc.meta("b"), Some("x"));
        assert_eq!(r.kept, [Change::Para(ParaId(0), "ONE".into()), Change::Meta("a".into())]);
        // Trusting the stale count instead would leave A in neither list.
        let stale = replay(&app, app.log().len(), {
            let mut d = shared.clone();
            set(&mut d, 1, "TWO", "b");
            d
        });
        assert_eq!(texts(&stale.doc), ["one", "TWO"]);
        assert!(stale.kept.is_empty() && stale.lost.is_empty());
    }

    #[test]
    fn a_dropped_event_that_clashes_is_lost_not_misapplied() {
        let shared = doc(&["one"]);
        let mut app = shared.clone();
        set(&mut app, 0, "mine", "a");
        let mut disk = shared.clone();
        set(&mut disk, 0, "theirs", "b");
        let synced = still_synced(app.log().len(), app.log(), disk.log());
        let r = replay(&app, synced, disk);
        assert_eq!(texts(&r.doc), ["theirs"]);
        assert!(r.lost.contains(&Change::Para(ParaId(0), "mine".into())));
    }

    #[test]
    fn rebuild_gives_back_the_document_and_its_history() {
        let mut d = doc(&["one", "two"]);
        set(&mut d, 0, "ONE", "a");
        let at = d.log().len();
        set(&mut d, 1, "TWO", "b");
        let mut back = rebuild(&d, at).unwrap();
        assert_eq!(texts(&back), ["ONE", "two"]);
        back.undo();
        assert_eq!(texts(&back), ["one", "two"]);
    }

    #[test]
    fn unsaved_changes_are_kept_on_top_of_a_reload() {
        // The app decided line 0; meanwhile another program changed line 1.
        let (mut app, mut disk, synced) = fork(&doc(&["one", "two"]));
        set(&mut app, 0, "ONE", "a");
        set(&mut disk, 1, "TWO", "b");
        let r = replay(&app, synced, disk);
        assert!(r.lost.is_empty());
        assert_eq!(texts(&r.doc), ["ONE", "TWO"]);
        assert_eq!(r.doc.meta("a"), Some("x"));
        assert_eq!(r.doc.meta("b"), Some("x"));
        assert_eq!(r.kept, [Change::Para(ParaId(0), "ONE".into()), Change::Meta("a".into())]);
    }

    #[test]
    fn an_unsaved_undo_reverts_what_it_reverted_not_the_other_programs_step() {
        // Saved: the app decided line 0. Then another program decided line 1,
        // and the app (not yet knowing) pressed undo, undoing its own decision.
        let mut shared = doc(&["one", "two"]);
        set(&mut shared, 0, "ONE", "a");
        let (mut app, mut disk, synced) = fork(&shared);
        set(&mut disk, 1, "TWO", "b");
        app.undo();
        let mut r = replay(&app, synced, disk);
        assert!(r.lost.is_empty());
        assert_eq!(texts(&r.doc), ["one", "TWO"], "the app's decision is undone, theirs is kept");
        assert_eq!(r.doc.meta("a"), None);
        assert_eq!(r.doc.meta("b"), Some("x"));
        // One shared history: undo now takes back the app's undo, then theirs.
        r.doc.undo();
        assert_eq!(texts(&r.doc), ["ONE", "TWO"]);
        r.doc.undo();
        assert_eq!(texts(&r.doc), ["ONE", "two"]);
    }

    #[test]
    fn an_unsaved_redo_redoes_the_apps_step() {
        let mut shared = doc(&["one", "two"]);
        set(&mut shared, 0, "ONE", "a");
        shared.undo();
        let (mut app, mut disk, synced) = fork(&shared);
        set(&mut disk, 1, "TWO", "b");
        app.redo();
        let r = replay(&app, synced, disk);
        assert_eq!(texts(&r.doc), ["ONE", "TWO"]);
        assert_eq!(r.doc.meta("a"), Some("x"));
    }

    #[test]
    fn an_undo_whose_step_the_other_program_changed_is_lost_not_misapplied() {
        let mut shared = doc(&["one"]);
        set(&mut shared, 0, "ONE", "a");
        let (mut app, mut disk, synced) = fork(&shared);
        set(&mut disk, 0, "theirs", "b");
        app.undo();
        let r = replay(&app, synced, disk);
        assert_eq!(texts(&r.doc), ["theirs"]);
        assert!(r.lost.contains(&Change::Para(ParaId(0), "one".into())));
    }

    #[test]
    fn a_rewrite_merges_only_the_apps_steps() {
        // The app starts a rewrite (depth noted), types, and finishes it
        // (merge since that depth) while another program decided two lines.
        let (mut app, mut disk, synced) = fork(&doc(&["one", "two", "three"]));
        let depth = app.undo_depth();
        let sel = Selection::cursor(app.start());
        app.replace_text(sel, ParaId(0), "ON", &[]).unwrap();
        app.replace_text(sel, ParaId(0), "ONE!", &[("a", Some("x"))]).unwrap();
        app.merge_undo_since(depth);
        set(&mut disk, 1, "TWO", "b");
        set(&mut disk, 2, "THREE", "c");
        let mut r = replay(&app, synced, disk);
        assert_eq!(texts(&r.doc), ["ONE!", "TWO", "THREE"]);
        // One undo takes back the whole rewrite and nothing of theirs.
        r.doc.undo();
        assert_eq!(texts(&r.doc), ["one", "TWO", "THREE"]);
        r.doc.undo();
        assert_eq!(texts(&r.doc), ["one", "TWO", "three"]);
    }

    #[test]
    fn a_rewrite_saved_halfway_never_swallows_the_other_programs_steps() {
        // The rewrite starts and is autosaved; then another program decides a
        // line; then the rewrite finishes (its merge reaches below the save).
        let mut shared = doc(&["one", "two"]);
        let depth = shared.undo_depth();
        let sel = Selection::cursor(shared.start());
        shared.replace_text(sel, ParaId(0), "ON", &[]).unwrap();
        let (mut app, mut disk, synced) = fork(&shared);
        app.replace_text(sel, ParaId(0), "ONE!", &[("a", Some("x"))]).unwrap();
        app.merge_undo_since(depth);
        set(&mut disk, 1, "TWO", "b");
        let mut r = replay(&app, synced, disk);
        assert_eq!(texts(&r.doc), ["ONE!", "TWO"]);
        r.doc.undo();
        assert_eq!(texts(&r.doc), ["ON", "TWO"], "only the app's own unsaved step");
        r.doc.undo();
        assert_eq!(texts(&r.doc), ["ON", "two"]);
    }

    #[test]
    fn typing_never_lands_in_a_line_another_program_changed() {
        let (mut app, mut disk, synced) = fork(&doc(&["I am go home", "other"]));
        let sel = Selection::cursor(scriptorium_core::Pos::new(ParaId(0), 5));
        app.insert_text(sel, "not ").unwrap();
        set(&mut disk, 0, "I am going home", "b");
        let r = replay(&app, synced, disk);
        assert_eq!(texts(&r.doc), ["I am going home", "other"]);
        assert_eq!(r.lost, [Change::Para(ParaId(0), "I am not go home".into())]);
    }

    #[test]
    fn every_event_that_still_fits_is_kept() {
        // Three unsaved edits: one clashes, the others don't.
        let (mut app, mut disk, synced) = fork(&doc(&["one", "two", "three"]));
        set(&mut app, 0, "ONE", "a");
        set(&mut app, 1, "mine", "b");
        set(&mut app, 2, "THREE", "c");
        set(&mut app, 1, "mine again", "b2");
        set(&mut disk, 1, "theirs", "z");
        let r = replay(&app, synced, disk);
        assert_eq!(texts(&r.doc), ["ONE", "theirs", "THREE"]);
        assert_eq!(r.doc.meta("a"), Some("x"));
        assert_eq!(r.doc.meta("c"), Some("x"));
        assert_eq!(r.doc.meta("b"), None);
        assert!(r.lost.contains(&Change::Para(ParaId(1), "mine again".into())));
        assert!(r.lost.contains(&Change::Meta("b".into())));
        assert!(!r.kept.iter().any(|c| matches!(c, Change::Para(ParaId(1), _))));
    }

    #[test]
    fn a_change_made_and_undone_is_neither_kept_nor_lost() {
        let (mut app, mut disk, synced) = fork(&doc(&["one"]));
        set(&mut app, 0, "ONE", "a");
        app.undo();
        set(&mut disk, 0, "theirs", "b");
        let r = replay(&app, synced, disk);
        assert_eq!(texts(&r.doc), ["theirs"]);
        assert!(r.lost.is_empty() && r.kept.is_empty());
    }

    #[test]
    fn undoing_a_paragraph_merge_puts_the_paragraph_back() {
        let (mut app, disk, synced) = fork(&doc(&["one", "two"]));
        // Backspace at the start of line 1 merges it into line 0 (saved), then undo.
        let sel = Selection::cursor(scriptorium_core::Pos::new(ParaId(1), 0));
        app.delete_backward(sel, false).unwrap();
        assert_eq!(texts(&app), ["onetwo"]);
        let synced_after = app.log().len();
        let disk = disk.replay_onto(&app.log()[synced..]).unwrap();
        app.undo();
        let r = replay(&app, synced_after, disk);
        assert_eq!(texts(&r.doc), ["one", "two"]);
        assert!(r.lost.is_empty());
    }

    #[test]
    fn whole_paragraph_ops_round_trip() {
        let mut a = doc(&["", "x"]);
        let b = a.clone();
        set(&mut a, 0, "filled", "k");
        let ops = ops_between(&b, &a).unwrap();
        let t = Transaction {
            ops,
            before: Selection::cursor(b.start()),
            after: Selection::cursor(b.start()),
            group: Group::Other,
        };
        let back = b.replay_onto(&[Event::Edit(t)]).unwrap();
        assert_eq!(texts(&back), texts(&a));
        assert_eq!(back.meta("k"), Some("x"));
    }
}
