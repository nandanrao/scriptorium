//! Replacing and restoring a run of paragraphs: what accepting a block
//! suggestion does, and what keep mine / undecide after an accept undo.
//!
//! Both are built from the [`Edit`] primitives, inserting before removing, so
//! a run that is the whole document can be replaced (a document is never
//! empty; see [`DocError::LastParagraph`]).

use std::collections::HashSet;

use super::{DocError, Document, Edit, NewPara, Op, common_ends};
use crate::diff::{self, Tag};
use crate::model::{ParaId, Script, Span, Style};
use crate::spans;
use crate::text::{FUZZY_THRESHOLD, normalize, similarity};

/// Where a block suggestion applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Run {
    /// These paragraphs, in document order.
    Ids(Vec<ParaId>),
    /// Nothing: the place where a cut removed lines, just after `after`
    /// (`None` = the start of the document).
    Empty { after: Option<ParaId> },
}

/// What [`Edit::replace_run`] did: the run's ids afterwards, and where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replaced {
    /// The run's ids afterwards, in order (empty for a cut).
    pub ids: Vec<ParaId>,
    /// The live paragraph just before the run (`None` = the start of the
    /// document). For a cut this is where the lines were: store it as the
    /// cut's anchor and pass it back as [`Run::Empty`].
    pub after: Option<ParaId>,
}

