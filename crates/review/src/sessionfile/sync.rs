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
use scriptorium_core::{Document, ParaId, Paragraph, Pos, Selection};

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
/// last synced with the session file. Paragraph ids are as the replayed
/// document has them (see [`Renumber`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The paragraph, and its text as the host has it ("" for a line the
    /// host emptied or added empty).
    Para(ParaId, String),
    /// A paragraph the host removed, with its text before that.
    Removed(ParaId, String),
    /// A metadata key (`review/<id>/<entry>` for a decision).
    Meta(String),
}

/// How the host's paragraph ids map onto the reloaded document's.
///
/// A fresh id the host gave out since it last synced (one at or above the
/// synced document's `next_id`) may have been given out by the other program
/// too, for another line, even if that line is gone again: fresh ids clash
/// whenever the reloaded document's `next_id` moved. So, when it moved, every
/// host-fresh id moves up by the same amount, onto ids the reloaded document
/// has never used. Ids below the synced `next_id` (the lines both programs
/// shared, including any an undo or `insert_para_with_id` brought back) keep
/// their meaning and never move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Renumber {
    /// The synced document's `next_id`: host ids from here on are fresh.
    from: u64,
    /// How far fresh ids move (0: the other program added no ids).
    by: u64,
}

impl Renumber {
    /// `synced_next`: the synced document's `next_id`; `reloaded_next`: the
    /// reloaded document's.
    pub fn new(synced_next: u64, reloaded_next: u64) -> Self {
        Self { from: synced_next, by: reloaded_next.saturating_sub(synced_next) }
    }

    /// Does any id move?
    pub fn is_identity(&self) -> bool {
        self.by == 0
    }

    /// The reloaded document's id for the host's paragraph `id`.
    pub fn id(&self, id: ParaId) -> ParaId {
        if id.0 >= self.from { ParaId(id.0 + self.by) } else { id }
    }

    pub fn pos(&self, pos: Pos) -> Pos {
        Pos::new(self.id(pos.para), pos.offset)
    }

    /// The host's cursor or selection, on the reloaded document.
    pub fn selection(&self, sel: Selection) -> Selection {
        Selection { anchor: self.pos(sel.anchor), head: self.pos(sel.head) }
    }

    fn para(&self, p: &Paragraph) -> Paragraph {
        Paragraph { id: self.id(p.id), ..p.clone() }
    }

    /// A metadata value with the paragraph ids it names renumbered. Only
    /// review decisions (keys under `review/`) name paragraphs, in the
    /// grammar of [`crate::meta`]. A value that doesn't parse as that is
    /// left as it is.
    pub fn meta_value(&self, key: &str, value: &str) -> String {
        if self.is_identity() || !key.starts_with("review/") {
            return value.to_string();
        }
        crate::meta::renumber(value, |p| self.id(p)).unwrap_or_else(|| value.to_string())
    }

    /// `op` with every paragraph id (and decision value) renumbered; indices
    /// are left for [`translate`] to recompute.
    fn op(&self, op: &Op) -> Op {
        match op.clone() {
            Op::Splice { para, at, removed, inserted } => Op::Splice { para: self.id(para), at, removed, inserted },
            Op::Split { para, at, new_para, new_kind } => {
                Op::Split { para: self.id(para), at, new_para: self.id(new_para), new_kind }
            }
            Op::Merge { para, at, next, next_kind } => {
                Op::Merge { para: self.id(para), at, next: self.id(next), next_kind }
            }
            Op::SetKind { para, from, to } => Op::SetKind { para: self.id(para), from, to },
            Op::InsertPara { index, para } => Op::InsertPara { index, para: self.para(&para) },
            Op::RemovePara { index, para } => Op::RemovePara { index, para: self.para(&para) },
            Op::SetMeta { key, from, to } => {
                let v = |v: Option<String>| v.map(|v| self.meta_value(&key, &v));
                Op::SetMeta { from: v(from), to: v(to), key }
            }
        }
    }
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
    /// The host's ids as `doc` has them: map the cursor, and anything else
    /// the host holds by paragraph id, through this.
    pub renumber: Renumber,
}