impl Edit<'_> {
    /// Replace `run` with `new` (empty `new` = cut).
    ///
    /// The run is located as in [`Edit::restore_run`]: for [`Run::Ids`], the
    /// live paragraphs from the first to the last live id (lines added between
    /// them included, ids since removed skipped), or, if none is live, the
    /// place just after the nearest live paragraph before the first id in
    /// [`Document::history_order`]; for [`Run::Empty`], just after the anchor
    /// (a gone anchor falls back to its nearest live predecessor in the
    /// history order, so an anchor added since the starting point and removed
    /// again still has a place). An empty [`Run::Ids`], or an anchor the
    /// document never had, is `Mismatch`.
    ///
    /// Old and new lines are paired: lines equal by kind and normalized text
    /// ([`diff::opcodes`] Equal), and inside a changed chunk, same-kind lines
    /// in order whose [`similarity`] is at least [`FUZZY_THRESHOLD`]. A fixed
    /// (dual dialogue) old line still unpaired is paired by position with the
    /// same-kind new line at the same offset between its paired neighbours,
    /// so its text can change. A paired line keeps its id and only the changed
    /// middle of its text is spliced, so styles on the unchanged start and end
    /// survive; the inserted middle takes the new line's styles if the new
    /// line has any styled span, else the style of the text it replaces.
    /// Every other new line is inserted with a fresh id, and every other old
    /// line is removed (after the inserts). Refused ([`DocError::Fixed`]) if a
    /// fixed paragraph would be removed or a line inserted inside its group.
    pub fn replace_run(&mut self, run: Run, new: &[NewPara]) -> Result<Replaced, DocError> {
        let (current, base) = self.place(&run, None)?;
        let after = self.id_before(base);
        if current.is_empty() {
            return Ok(Replaced { ids: self.insert_all(base, new)?, after });
        }
        let old: Vec<_> = current.iter().map(|id| self.doc.paragraph(*id).cloned().expect("live span")).collect();
        let key = |kind: &crate::model::ElementKind, spans: &[Span]| (kind.clone(), normalize(&spans::text(spans)));
        let old_keys: Vec<_> = old.iter().map(|p| key(&p.kind, &p.spans)).collect();
        let new_keys: Vec<_> = new.iter().map(|p| key(&p.kind, &p.spans)).collect();

        // pair[j] = the old line new line j keeps the id of. Pairs are
        // increasing in both indices.
        let mut pair: Vec<Option<usize>> = vec![None; new.len()];
        for op in diff::opcodes(&old_keys, &new_keys) {
            match op.tag {
                Tag::Equal => (op.i1..op.i2).zip(op.j1..op.j2).for_each(|(i, j)| pair[j] = Some(i)),
                Tag::Replace => {
                    let mut next = op.j1;
                    for (i, o) in old.iter().enumerate().take(op.i2).skip(op.i1) {
                        let text = o.text();
                        let found = (next..op.j2).find(|&j| {
                            new[j].kind == o.kind && similarity(&text, &spans::text(&new[j].spans)) >= FUZZY_THRESHOLD
                        });
                        if let Some(j) = found {
                            pair[j] = Some(i);
                            next = j + 1;
                        }
                    }
                }
                Tag::Insert | Tag::Delete => {}
            }
        }
        self.pair_fixed_by_position(&old, new, &mut pair);

        let mut ids = Vec::with_capacity(new.len());
        let mut at = base;
        for (j, p) in new.iter().enumerate() {
            match pair[j] {
                Some(i) => {
                    at = self.index(old[i].id)? + 1;
                    ids.push(old[i].id);
                }
                None => {
                    ids.push(self.insert_para(at, p.clone())?);
                    at += 1;
                }
            }
        }
        for (j, p) in new.iter().enumerate() {
            if let Some(i) = pair[j] {
                self.splice_to(old[i].id, &p.spans)?;
            }
        }
        let kept: HashSet<usize> = pair.iter().flatten().copied().collect();
        for (i, p) in old.iter().enumerate() {
            if !kept.contains(&i) {
                self.remove_para(p.id)?;
            }
        }
        Ok(Replaced { ids, after })
    }

    /// Put back `before` (paragraphs with their own ids, in order) in place of
    /// `run`, by position, with no pairing: afterwards the run reads as
    /// `before` (same ids, kinds and text).
    ///
    /// The run is the live paragraphs from the first to the last live id of a
    /// [`Run::Ids`] (lines added between them included; ids since removed are
    /// skipped). For a [`Run::Empty`] (a cut), `before` goes back just after
    /// the anchor; if none of a [`Run::Ids`] is live, just after the nearest
    /// live paragraph before the first `before` line (else the first run id)
    /// in [`Document::history_order`]. An anchor that is gone falls back the
    /// same way: to the nearest live paragraph before it in the history
    /// order, else the start of the document. An anchor the document never
    /// had, or a run with no live id and no line the document ever had, is
    /// `Mismatch`.
    ///
    /// A `before` id live in the run gets its kind set and its text spliced in
    /// place, only the changed middle (so styles on the unchanged ends
    /// survive; the middle takes `before`'s styles if it has any styled span,
    /// else the style of the text it replaces). A missing one is inserted with
    /// its own id and spans ([`Edit::insert_para_with_id`], the one case an id
    /// comes back); run ids not in `before` are removed, after the inserts. A
    /// `before` id live outside the run is `Mismatch`.
    pub fn restore_run(&mut self, run: Run, before: &[(ParaId, NewPara)]) -> Result<(), DocError> {
        let (current, mut at) = self.place(&run, before.first().map(|(id, _)| *id))?;
        let in_run: HashSet<ParaId> = current.iter().copied().collect();
        for (id, _) in before {
            if self.doc.paragraph(*id).is_some() && !in_run.contains(id) {
                return Err(DocError::Mismatch("restore run: a line to restore is live outside the run"));
            }
        }
        for (id, p) in before {
            if in_run.contains(id) {
                let i = self.index(*id)?;
                if i < at {
                    return Err(DocError::Mismatch("restore run: lines out of order"));
                }
                at = i + 1;
            } else {
                self.insert_para_with_id(at, *id, p.clone())?;
                at += 1;
            }
        }
        for (id, p) in before {
            if in_run.contains(id) {
                self.splice_to(*id, &p.spans)?;
                self.set_kind(*id, p.kind.clone())?;
            }
        }
        let keep: HashSet<ParaId> = before.iter().map(|(id, _)| *id).collect();
        for id in current {
            if !keep.contains(&id) {
                self.remove_para(id)?;
            }
        }
        Ok(())
    }

    // ---- helpers -----------------------------------------------------------

    fn insert_all(&mut self, mut at: usize, new: &[NewPara]) -> Result<Vec<ParaId>, DocError> {
        let mut out = Vec::with_capacity(new.len());
        for p in new {
            out.push(self.insert_para(at, p.clone())?);
            at += 1;
        }
        Ok(out)
    }

    /// Where `run` is now: its live paragraphs (first to last live id) and
    /// the index of the first, or no paragraphs and the index to insert at.
    /// With no run id live, the place is where `hint` (else the first run id)
    /// was ([`Self::after_in`]).
    fn place(&self, run: &Run, hint: Option<ParaId>) -> Result<(Vec<ParaId>, usize), DocError> {
        let ids = match run {
            Run::Empty { after } => return Ok((vec![], self.after_anchor(*after)?)),
            Run::Ids(ids) => ids,
        };
        let mut live: Vec<usize> = ids.iter().filter_map(|id| self.doc.index_of(*id)).collect();
        live.sort_unstable();
        if let (Some(&a), Some(&b)) = (live.first(), live.last()) {
            let current = self.doc.script.paragraphs[a..=b].iter().map(|p| p.id).collect();
            return Ok((current, a));
        }
        // Where the lines were: just after the live line now before the first
        // line (to restore, else of the run) in the document's history.
        let order = self.history_order();
        let at = hint
            .into_iter()
            .chain(ids.first().copied())
            .find_map(|id| self.after_in(&order, id))
            .ok_or(DocError::Mismatch("run: no line is live, and the document never had one"))?;
        Ok((vec![], at))
    }

    /// The id of the paragraph just before index `at` (`None` at the start).
    fn id_before(&self, at: usize) -> Option<ParaId> {
        at.checked_sub(1).map(|i| self.doc.script.paragraphs[i].id)
    }

    /// [`Document::history_order`] including the ops of this transaction so far.
    fn history_order(&self) -> Vec<ParaId> {
        let mut h = History::new(&self.doc.initial);
        self.doc.for_each_applied_op(|op| h.apply(op));
        self.ops.iter().for_each(|op| h.apply(op));
        h.order
    }

    /// The index just after `id` if it is live; if it is gone, just after the
    /// nearest live paragraph before it in `order` (see
    /// [`Document::history_order`]), else 0. `None` if `order` doesn't hold
    /// it: there is no telling where it was.
    fn after_in(&self, order: &[ParaId], id: ParaId) -> Option<usize> {
        if let Some(i) = self.doc.index_of(id) {
            return Some(i + 1);
        }
        let k = order.iter().position(|&p| p == id)?;
        Some(order[..k].iter().rev().find_map(|p| self.doc.index_of(*p)).map_or(0, |i| i + 1))
    }

    /// The index just after `anchor` (`None` = 0); a gone anchor falls back
    /// as [`Self::after_in`] says. One the document never had is `Mismatch`.
    fn after_anchor(&self, anchor: Option<ParaId>) -> Result<usize, DocError> {
        let Some(anchor) = anchor else { return Ok(0) };
        if let Some(i) = self.doc.index_of(anchor) {
            return Ok(i + 1);
        }
        self.after_in(&self.history_order(), anchor)
            .ok_or(DocError::Mismatch("run anchor: the document never had this paragraph"))
    }

    /// Pair each still unpaired fixed old line with the new line at the same
    /// offset inside its gap between paired lines, if that one has its kind
    /// (dual dialogue takes text edits: D2). Keeps pairs increasing.
    fn pair_fixed_by_position(&self, old: &[crate::model::Paragraph], new: &[NewPara], pair: &mut [Option<usize>]) {
        let mut old_to_new: Vec<Option<usize>> = vec![None; old.len()];
        for (j, i) in pair.iter().enumerate() {
            if let Some(i) = i {
                old_to_new[*i] = Some(j);
            }
        }
        // Walk the gaps: old[i0..i1) against new[j0..j1).
        let (mut i0, mut j0) = (0, 0);
        for i1 in 0..=old.len() {
            let paired = old_to_new.get(i1).copied().flatten();
            if i1 < old.len() && paired.is_none() {
                continue;
            }
            let j1 = paired.unwrap_or(new.len());
            for (i, o) in old.iter().enumerate().take(i1).skip(i0) {
                let j = j0 + (i - i0);
                if j < j1 && self.doc.script.is_fixed(o.id) && new[j].kind == o.kind {
                    pair[j] = Some(i);
                }
            }
            (i0, j0) = (i1 + 1, j1 + 1);
        }
    }

    /// Set paragraph `id`'s spans to `target`'s text, splicing only the
    /// changed middle (as [`super::Document::replace_text`] does). The middle
    /// takes `target`'s styles if `target` has any styled span, else the
    /// style of the text it replaces (or of the text before it).
    fn splice_to(&mut self, id: ParaId, target: &[Span]) -> Result<(), DocError> {
        let p = self.para(id)?;
        let old = p.text();
        let text = spans::text(target);
        let (pre, suf) = common_ends(&old, &text);
        let mid = pre..text.len() - suf;
        let inserted = if target.iter().any(|s| s.style != Style::default()) {
            spans::slice(target, mid)
        } else if mid.is_empty() {
            vec![]
        } else {
            let removed = spans::slice(&p.spans, pre..old.len() - suf);
            let style = if removed.is_empty() { spans::style_at(&p.spans, pre) } else { removed[0].style };
            vec![Span { text: text[mid].to_string(), style }]
        };
        self.splice(id, pre..old.len() - suf, inserted)
    }
}