/// Replay the host's `unsaved` events on top of `reloaded` (the document
/// resumed from the session file another program wrote). `synced` is the
/// host's document as the session file last held it, with its undo history
/// ([`rebuild`] at the log length last read or written).
///
/// Paragraphs the host added since it synced are renumbered if the other
/// program added any meanwhile (see [`Renumber`]), so both programs' new
/// lines are kept.
///
/// Event by event, so every one that still fits is kept:
/// - An edit applies only if every paragraph (and decision) it touches is,
///   in `reloaded`, exactly as the host had it just before the edit, so
///   typing never lands at an offset in text another program changed.
///   Edits that depend on a dropped one are dropped too. A paragraph added
///   goes between the same two neighbours as in the host's document,
///   wherever they are in `reloaded`, and only if they are still neighbours
///   there; if only one of them is still there (at an end of the document,
///   or the other program removed the other), that one decides.
/// - An undo or redo is replayed as the concrete change it made to the
///   host's document (so it reverts exactly what it reverted then, never
///   the other program's newest step), recorded as a new edit.
/// - A merge of undo steps (a finished rewrite) merges only the host's own
///   steps replayed here, never steps another program added meanwhile.
pub fn with_unsaved(synced: Document, unsaved: &[Event], reloaded: Document) -> Replayed {
    let start = synced.clone();
    let map = Renumber::new(synced.next_id(), reloaded.next_id());
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
                regular = translate(&sim, &target, t, map).is_some_and(|t| try_apply(&mut target, Event::Edit(t)));
                sim = next;
            }
            Event::Undo | Event::Redo => {
                let mut next = sim.clone();
                let done = if matches!(e, Event::Redo) { next.redo() } else { next.undo() };
                if let Some(sel) = done {
                    regular = false;
                    if let Some(ops) = ops_between(&sim, &next).filter(|o| !o.is_empty()) {
                        let t = Transaction { ops, before: sel, after: sel, group: Group::Other };
                        if let Some(t) = translate(&sim, &target, &t, map) {
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
    let (kept, lost) = net_changes(&start, &sim, &target, map);
    Replayed { doc: target, kept, lost, renumber: map }
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

/// `t`, the host's edit on `sim` (the host's document just before it), as
/// an edit of `target`: ids renumbered through `map`, paragraphs added or
/// removed placed by their neighbours in `target`. None unless everything
/// `t` touches is the same in `target` as in `sim`, op by op (each op sees
/// what the ones before it did).
fn translate(sim: &Document, target: &Document, t: &Transaction, map: Renumber) -> Option<Transaction> {
    // Working copies without history: cheap to step one op at a time.
    let bare = |d: &Document| Document::with_meta(d.script().clone(), d.all_meta().clone());
    let (mut s, mut g) = (bare(sim), bare(target));
    let mut ops = Vec::with_capacity(t.ops.len());
    for op in &t.ops {
        let theirs = translate_op(&s, &g, op, map)?;
        let step = |o: Op| Event::Edit(Transaction { ops: vec![o], ..t.clone() });
        s = s.replay_onto(&[step(op.clone())]).ok()?;
        g = g.replay_onto(&[step(theirs.clone())]).ok()?;
        ops.push(theirs);
    }
    Some(Transaction { ops, before: map.selection(t.before), after: map.selection(t.after), group: t.group })
}

/// One op of [`translate`]: `op` applies to `s` (the host's side); the op
/// for `g` (the target), if everything it touches is the same there.
fn translate_op(s: &Document, g: &Document, op: &Op, map: Renumber) -> Option<Op> {
    let same = |id: ParaId| {
        g.paragraph(map.id(id)).is_some_and(|theirs| s.paragraph(id).is_some_and(|mine| map.para(mine) == *theirs))
    };
    let fits = match op {
        Op::Splice { para, .. } | Op::Split { para, .. } | Op::SetKind { para, .. } => same(*para),
        Op::Merge { para, next, .. } => same(*para) && same(*next),
        Op::InsertPara { index, para } => {
            // Between the same two neighbours, wherever they are now. When
            // only one of them is still there (the other is at an end of the
            // document, or another program removed it) that one decides: the
            // other program may have added lines at that end, or the line may
            // have stood next to the one it removed. With both gone there is
            // nothing to place it by, and it's lost.
            let id_at = |k: usize| s.paragraphs().get(k).map(|p| map.id(p.id));
            let (before, after) = (index.checked_sub(1).and_then(id_at), id_at(*index));
            let (b, a) = (before.and_then(|b| g.index_of(b)), after.and_then(|a| g.index_of(a)));
            let at = match (b, a) {
                (Some(b), Some(a)) => (a == b + 1).then_some(a)?,
                (Some(b), None) => b + 1,
                (None, Some(a)) => a,
                (None, None) => return None,
            };
            return Some(Op::InsertPara { index: at, para: map.para(para) });
        }
        Op::RemovePara { para, .. } => {
            let at = g.index_of(map.id(para.id)).filter(|_| same(para.id))?;
            return Some(Op::RemovePara { index: at, para: map.para(para) });
        }
        Op::SetMeta { key, .. } => s.meta(key).map(|v| map.meta_value(key, v)).as_deref() == g.meta(key),
    };
    fits.then(|| map.op(op))
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
/// ended up with it (compared through `map`).
fn net_changes(start: &Document, host: &Document, target: &Document, map: Renumber) -> (Vec<Change>, Vec<Change>) {
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
        let theirs = map.id(id);
        let change = match (mine, s.get(&id)) {
            (Some(p), _) => Change::Para(theirs, p.text()),
            (None, was) => Change::Removed(theirs, was.map(Paragraph::text).unwrap_or_default()),
        };
        if t.get(&theirs) == mine.map(|p| map.para(p)).as_ref() { kept.push(change) } else { lost.push(change) }
    }
    let keys: BTreeSet<&String> = start.all_meta().keys().chain(host.all_meta().keys()).collect();
    for key in keys {
        let mine = host.meta(key);
        if start.meta(key) == mine {
            continue;
        }
        let change = Change::Meta(key.clone());
        if target.meta(key) == mine.map(|v| map.meta_value(key, v)).as_deref() {
            kept.push(change)
        } else {
            lost.push(change)
        }
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

    // ---- renumbering the host's new paragraphs ------------------------------

    use scriptorium_core::Pos;
    use scriptorium_core::doc::NewPara;

    fn ids(d: &Document) -> Vec<u64> {
        d.paragraphs().iter().map(|p| p.id.0).collect()
    }

    /// As `scriptorium insert --end`: a new last line, with a fresh id.
    fn append(d: &mut Document, text: &str) -> ParaId {
        let sel = Selection::cursor(d.start());
        let n = d.paragraphs().len();
        d.edit(sel, Group::Other, |e| Ok((sel, e.insert_para(n, NewPara::plain(ElementKind::Action, text))?))).unwrap()
    }

    fn enter(d: &mut Document, para: u64, at: usize) -> Selection {
        d.enter(Selection::cursor(Pos::new(ParaId(para), at))).unwrap()
    }

    #[test]
    fn renumber_moves_only_fresh_ids() {
        let m = Renumber::new(3, 5);
        assert!(!m.is_identity());
        assert_eq!(m.id(ParaId(2)), ParaId(2));
        assert_eq!(m.id(ParaId(3)), ParaId(5));
        assert_eq!(m.id(ParaId(4)), ParaId(6));
        let sel = Selection { anchor: Pos::new(ParaId(0), 1), head: Pos::new(ParaId(4), 2) };
        assert_eq!(m.selection(sel), Selection { anchor: Pos::new(ParaId(0), 1), head: Pos::new(ParaId(6), 2) });
        assert!(Renumber::new(3, 3).is_identity());
        // The other program's document is never behind the synced one; if it
        // were, nothing moves.
        assert!(Renumber::new(5, 3).is_identity());
    }

    #[test]
    fn decision_values_are_renumbered() {
        let m = Renumber::new(10, 20);
        let v = |s: &str| m.meta_value("review/abc/3", s);
        assert_eq!(v("accepted:0@7"), "accepted:0@7");
        assert_eq!(v("accepted:0@12"), "accepted:0@22");
        assert_eq!(v("rejected@9,10,11"), "rejected@9,20,21");
        assert_eq!(v("accepted:0@4,5=>4,10"), "accepted:0@4,5=>4,20");
        assert_eq!(v("accepted:1@11,12=>after:10"), "accepted:1@21,22=>after:20");
        assert_eq!(v("accepted:1@11=>start"), "accepted:1@21=>start");
        assert_eq!(v("rewritten"), "rewritten", "old values name no paragraph");
        assert_eq!(v("odd@x,12"), "odd@x,12", "unparsable values are left alone");
        assert_eq!(m.meta_value("other/key", "a@12"), "a@12", "only decisions name paragraphs");
        assert_eq!(Renumber::new(10, 10).meta_value("review/a/1", "rejected@12"), "rejected@12");
    }

    #[test]
    fn a_split_keeps_its_line_when_the_cli_took_the_same_id() {
        // The app presses Enter at the end of "one" (new id 2); meanwhile the
        // CLI appends "x", which also got id 2.
        let (mut app, mut disk, synced) = fork(&doc(&["one", "two"]));
        let cursor = enter(&mut app, 0, 3);
        assert_eq!(cursor.head.para, ParaId(2));
        assert_eq!(append(&mut disk, "x"), ParaId(2));
        let r = replay(&app, synced, disk);
        assert!(r.lost.is_empty(), "{:?}", r.lost);
        assert_eq!(texts(&r.doc), ["one", "", "two", "x"]);
        assert_eq!(ids(&r.doc), [0, 3, 1, 2], "the app's new line moved to id 3");
        assert_eq!(r.renumber.selection(cursor).head, Pos::new(ParaId(3), 0));
        assert_eq!(r.kept, [Change::Para(ParaId(3), String::new())]);
        // Typing into the new line after the split is replayed onto id 3 too.
        let (mut app, mut disk, synced) = fork(&doc(&["one", "two"]));
        let c = enter(&mut app, 0, 3);
        app.insert_text(c, "new").unwrap();
        append(&mut disk, "x");
        let r = replay(&app, synced, disk);
        assert_eq!(texts(&r.doc), ["one", "new", "two", "x"]);
        assert_eq!(r.doc.paragraph(ParaId(3)).unwrap().text(), "new");
    }

    #[test]
    fn an_insert_is_placed_by_its_neighbours_after_renumbering() {
        // The app inserts a line between 0 and 1; the CLI inserted two lines
        // at the top, so every index moved, and took ids 2 and 3.
        let (mut app, mut disk, synced) = fork(&doc(&["one", "two"]));
        let sel = Selection::cursor(app.start());
        app.edit(sel, Group::Other, |e| Ok((sel, e.insert_para(1, NewPara::plain(ElementKind::Action, "mine"))?)))
            .unwrap();
        for t in ["a", "b"] {
            let s = Selection::cursor(disk.start());
            disk.edit(s, Group::Other, |e| Ok((s, e.insert_para(0, NewPara::plain(ElementKind::Action, t))?))).unwrap();
        }
        let mut r = replay(&app, synced, disk);
        assert!(r.lost.is_empty());
        assert_eq!(texts(&r.doc), ["b", "a", "one", "mine", "two"]);
        assert_eq!(r.doc.paragraph(ParaId(4)).unwrap().text(), "mine");
        // Undo after the replay takes back the app's insert, then theirs.
        r.doc.undo();
        assert_eq!(texts(&r.doc), ["b", "a", "one", "two"]);
        r.doc.redo();
        assert_eq!(texts(&r.doc), ["b", "a", "one", "mine", "two"]);
    }

    #[test]
    fn an_insert_whose_neighbours_moved_apart_is_lost_not_misplaced() {
        // The app inserts between 0 and 1; the CLI put a line there first.
        let (mut app, mut disk, synced) = fork(&doc(&["one", "two"]));
        let sel = Selection::cursor(app.start());
        app.edit(sel, Group::Other, |e| Ok((sel, e.insert_para(1, NewPara::plain(ElementKind::Action, "mine"))?)))
            .unwrap();
        disk.edit(sel, Group::Other, |e| Ok((sel, e.insert_para(1, NewPara::plain(ElementKind::Action, "theirs"))?)))
            .unwrap();
        let r = replay(&app, synced, disk);
        assert_eq!(texts(&r.doc), ["one", "theirs", "two"]);
        assert_eq!(r.lost, [Change::Para(ParaId(3), "mine".into())]);
    }

    #[test]
    fn an_insert_next_to_a_line_the_cli_removed_goes_by_the_other_neighbour() {
        // The app presses Enter at the start of "one" (a new line above it)
        // and types into it; the CLI removed "zero", the line above.
        let (mut app, mut disk, synced) = fork(&doc(&["zero", "one", "two"]));
        enter(&mut app, 1, 0);
        app.insert_text(Selection::cursor(Pos::new(ParaId(3), 0)), "NEW").unwrap();
        assert_eq!(texts(&app), ["zero", "NEW", "one", "two"]);
        let s = Selection::cursor(disk.start());
        disk.edit(s, Group::Other, |e| Ok((s, e.remove_para(ParaId(0))?))).unwrap();
        let r = replay(&app, synced, disk);
        assert!(r.lost.is_empty(), "{:?}", r.lost);
        assert_eq!(texts(&r.doc), ["NEW", "one", "two"]);
        // The same with the line below removed: placed after "zero".
        let (mut app, mut disk, synced) = fork(&doc(&["zero", "one", "two"]));
        enter(&mut app, 1, 0);
        app.insert_text(Selection::cursor(Pos::new(ParaId(3), 0)), "NEW").unwrap();
        disk.edit(s, Group::Other, |e| Ok((s, e.remove_para(ParaId(1))?))).unwrap();
        let r = replay(&app, synced, disk);
        assert!(r.lost.is_empty(), "{:?}", r.lost);
        assert_eq!(texts(&r.doc), ["zero", "NEW", "two"]);
    }

    #[test]
    fn an_insert_whose_neighbours_are_both_gone_is_lost() {
        let (mut app, mut disk, synced) = fork(&doc(&["zero", "one", "two"]));
        enter(&mut app, 1, 0);
        app.insert_text(Selection::cursor(Pos::new(ParaId(3), 0)), "NEW").unwrap();
        let s = Selection::cursor(disk.start());
        for id in [0, 1] {
            disk.edit(s, Group::Other, |e| Ok((s, e.remove_para(ParaId(id))?))).unwrap();
        }
        let r = replay(&app, synced, disk);
        assert_eq!(texts(&r.doc), ["two"]);
        assert_eq!(r.lost, [Change::Para(ParaId(3), "NEW".into())]);
        // At an end of the document, its one neighbour gone: lost too.
        let (mut app, mut disk, synced) = fork(&doc(&["zero", "one"]));
        enter(&mut app, 0, 0);
        app.insert_text(Selection::cursor(Pos::new(ParaId(2), 0)), "NEW").unwrap();
        disk.edit(s, Group::Other, |e| Ok((s, e.remove_para(ParaId(0))?))).unwrap();
        let r = replay(&app, synced, disk);
        assert_eq!(texts(&r.doc), ["one"]);
        assert_eq!(r.lost, [Change::Para(ParaId(2), "NEW".into())]);
    }

    #[test]
    fn a_merge_of_the_apps_new_line_follows_the_renumbering() {
        // The app splits "onetwo" (new id 2), then joins the halves back with
        // an edit in between; the CLI appended a line (id 2) meanwhile.
        let (mut app, mut disk, synced) = fork(&doc(&["onetwo", "end"]));
        let c = enter(&mut app, 0, 3);
        app.insert_text(c, "+").unwrap();
        app.delete_backward(Selection::cursor(Pos::new(ParaId(2), 0)), false).unwrap();
        assert_eq!(texts(&app), ["one+two", "end"]);
        append(&mut disk, "x");
        let r = replay(&app, synced, disk);
        assert!(r.lost.is_empty(), "{:?}", r.lost);
        assert_eq!(texts(&r.doc), ["one+two", "end", "x"]);
        assert_eq!(ids(&r.doc), [0, 1, 2]);
    }

    #[test]
    fn undo_after_replay_takes_back_the_renumbered_split() {
        let (mut app, mut disk, synced) = fork(&doc(&["one", "two"]));
        enter(&mut app, 0, 1);
        append(&mut disk, "x");
        let mut r = replay(&app, synced, disk);
        assert_eq!(texts(&r.doc), ["o", "ne", "two", "x"]);
        r.doc.undo();
        assert_eq!(texts(&r.doc), ["one", "two", "x"]);
        r.doc.undo();
        assert_eq!(texts(&r.doc), ["one", "two"]);
        r.doc.redo();
        r.doc.redo();
        assert_eq!(ids(&r.doc), [0, 3, 1, 2]);
    }

    #[test]
    fn an_unsaved_undo_and_redo_of_a_split_are_renumbered() {
        // The app splits, undoes and redoes it (redo brings back its fresh
        // id, which is still the app's own); the CLI appended meanwhile.
        let (mut app, mut disk, synced) = fork(&doc(&["one", "two"]));
        enter(&mut app, 1, 1);
        app.undo();
        app.redo();
        append(&mut disk, "x");
        let r = replay(&app, synced, disk);
        assert!(r.lost.is_empty(), "{:?}", r.lost);
        assert_eq!(texts(&r.doc), ["one", "t", "wo", "x"]);
        assert_eq!(ids(&r.doc), [0, 1, 3, 2]);
    }

    #[test]
    fn an_id_the_other_program_used_and_removed_is_not_reused() {
        // The CLI added id 2 and removed it again: the app's own id 2 must
        // still move, or the session would give one id two meanings.
        let (mut app, mut disk, synced) = fork(&doc(&["one", "two"]));
        enter(&mut app, 0, 3);
        let id = append(&mut disk, "x");
        let s = Selection::cursor(disk.start());
        disk.edit(s, Group::Other, |e| Ok((s, e.remove_para(id)?))).unwrap();
        let r = replay(&app, synced, disk);
        assert_eq!(ids(&r.doc), [0, 3, 1]);
        assert!(r.lost.is_empty());
    }

    #[test]
    fn decisions_on_the_apps_new_lines_are_renumbered() {
        let (mut app, mut disk, synced) = fork(&doc(&["one", "two"]));
        enter(&mut app, 0, 3);
        let sel = Selection::cursor(app.start());
        app.edit(sel, Group::Other, |e| Ok((sel, e.set_meta("review/r/0", Some("accepted:0@0=>0,2"))?))).unwrap();
        app.edit(sel, Group::Other, |e| Ok((sel, e.set_meta("review/r/0", Some("rejected@0,2"))?))).unwrap();
        append(&mut disk, "x");
        let r = replay(&app, synced, disk);
        assert!(r.lost.is_empty(), "{:?}", r.lost);
        assert_eq!(r.doc.meta("review/r/0"), Some("rejected@0,3"), "SetMeta `from` compared through the map");
        assert!(r.kept.contains(&Change::Meta("review/r/0".into())));
    }

    #[test]
    fn a_removed_line_that_clashes_is_named() {
        let (mut app, mut disk, synced) = fork(&doc(&["one", "two"]));
        let s = Selection::cursor(app.start());
        app.edit(s, Group::Other, |e| Ok((s, e.remove_para(ParaId(1))?))).unwrap();
        set(&mut disk, 1, "TWO", "b");
        let r = replay(&app, synced, disk);
        assert_eq!(texts(&r.doc), ["one", "TWO"]);
        assert_eq!(r.lost, [Change::Removed(ParaId(1), "two".into())]);
    }
}