impl Document {
    /// Every paragraph this document has had since its starting point, in
    /// script order: its starting paragraphs, with each line added since
    /// placed where it was added (just before the line that followed it then,
    /// else after the one before it) and kept there after it is removed.
    /// Undo and redo included ([`Document::for_each_applied_op`]): a line
    /// they bring back is already in place. Where a gone paragraph was is
    /// then just after the nearest live paragraph before it here.
    pub fn history_order(&self) -> Vec<ParaId> {
        let mut h = History::new(&self.initial);
        self.for_each_applied_op(|op| h.apply(op));
        h.order
    }
}

/// [`Document::history_order`] while replaying ops: the order so far, and
/// the paragraphs live at this point of the replay.
struct History {
    order: Vec<ParaId>,
    seen: HashSet<ParaId>,
    live: Vec<ParaId>,
}

impl History {
    fn new(initial: &Script) -> Self {
        let order: Vec<ParaId> = initial.paragraphs.iter().map(|p| p.id).collect();
        History { seen: order.iter().copied().collect(), live: order.clone(), order }
    }

    /// Puts `new` just before `next` in the order (else after `prev`, else
    /// last), unless it is there already.
    fn place(&mut self, new: ParaId, next: Option<ParaId>, prev: Option<ParaId>) {
        if !self.seen.insert(new) {
            return;
        }
        let at = next
            .and_then(|n| self.order.iter().position(|&p| p == n))
            .or_else(|| prev.and_then(|p| self.order.iter().position(|&q| q == p).map(|i| i + 1)))
            .unwrap_or(self.order.len());
        self.order.insert(at, new);
    }

    fn apply(&mut self, op: &Op) {
        match op {
            Op::InsertPara { index, para } => {
                let i = (*index).min(self.live.len());
                let (next, prev) = (self.live.get(i).copied(), i.checked_sub(1).map(|k| self.live[k]));
                self.place(para.id, next, prev);
                self.live.insert(i, para.id);
            }
            Op::RemovePara { para, .. } => self.live.retain(|&p| p != para.id),
            Op::Split { para, new_para, .. } => {
                let i = self.live.iter().position(|p| p == para).map_or(self.live.len(), |i| i + 1);
                self.place(*new_para, self.live.get(i).copied(), Some(*para));
                self.live.insert(i, *new_para);
            }
            Op::Merge { next, .. } => self.live.retain(|p| p != next),
            Op::Splice { .. } | Op::SetKind { .. } | Op::SetMeta { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Document, Group, Pos, Selection};
    use crate::model::{ElementKind, Paragraph, Script};
    use ElementKind::*;

    fn doc(lines: &[(ElementKind, &str)]) -> Document {
        let paragraphs =
            lines.iter().enumerate().map(|(i, (k, t))| Paragraph::plain(ParaId(i as u64), k.clone(), *t)).collect();
        Document::new(Script { paragraphs, ..Script::default() })
    }

    fn edit<T>(d: &mut Document, f: impl FnOnce(&mut Edit<'_>) -> Result<T, DocError>) -> Result<T, DocError> {
        let sel = Selection::cursor(Pos::new(d.paragraphs()[0].id, 0));
        d.edit(sel, Group::Other, |e| f(e).map(|v| (sel, v)))
    }

    fn state(d: &Document) -> Vec<(u64, String)> {
        d.paragraphs().iter().map(|p| (p.id.0, p.text())).collect()
    }

    fn st(pairs: &[(u64, &str)]) -> Vec<(u64, String)> {
        pairs.iter().map(|(i, t)| (*i, t.to_string())).collect()
    }

    fn sp(text: &str, italic: bool) -> Span {
        Span { text: text.into(), style: Style { italic, ..Style::default() } }
    }

    fn four() -> Document {
        doc(&[(Action, "Zero."), (Action, "One."), (Action, "Two."), (Action, "Three.")])
    }

    #[test]
    fn a_cut_reports_where_its_lines_were() {
        let mut d = four();
        let r = edit(&mut d, |e| e.replace_run(Run::Ids(vec![ParaId(1), ParaId(2)]), &[])).unwrap();
        assert_eq!(r, Replaced { ids: vec![], after: Some(ParaId(0)) });
        let r = edit(&mut d, |e| e.replace_run(Run::Ids(vec![ParaId(0)]), &[])).unwrap();
        assert_eq!(r, Replaced { ids: vec![], after: None });
        // Switching from the cut to another option goes back to the same place.
        let r =
            edit(&mut d, |e| e.replace_run(Run::Empty { after: None }, &[NewPara::plain(Action, "Back.")])).unwrap();
        assert_eq!(r.after, None);
        assert_eq!(state(&d), st(&[(4, "Back."), (3, "Three.")]));
    }

    #[test]
    fn empty_run_with_a_gone_anchor_falls_back_to_its_live_predecessor() {
        let mut d = four();
        edit(&mut d, |e| e.remove_para(ParaId(2))).unwrap();
        edit(&mut d, |e| e.remove_para(ParaId(1))).unwrap();
        let r = edit(&mut d, |e| e.replace_run(Run::Empty { after: Some(ParaId(2)) }, &[NewPara::plain(Action, "X.")]))
            .unwrap();
        assert_eq!(r, Replaced { ids: vec![ParaId(4)], after: Some(ParaId(0)) });
        assert_eq!(state(&d), st(&[(0, "Zero."), (4, "X."), (3, "Three.")]));
    }

    #[test]
    fn an_anchor_the_document_never_had_is_a_mismatch() {
        let mut d = four();
        let new = [NewPara::plain(Action, "X.")];
        let gone = ParaId(99);
        let err = edit(&mut d, |e| e.replace_run(Run::Empty { after: Some(gone) }, &new)).unwrap_err();
        assert!(matches!(err, DocError::Mismatch(_)), "{err:?}");
        let err = edit(&mut d, |e| e.restore_run(Run::Empty { after: Some(gone) }, &[])).unwrap_err();
        assert!(matches!(err, DocError::Mismatch(_)), "{err:?}");
        // No live run id and none the document ever had: nowhere to go.
        let err = edit(&mut d, |e| e.replace_run(Run::Ids(vec![gone]), &new)).unwrap_err();
        assert!(matches!(err, DocError::Mismatch(_)), "{err:?}");
        assert_eq!(state(&d), state(&four()));
    }

    #[test]
    fn a_gone_anchor_added_since_the_start_is_placed_from_the_history() {
        let mut d = four();
        // A line added after 1, later removed, along with the line before it.
        let added = edit(&mut d, |e| e.insert_para(2, NewPara::plain(Action, "Added."))).unwrap();
        let more = edit(&mut d, |e| e.insert_para(2, NewPara::plain(Action, "More."))).unwrap();
        edit(&mut d, |e| e.remove_para(added)).unwrap();
        edit(&mut d, |e| e.remove_para(ParaId(1))).unwrap();
        assert_eq!(d.history_order(), vec![ParaId(0), ParaId(1), more, added, ParaId(2), ParaId(3)]);
        let new = [NewPara::plain(Action, "X.")];
        let r = edit(&mut d, |e| e.replace_run(Run::Empty { after: Some(added) }, &new)).unwrap();
        assert_eq!(r.after, Some(more));
        assert_eq!(state(&d), st(&[(0, "Zero."), (more.0, "More."), (r.ids[0].0, "X."), (2, "Two."), (3, "Three.")]));
        // A run of lines all gone goes back where its first line was.
        let mut d = four();
        let added = edit(&mut d, |e| e.insert_para(1, NewPara::plain(Action, "Added."))).unwrap();
        edit(&mut d, |e| e.remove_para(added)).unwrap();
        edit(&mut d, |e| e.remove_para(ParaId(0))).unwrap();
        let r = edit(&mut d, |e| e.replace_run(Run::Ids(vec![added]), &new)).unwrap();
        assert_eq!(r.after, None);
        edit(&mut d, |e| e.restore_run(Run::Ids(vec![r.ids[0]]), &[(added, NewPara::plain(Action, "Added."))]))
            .unwrap();
        assert_eq!(state(&d), st(&[(added.0, "Added."), (1, "One."), (2, "Two."), (3, "Three.")]));
    }

    #[test]
    fn an_initial_anchor_whose_initial_neighbours_are_gone_follows_lines_added_before_it() {
        let mut d = four();
        // New lines before 2, then 1 and 2 removed: a cut anchored on 2 goes
        // after the new lines, not to the start.
        let a = edit(&mut d, |e| e.insert_para(2, NewPara::plain(Action, "New."))).unwrap();
        edit(&mut d, |e| e.remove_para(ParaId(1))).unwrap();
        edit(&mut d, |e| e.remove_para(ParaId(0))).unwrap();
        edit(&mut d, |e| e.remove_para(ParaId(2))).unwrap();
        let r = edit(&mut d, |e| e.replace_run(Run::Empty { after: Some(ParaId(2)) }, &[NewPara::plain(Action, "X.")]))
            .unwrap();
        assert_eq!(r.after, Some(a));
        assert_eq!(state(&d), st(&[(a.0, "New."), (r.ids[0].0, "X."), (3, "Three.")]));
    }

    #[test]
    fn re_accept_over_a_run_changed_by_hand_uses_its_live_span() {
        let mut d = four();
        // The decided run was 1,2; since then 2 was removed and a line added
        // after 1, and a line added after it.
        edit(&mut d, |e| e.remove_para(ParaId(2))).unwrap();
        edit(&mut d, |e| e.insert_para(2, NewPara::plain(Action, "Added."))).unwrap();
        let r = edit(&mut d, |e| {
            e.replace_run(Run::Ids(vec![ParaId(1), ParaId(2), ParaId(4)]), &[NewPara::plain(Action, "One.")])
        })
        .unwrap();
        assert_eq!(r, Replaced { ids: vec![ParaId(1)], after: Some(ParaId(0)) });
        assert_eq!(state(&d), st(&[(0, "Zero."), (1, "One."), (3, "Three.")]));
        // None live: back after the first id's initial predecessor.
        edit(&mut d, |e| e.remove_para(ParaId(1))).unwrap();
        let r = edit(&mut d, |e| e.replace_run(Run::Ids(vec![ParaId(1)]), &[NewPara::plain(Action, "Uno.")])).unwrap();
        assert_eq!(r, Replaced { ids: vec![ParaId(5)], after: Some(ParaId(0)) });
        assert_eq!(state(&d), st(&[(0, "Zero."), (5, "Uno."), (3, "Three.")]));
    }

    #[test]
    fn restore_keeps_styles_on_the_unchanged_ends() {
        let styled = vec![sp("She ", false), sp("waits", true), sp(" here.", false)];
        let mut d = Document::new(Script {
            paragraphs: vec![Paragraph { id: ParaId(0), kind: Action, spans: styled.clone() }],
            ..Script::default()
        });
        edit(&mut d, |e| e.replace_run(Run::Ids(vec![ParaId(0)]), &[NewPara::plain(Action, "She waits there.")]))
            .unwrap();
        assert_eq!(d.paragraphs()[0].spans, vec![sp("She ", false), sp("waits", true), sp(" there.", false)]);
        // Keep mine with a plain typed BEFORE: the italics survive.
        edit(&mut d, |e| {
            e.restore_run(Run::Ids(vec![ParaId(0)]), &[(ParaId(0), NewPara::plain(Action, "She waits here."))])
        })
        .unwrap();
        assert_eq!(d.paragraphs()[0].spans, styled);
    }

    #[test]
    fn a_styled_new_line_brings_its_styles_a_plain_one_inherits() {
        let mut d = Document::new(Script {
            paragraphs: vec![Paragraph {
                id: ParaId(0),
                kind: Action,
                spans: vec![sp("She waits by the front door all night.", true)],
            }],
            ..Script::default()
        });
        edit(&mut d, |e| {
            e.replace_run(
                Run::Ids(vec![ParaId(0)]),
                &[NewPara::plain(Action, "She waits by the front door all night!")],
            )
        })
        .unwrap();
        assert_eq!(d.paragraphs()[0].spans, vec![sp("She waits by the front door all night!", true)]);
        let new = vec![sp("She waits by the front door all night", true), sp(" now", false), sp("!", true)];
        edit(&mut d, |e| e.replace_run(Run::Ids(vec![ParaId(0)]), &[NewPara { kind: Action, spans: new.clone() }]))
            .unwrap();
        assert_eq!(d.paragraphs()[0].spans, new);
        assert_eq!(d.paragraphs()[0].id, ParaId(0));
    }

    #[test]
    fn dual_dialogue_takes_text_changes_but_no_structural_ones() {
        let base = || {
            let mut d = doc(&[(Action, "Zero."), (Character, "ANA"), (Dialogue, "Hello there."), (Action, "Three.")]);
            let mut s = d.script().clone();
            s.fixed = [(ParaId(1), 0), (ParaId(2), 0)].into();
            d = Document::new(s);
            d
        };
        let mut d = base();
        let run = || Run::Ids(vec![ParaId(1), ParaId(2), ParaId(3)]);
        let new = [
            NewPara::plain(Character, "ANA"),
            NewPara::plain(Dialogue, "Completely other words entirely!"),
            NewPara::plain(Action, "Three."),
        ];
        let r = edit(&mut d, |e| e.replace_run(run(), &new)).unwrap();
        assert_eq!(r.ids, vec![ParaId(1), ParaId(2), ParaId(3)]);
        assert_eq!(state(&d)[2], (2, "Completely other words entirely!".to_string()));

        let refused = [
            // The dialogue line removed.
            vec![NewPara::plain(Character, "ANA"), NewPara::plain(Action, "Three.")],
            // Retyped.
            vec![
                NewPara::plain(Character, "ANA"),
                NewPara::plain(Action, "Hello there."),
                NewPara::plain(Action, "Three."),
            ],
            // A line inserted inside the group.
            vec![
                NewPara::plain(Character, "ANA"),
                NewPara::plain(Parenthetical, "(softly)"),
                NewPara::plain(Dialogue, "Hello there."),
                NewPara::plain(Action, "Three."),
            ],
        ];
        for new in refused {
            let mut d = base();
            let err = edit(&mut d, |e| e.replace_run(run(), &new)).unwrap_err();
            assert!(matches!(err, DocError::Fixed(_)), "{err:?}");
            assert_eq!(state(&d), state(&base()));
        }
    }
}
