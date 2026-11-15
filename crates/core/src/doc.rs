//! The editable document: a [`Script`] plus an append-only event log.
//!
//! Every change is a [`Transaction`] of primitive, invertible [`Op`]s. The log
//! records edits, undos and redos as [`Event`]s; the current script is what you
//! get by replaying the log over the initial script ([`Document::replay`]).
//! Undo and redo are themselves events, so history is never rewritten.
//!
//! UIs never mutate the script directly. They call commands
//! ([`Document::insert_text`], [`Document::enter`], …) with a [`Selection`] and
//! get back the selection to show next. Every command, and anything outside
//! core that changes the script (CLI commands, review decisions), is built from
//! the primitives of [`Edit`], run in one transaction by [`Document::edit`].
//!
//! Paragraph identity rules (a first version, see AGENTS.md):
//! - Typing, deleting and retyping never change a paragraph's id.
//! - Enter in the middle of a paragraph: the first half keeps the id, the
//!   second half gets a new one. Enter at the start inserts a new empty
//!   paragraph *above*, so the text keeps its id. Enter at the end adds a new
//!   paragraph below.
//! - Joining two paragraphs keeps the first one's id, except that removing an
//!   empty paragraph never costs its non-empty neighbour its id.
//! - Undo and redo restore the exact ids that existed before.
//! - A fresh id is never one that was used before, even if no paragraph has
//!   it now ([`Document::next_id`], [`Script::id_floor`]): an exporter takes
//!   an id for the file's paragraph it came from. Only
//!   [`Edit::insert_para_with_id`] (restoring removed lines) brings one back.
//! - [`Script::fixed`] paragraphs take text edits only: [`Edit`] refuses to
//!   add, remove, split, join or retype them ([`DocError::Fixed`]). Replaying
//!   logged ops, undo and redo are not checked (logs made before the rule).
//!
//! Besides the script, a document holds *metadata*: string keys and values
//! that other crates attach (e.g. a review decision). Metadata changes are ops
//! like any other, so a decision and the text change it causes are one
//! transaction and undo together. Core never interprets the keys.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

use crate::model::{ElementKind, ParaId, Paragraph, Script, Span};
use crate::{rules, spans};

/// A position: a byte offset into a paragraph's text, on a char boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pos {
    pub para: ParaId,
    pub offset: usize,
}

impl Pos {
    pub fn new(para: ParaId, offset: usize) -> Self {
        Self { para, offset }
    }
}

/// `anchor` is where the selection started, `head` is where the cursor is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Selection {
    pub anchor: Pos,
    pub head: Pos,
}

impl Selection {
    pub fn cursor(pos: Pos) -> Self {
        Self { anchor: pos, head: pos }
    }

    pub fn is_collapsed(&self) -> bool {
        self.anchor == self.head
    }
}

/// A primitive, invertible change. Each op carries what it removes so it can be undone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    /// In paragraph `para`, replace the spans `removed` starting at byte `at` with `inserted`.
    Splice {
        para: ParaId,
        at: usize,
        removed: Vec<Span>,
        inserted: Vec<Span>,
    },
    /// Split `para` at byte `at`; the tail becomes a new paragraph `new_para` of kind `new_kind`.
    Split {
        para: ParaId,
        at: usize,
        new_para: ParaId,
        new_kind: ElementKind,
    },
    /// Append paragraph `next` (which must directly follow) onto `para`, whose length is `at`.
    Merge {
        para: ParaId,
        at: usize,
        next: ParaId,
        next_kind: ElementKind,
    },
    SetKind {
        para: ParaId,
        from: ElementKind,
        to: ElementKind,
    },
    InsertPara {
        index: usize,
        para: Paragraph,
    },
    RemovePara {
        index: usize,
        para: Paragraph,
    },
    /// Set metadata `key` from `from` to `to` (`None` = absent).
    SetMeta {
        key: String,
        from: Option<String>,
        to: Option<String>,
    },
}

impl Op {
    pub fn inverse(&self) -> Op {
        match self.clone() {
            Op::Splice { para, at, removed, inserted } => Op::Splice { para, at, removed: inserted, inserted: removed },
            Op::Split { para, at, new_para, new_kind } => Op::Merge { para, at, next: new_para, next_kind: new_kind },
            Op::Merge { para, at, next, next_kind } => Op::Split { para, at, new_para: next, new_kind: next_kind },
            Op::SetKind { para, from, to } => Op::SetKind { para, from: to, to: from },
            Op::InsertPara { index, para } => Op::RemovePara { index, para },
            Op::RemovePara { index, para } => Op::InsertPara { index, para },
            Op::SetMeta { key, from, to } => Op::SetMeta { key, from: to, to: from },
        }
    }
}

/// How a transaction groups with its neighbours for undo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Group {
    /// Consecutive typing undoes word by word.
    Typing,
    /// Consecutive single deletions undo together.
    Deleting,
    /// Always its own undo step.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    pub ops: Vec<Op>,
    pub before: Selection,
    pub after: Selection,
    pub group: Group,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    Edit(Transaction),
    Undo,
    Redo,
    /// Merge the undo steps above `depth` into one (see [`Document::merge_undo_since`]).
    MergeUndo {
        depth: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocError {
    MissingParagraph(ParaId),
    /// An op didn't match the document (a bug in the caller or a corrupt log).
    Mismatch(&'static str),
    /// A structural edit (adding, removing, splitting, joining or retyping
    /// lines) touching this [`Script::fixed`] paragraph, which takes text
    /// edits only.
    Fixed(ParaId),
    /// Removing the only remaining paragraph: a document is never empty. For a
    /// full replace, insert the new paragraphs before removing the old ones.
    LastParagraph,
}

impl fmt::Display for DocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingParagraph(id) => write!(f, "no paragraph with id {}", id.0),
            Self::Mismatch(what) => write!(f, "operation does not match the document: {what}"),
            Self::LastParagraph => write!(f, "the last paragraph can't be removed: insert the replacement first"),
            Self::Fixed(id) => write!(
                f,
                "paragraph {} takes text edits only: lines can't be added, removed, split, joined or retyped there",
                id.0
            ),
        }
    }
}

impl std::error::Error for DocError {}

/// What to tell a writer when an edit is refused with [`DocError::Fixed`]
/// (the app's notice; the CLI and tool validation say the same).
pub const FIXED_MESSAGE: &str =
    "Dual dialogue lines can't be added, removed or split here; edit their text, or change them in Final Draft.";

/// Source of [`Document::origin`] values.
static NEXT_ORIGIN: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct Document {
    origin: u64,
    initial: Script,
    initial_meta: BTreeMap<String, String>,
    script: Script,
    meta: BTreeMap<String, String>,
    next_id: u64,
    log: Vec<Event>,
    undo: Vec<Vec<Transaction>>,
    redo: Vec<Vec<Transaction>>,
}

impl Document {
    /// Start editing `script`. Spans are normalized, and an empty script gets
    /// one empty Action paragraph so there is always somewhere to type.
    pub fn new(script: Script) -> Self {
        Self::with_meta(script, BTreeMap::new())
    }

    /// Like [`Document::new`], starting with metadata `meta`. Fresh ids start
    /// above every live id and at [`Script::id_floor`] or later.
    pub fn with_meta(mut script: Script, meta: BTreeMap<String, String>) -> Self {
        for p in &mut script.paragraphs {
            spans::normalize(&mut p.spans);
        }
        if script.paragraphs.is_empty() {
            let id = ParaId(script.id_floor);
            script.paragraphs.push(Paragraph { id, kind: ElementKind::Action, spans: vec![] });
        }
        let next_id = script.paragraphs.iter().map(|p| p.id.0 + 1).max().unwrap_or(0).max(script.id_floor);
        Self {
            origin: NEXT_ORIGIN.fetch_add(1, Ordering::Relaxed),
            initial: script.clone(),
            initial_meta: meta.clone(),
            script,
            meta,
            next_id,
            log: vec![],
            undo: vec![],
            redo: vec![],
        }
    }

    /// Make the current state the starting point: the log and undo history are
    /// dropped. Used for state loaded from a file, which undo shouldn't undo.
    /// The new starting script's [`Script::id_floor`] is this document's
    /// [`Document::next_id`], so ids used and since removed stay used (also
    /// for [`Document::initial`], which a rebuild replays from).
    pub fn rebase(self) -> Self {
        let mut script = self.script;
        script.id_floor = self.next_id;
        Self::with_meta(script, self.meta)
    }

    /// Rebuild a document by replaying `events` over `initial`.
    pub fn replay(initial: Script, events: &[Event]) -> Result<Self, DocError> {
        Self::new(initial).replay_onto(events)
    }

    /// Replay `events` on top of this document's current state.
    pub fn replay_onto(mut self, events: &[Event]) -> Result<Self, DocError> {
        let doc = &mut self;
        for e in events {
            match e {
                Event::Edit(t) => {
                    for op in &t.ops {
                        doc.apply_op(op)?;
                    }
                    for op in &t.ops {
                        doc.note_ids(op);
                    }
                    doc.record_edit(t.clone());
                }
                Event::Undo => {
                    doc.undo();
                }
                Event::Redo => {
                    doc.redo();
                }
                Event::MergeUndo { depth } => doc.merge_undo_since(*depth),
            }
        }
        Ok(self)
    }

    pub fn script(&self) -> &Script {
        &self.script
    }

    /// Identifies where this document's history started: unique to each
    /// [`Document::new`], [`Document::with_meta`] and [`Document::rebase`]
    /// (so also each replay), shared by clones. Lets a cache tell a rebased
    /// document from its predecessor when other counts happen to match.
    pub fn origin(&self) -> u64 {
        self.origin
    }

    /// The id the next new paragraph gets. Every id below it has been used
    /// (it may be absent now); none at or above it has.
    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    pub fn initial(&self) -> &Script {
        &self.initial
    }

    pub fn initial_meta(&self) -> &BTreeMap<String, String> {
        &self.initial_meta
    }

    pub fn meta(&self, key: &str) -> Option<&str> {
        self.meta.get(key).map(String::as_str)
    }

    pub fn all_meta(&self) -> &BTreeMap<String, String> {
        &self.meta
    }

    pub fn log(&self) -> &[Event] {
        &self.log
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn paragraphs(&self) -> &[Paragraph] {
        &self.script.paragraphs
    }

    pub fn index_of(&self, id: ParaId) -> Option<usize> {
        self.script.position(id)
    }

    pub fn paragraph(&self, id: ParaId) -> Option<&Paragraph> {
        self.script.get(id)
    }

    fn para_len(&self, id: ParaId) -> usize {
        self.paragraph(id).map_or(0, |p| spans::len(&p.spans))
    }

    fn fresh_id(&mut self) -> ParaId {
        let id = ParaId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Keep `next_id` ahead of any id an op introduces (matters when replaying).
    fn note_ids(&mut self, op: &Op) {
        let id = match op {
            Op::Split { new_para, .. } => new_para.0,
            Op::InsertPara { para, .. } => para.id.0,
            _ => return,
        };
        self.next_id = self.next_id.max(id + 1);
    }

    // ---- positions -------------------------------------------------------

    pub fn start(&self) -> Pos {
        Pos::new(self.script.paragraphs[0].id, 0)
    }

    pub fn end(&self) -> Pos {
        let last = self.script.paragraphs.last().expect("never empty");
        Pos::new(last.id, spans::len(&last.spans))
    }

    /// Make `pos` valid: an existing paragraph, an offset within it on a char boundary.
    pub fn clamp(&self, pos: Pos) -> Pos {
        let Some(p) = self.paragraph(pos.para) else { return self.start() };
        let text = p.text();
        let mut offset = pos.offset.min(text.len());
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        Pos::new(pos.para, offset)
    }

    pub fn clamp_selection(&self, sel: Selection) -> Selection {
        Selection { anchor: self.clamp(sel.anchor), head: self.clamp(sel.head) }
    }

    fn key(&self, pos: Pos) -> (usize, usize) {
        (self.index_of(pos.para).unwrap_or(0), pos.offset)
    }

    /// The selection's ends in document order.
    pub fn ordered(&self, sel: Selection) -> (Pos, Pos) {
        if self.key(sel.anchor) <= self.key(sel.head) { (sel.anchor, sel.head) } else { (sel.head, sel.anchor) }
    }

    /// Selected text, paragraphs joined with '\n'.
    pub fn text_in(&self, sel: Selection) -> String {
        let (a, b) = self.ordered(sel);
        let (ia, ib) = (self.key(a).0, self.key(b).0);
        let mut out = String::new();
        for i in ia..=ib {
            let text = self.script.paragraphs[i].text();
            let lo = if i == ia { a.offset } else { 0 };
            let hi = if i == ib { b.offset } else { text.len() };
            if i > ia {
                out.push('\n');
            }
            out.push_str(&text[lo.min(hi)..hi]);
        }
        out
    }

    /// One grapheme (or word, if `word`) to the left; crosses into the previous paragraph at its start.
    pub fn move_left(&self, pos: Pos, word: bool) -> Pos {
        if pos.offset == 0 {
            let i = self.key(pos).0;
            return if i == 0 { pos } else { self.end_of(i - 1) };
        }
        let text = self.paragraph(pos.para).map(|p| p.text()).unwrap_or_default();
        let offset = if word { prev_word(&text, pos.offset) } else { prev_grapheme(&text, pos.offset) };
        Pos::new(pos.para, offset)
    }

    /// One grapheme (or word) to the right; crosses into the next paragraph at its end.
    pub fn move_right(&self, pos: Pos, word: bool) -> Pos {
        let text = self.paragraph(pos.para).map(|p| p.text()).unwrap_or_default();
        if pos.offset >= text.len() {
            let i = self.key(pos).0;
            return match self.script.paragraphs.get(i + 1) {
                Some(next) => Pos::new(next.id, 0),
                None => pos,
            };
        }
        let offset = if word { next_word(&text, pos.offset) } else { next_grapheme(&text, pos.offset) };
        Pos::new(pos.para, offset)
    }

    /// The word around `pos` (for double-click), or an empty range if there is none.
    pub fn word_at(&self, pos: Pos) -> Selection {
        let text = self.paragraph(pos.para).map(|p| p.text()).unwrap_or_default();
        for (i, seg) in text.split_word_bound_indices() {
            if pos.offset >= i && pos.offset <= i + seg.len() && seg.chars().any(char::is_alphanumeric) {
                return Selection { anchor: Pos::new(pos.para, i), head: Pos::new(pos.para, i + seg.len()) };
            }
        }
        Selection::cursor(pos)
    }

    fn end_of(&self, index: usize) -> Pos {
        let p = &self.script.paragraphs[index];
        Pos::new(p.id, spans::len(&p.spans))
    }

    // ---- commands ---------------------------------------------------------

    /// Replace the selection with `text`. Newlines start new paragraphs, as Enter would.
    pub fn insert_text(&mut self, sel: Selection, text: &str) -> Result<Selection, DocError> {
        let sel = self.clamp_selection(sel);
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let group = if sel.is_collapsed() && !text.contains('\n') { Group::Typing } else { Group::Other };
        self.transact(sel, group, |tx| {
            let mut pos = tx.delete_selection(sel)?;
            for (i, line) in text.split('\n').enumerate() {
                if i > 0 {
                    pos = tx.enter_at(pos)?;
                }
                pos = tx.insert_plain(pos, line)?;
            }
            Ok(Selection::cursor(pos))
        })
    }

    /// Enter: split the paragraph, following [`rules::next_kind`] at the end of a paragraph.
    pub fn enter(&mut self, sel: Selection) -> Result<Selection, DocError> {
        let sel = self.clamp_selection(sel);
        self.transact(sel, Group::Other, |tx| {
            let pos = tx.delete_selection(sel)?;
            Ok(Selection::cursor(tx.enter_at(pos)?))
        })
    }

    /// Backspace (or word-backspace). At the start of a paragraph, joins it to the previous one.
    pub fn delete_backward(&mut self, sel: Selection, word: bool) -> Result<Selection, DocError> {
        let sel = self.clamp_selection(sel);
        if !sel.is_collapsed() {
            return self.transact(sel, Group::Other, |tx| Ok(Selection::cursor(tx.delete_selection(sel)?)));
        }
        let pos = sel.head;
        if pos.offset > 0 {
            let to = self.move_left(pos, word);
            return self.transact(sel, Group::Deleting, |tx| Ok(Selection::cursor(tx.delete_range(to, pos)?)));
        }
        let i = self.key(pos).0;
        if i == 0 {
            return Ok(sel);
        }
        let prev = self.script.paragraphs[i - 1].id;
        self.transact(sel, Group::Other, |tx| Ok(Selection::cursor(tx.join(prev, pos.para)?)))
    }

    /// Delete (or word-delete). At the end of a paragraph, joins the next one onto it.
    pub fn delete_forward(&mut self, sel: Selection, word: bool) -> Result<Selection, DocError> {
        let sel = self.clamp_selection(sel);
        if !sel.is_collapsed() {
            return self.transact(sel, Group::Other, |tx| Ok(Selection::cursor(tx.delete_selection(sel)?)));
        }
        let pos = sel.head;
        if pos.offset < self.para_len(pos.para) {
            let to = self.move_right(pos, word);
            return self.transact(sel, Group::Deleting, |tx| Ok(Selection::cursor(tx.delete_range(pos, to)?)));
        }
        let i = self.key(pos).0;
        let Some(next) = self.script.paragraphs.get(i + 1).map(|p| p.id) else { return Ok(sel) };
        self.transact(sel, Group::Other, |tx| {
            let joined = tx.join(pos.para, next)?;
            // Keep the cursor where it was, even if the empty paragraph it was in went away.
            Ok(Selection::cursor(if joined.para == next { Pos::new(next, 0) } else { pos }))
        })
    }

    /// The paragraph a Backspace (`forward` false) or Delete at `sel` would
    /// remove or merge away by joining, if it would join: a collapsed cursor
    /// at the start (end) of a paragraph with one above (below). The rules
    /// are [`Edit::join`]'s: an empty paragraph goes, else the second one is
    /// merged into the first.
    pub fn removed_by_join(&self, sel: Selection, forward: bool) -> Option<ParaId> {
        let sel = self.clamp_selection(sel);
        if !sel.is_collapsed() {
            return None;
        }
        let pos = sel.head;
        let i = self.index_of(pos.para)?;
        let (first, second) = if forward {
            if pos.offset < self.para_len(pos.para) {
                return None;
            }
            (pos.para, self.script.paragraphs.get(i + 1)?.id)
        } else {
            if pos.offset > 0 || i == 0 {
                return None;
            }
            (self.script.paragraphs[i - 1].id, pos.para)
        };
        let empty = |id| self.para_len(id) == 0;
        Some(if empty(first) && !empty(second) { first } else { second })
    }

    /// Give every paragraph touched by the selection the element `kind`.
    pub fn set_kind(&mut self, sel: Selection, kind: ElementKind) -> Result<Selection, DocError> {
        let sel = self.clamp_selection(sel);
        let (a, b) = self.ordered(sel);
        let ids: Vec<ParaId> = self.script.paragraphs[self.key(a).0..=self.key(b).0].iter().map(|p| p.id).collect();
        self.transact(sel, Group::Other, |tx| {
            for id in ids {
                tx.set_kind(id, kind.clone())?;
            }
            Ok(sel)
        })
    }

    /// Tab / Shift-Tab: cycle the element kind of the paragraph with the cursor.
    pub fn cycle_kind(&mut self, sel: Selection, forward: bool) -> Result<Selection, DocError> {
        let sel = self.clamp_selection(sel);
        let kind = self.paragraph(sel.head.para).map(|p| p.kind.clone()).unwrap_or(ElementKind::Action);
        self.set_kind(sel, rules::cycle_kind(&kind, forward))
    }

    /// Set paragraph `para`'s text to `text` and change metadata, as one undo
    /// step. Only the changed middle of the text is spliced, so styles on the
    /// unchanged start and end survive; inserted text takes the style of the
    /// text it replaces. Nothing is logged if nothing changes. Returns a cursor
    /// at the end of the changed text.
    pub fn replace_text(
        &mut self,
        sel: Selection,
        para: ParaId,
        text: &str,
        meta: &[(&str, Option<&str>)],
    ) -> Result<Selection, DocError> {
        let sel = self.clamp_selection(sel);
        let p = self.paragraph(para).ok_or(DocError::MissingParagraph(para))?;
        let old = p.text();
        let (pre, suf) = common_ends(&old, text);
        let removed = spans::slice(&p.spans, pre..old.len() - suf);
        let new_mid = &text[pre..text.len() - suf];
        let inserted = if new_mid.is_empty() {
            vec![]
        } else {
            let style = if removed.is_empty() { spans::style_at(&p.spans, pre) } else { removed[0].style };
            vec![Span { text: new_mid.to_string(), style }]
        };
        let end = Pos::new(para, text.len() - suf);
        self.transact(sel, Group::Other, |tx| {
            tx.splice(para, pre..old.len() - suf, inserted)?;
            for (key, to) in meta {
                tx.set_meta(key, *to)?;
            }
            Ok(Selection::cursor(end))
        })
    }

    /// How many undo steps there are. Pass it to [`Document::merge_undo_since`]
    /// later to make everything done in between one step.
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    /// Make every undo step above `depth` one step (e.g. a whole rewrite of a
    /// line: the starting text, the typing, the decision). Logged, so replaying
    /// gives the same undo history.
    pub fn merge_undo_since(&mut self, depth: usize) {
        if self.undo.len() > depth + 1 {
            let merged: Vec<Transaction> = self.undo.drain(depth..).flatten().collect();
            self.undo.push(merged);
            self.log.push(Event::MergeUndo { depth });
        }
    }

    /// Metadata keys the next undo (or redo) would change.
    pub fn next_undo_meta_keys(&self, redo: bool) -> Vec<&str> {
        let group = if redo { self.redo.last() } else { self.undo.last() };
        group
            .into_iter()
            .flatten()
            .flat_map(|t| &t.ops)
            .filter_map(|op| match op {
                Op::SetMeta { key, .. } => Some(key.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Paragraphs the next undo (or redo) would change, in order.
    pub fn next_undo_paras(&self, redo: bool) -> Vec<ParaId> {
        let group = if redo { self.redo.last() } else { self.undo.last() };
        let mut out: Vec<ParaId> = vec![];
        for t in group.into_iter().flatten() {
            for op in &t.ops {
                let id = match op {
                    Op::Splice { para, .. }
                    | Op::Split { para, .. }
                    | Op::Merge { para, .. }
                    | Op::SetKind { para, .. } => *para,
                    Op::InsertPara { para, .. } | Op::RemovePara { para, .. } => para.id,
                    Op::SetMeta { .. } => continue,
                };
                if !out.contains(&id) {
                    out.push(id);
                }
            }
        }
        out
    }

    /// Calls `f` with every op applied since the starting point, in order:
    /// each logged edit's ops, and for an undo (redo) the inverses (the ops)
    /// of the group it undid (redid), as [`Document::undo`] applies them.
    /// Replays only the undo history, not the text, so it is cheap.
    pub fn for_each_applied_op(&self, mut f: impl FnMut(&Op)) {
        let (mut undo, mut redo): (Vec<Vec<&Transaction>>, Vec<Vec<&Transaction>>) = (vec![], vec![]);
        let mut after_edit = false;
        for e in &self.log {
            match e {
                Event::Edit(t) => {
                    t.ops.iter().for_each(&mut f);
                    let coalesce = coalesces(after_edit, undo.last().and_then(|g| g.last().copied()), t);
                    redo.clear();
                    match undo.last_mut() {
                        Some(g) if coalesce => g.push(t),
                        _ => undo.push(vec![t]),
                    }
                }
                Event::Undo => {
                    if let Some(g) = undo.pop() {
                        g.iter().rev().flat_map(|t| t.ops.iter().rev()).for_each(|op| f(&op.inverse()));
                        redo.push(g);
                    }
                }
                Event::Redo => {
                    if let Some(g) = redo.pop() {
                        g.iter().flat_map(|t| &t.ops).for_each(&mut f);
                        undo.push(g);
                    }
                }
                Event::MergeUndo { depth } => {
                    if undo.len() > depth + 1 {
                        let merged: Vec<&Transaction> = undo.drain(depth..).flatten().collect();
                        undo.push(merged);
                    }
                }
            }
            after_edit = matches!(e, Event::Edit(_));
        }
    }

    /// Undo the last undo group; returns the selection from before it.
    pub fn undo(&mut self) -> Option<Selection> {
        let group = self.undo.pop()?;
        for t in group.iter().rev() {
            for op in t.ops.iter().rev() {
                self.apply_op(&op.inverse()).expect("undo applies cleanly");
            }
        }
        let sel = group[0].before;
        self.redo.push(group);
        self.log.push(Event::Undo);
        Some(sel)
    }

    /// Redo the last undone group; returns the selection from after it.
    pub fn redo(&mut self) -> Option<Selection> {
        let group = self.redo.pop()?;
        for t in &group {
            for op in &t.ops {
                self.apply_op(op).expect("redo applies cleanly");
            }
        }
        let sel = group.last().expect("groups are non-empty").after;
        self.undo.push(group);
        self.log.push(Event::Redo);
        Some(sel)
    }

    // ---- internals ---------------------------------------------------------

    /// Run `f` with the [`Edit`] builder as one transaction (one undo step,
    /// unless `group` coalesces it with its neighbours). `before` is the
    /// selection undo returns to; `f` returns the selection to show after
    /// (which redo returns to) and a value for the caller. On error every
    /// applied op is rolled back, no fresh id is spent and nothing is logged.
    /// A transaction with no ops is not logged either.
    pub fn edit<T>(
        &mut self,
        before: Selection,
        group: Group,
        f: impl FnOnce(&mut Edit<'_>) -> Result<(Selection, T), DocError>,
    ) -> Result<T, DocError> {
        let next_id = self.next_id;
        let mut tx = Edit { doc: self, ops: vec![] };
        let result = f(&mut tx);
        let ops = std::mem::take(&mut tx.ops);
        match result {
            Ok((after, value)) => {
                if !ops.is_empty() {
                    self.record_edit(Transaction { ops, before, after, group });
                }
                Ok(value)
            }
            Err(e) => {
                for op in ops.iter().rev() {
                    self.apply_op(&op.inverse()).expect("rollback applies cleanly");
                }
                self.next_id = next_id;
                Err(e)
            }
        }
    }

    fn transact(
        &mut self,
        before: Selection,
        group: Group,
        f: impl FnOnce(&mut Edit<'_>) -> Result<Selection, DocError>,
    ) -> Result<Selection, DocError> {
        self.edit(before, group, |tx| f(tx).map(|after| (after, after)))
    }

    fn record_edit(&mut self, t: Transaction) {
        let coalesce =
            coalesces(matches!(self.log.last(), Some(Event::Edit(_))), self.undo.last().and_then(|g| g.last()), &t);
        self.log.push(Event::Edit(t.clone()));
        self.redo.clear();
        match self.undo.last_mut() {
            Some(g) if coalesce => g.push(t),
            _ => self.undo.push(vec![t]),
        }
    }

    fn apply_op(&mut self, op: &Op) -> Result<(), DocError> {
        let paras = &mut self.script.paragraphs;
        let idx = |paras: &[Paragraph], id: ParaId| {
            paras.iter().position(|p| p.id == id).ok_or(DocError::MissingParagraph(id))
        };
        match op {
            Op::Splice { para, at, removed, inserted } => {
                let i = idx(paras, *para)?;
                let end = at + spans::len(removed);
                if end > spans::len(&paras[i].spans) || spans::slice(&paras[i].spans, *at..end) != *removed {
                    return Err(DocError::Mismatch("splice: removed text differs"));
                }
                spans::splice(&mut paras[i].spans, *at..end, inserted);
            }
            Op::Split { para, at, new_para, new_kind } => {
                let i = idx(paras, *para)?;
                let len = spans::len(&paras[i].spans);
                if *at > len || paras.iter().any(|p| p.id == *new_para) {
                    return Err(DocError::Mismatch("split: bad offset or id in use"));
                }
                let tail = spans::slice(&paras[i].spans, *at..len);
                paras[i].spans = spans::slice(&paras[i].spans, 0..*at);
                paras.insert(i + 1, Paragraph { id: *new_para, kind: new_kind.clone(), spans: tail });
            }
            Op::Merge { para, at, next, next_kind } => {
                let i = idx(paras, *para)?;
                let ok = paras.get(i + 1).is_some_and(|n| n.id == *next && n.kind == *next_kind)
                    && spans::len(&paras[i].spans) == *at;
                if !ok {
                    return Err(DocError::Mismatch("merge: paragraphs not adjacent or changed"));
                }
                let tail = paras.remove(i + 1).spans;
                paras[i].spans.extend(tail);
                spans::normalize(&mut paras[i].spans);
            }
            Op::SetKind { para, from, to } => {
                let i = idx(paras, *para)?;
                if paras[i].kind != *from {
                    return Err(DocError::Mismatch("set kind: kind differs"));
                }
                paras[i].kind = to.clone();
            }
            Op::InsertPara { index, para } => {
                if *index > paras.len() || paras.iter().any(|p| p.id == para.id) {
                    return Err(DocError::Mismatch("insert paragraph: bad index or id in use"));
                }
                paras.insert(*index, para.clone());
            }
            Op::RemovePara { index, para } => {
                if paras.get(*index) != Some(para) {
                    return Err(DocError::Mismatch("remove paragraph: differs from the document"));
                }
                if paras.len() == 1 {
                    return Err(DocError::LastParagraph);
                }
                paras.remove(*index);
            }
            Op::SetMeta { key, from, to } => {
                if self.meta.get(key) != from.as_ref() {
                    return Err(DocError::Mismatch("set meta: value differs"));
                }
                match to {
                    Some(v) => self.meta.insert(key.clone(), v.clone()),
                    None => self.meta.remove(key),
                };
            }
        }
        Ok(())
    }
}

/// A paragraph to add: its id comes from the document ([`Edit::insert_para`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewPara {
    pub kind: ElementKind,
    pub spans: Vec<Span>,
}

impl NewPara {
    /// A paragraph of plain, unstyled text.
    pub fn plain(kind: ElementKind, text: impl Into<String>) -> Self {
        Self { kind, spans: vec![Span { text: text.into(), style: Default::default() }] }
    }
}

/// The transaction builder handed out by [`Document::edit`]. Each primitive
/// applies its op at once, so later steps see earlier ones; if the closure
/// fails, the whole transaction is rolled back.
pub struct Edit<'a> {
    doc: &'a mut Document,
    ops: Vec<Op>,
}

impl Edit<'_> {
    /// The document as edited so far in this transaction.
    pub fn doc(&self) -> &Document {
        self.doc
    }

    /// Replace bytes `range` of paragraph `para`'s text with `spans`. The range
    /// must lie within the text, on char boundaries. Allowed on fixed
    /// paragraphs. A splice that changes nothing records nothing.
    pub fn splice(&mut self, para: ParaId, range: Range<usize>, mut spans: Vec<Span>) -> Result<(), DocError> {
        let p = self.para(para)?;
        let text = p.text();
        let ok = range.start <= range.end
            && range.end <= text.len()
            && text.is_char_boundary(range.start)
            && text.is_char_boundary(range.end);
        if !ok {
            return Err(DocError::Mismatch("splice: range outside the text or not on a char boundary"));
        }
        spans::normalize(&mut spans);
        let removed = spans::slice(&p.spans, range.clone());
        if removed == spans {
            return Ok(());
        }
        self.push(Op::Splice { para, at: range.start, removed, inserted: spans })
    }

    /// Add paragraph `p` at `index` (0 = first, `len` = last) with a fresh id,
    /// which it returns. Refused between two paragraphs of the same fixed group.
    pub fn insert_para(&mut self, index: usize, p: NewPara) -> Result<ParaId, DocError> {
        self.check_insert(index)?;
        let id = self.doc.fresh_id();
        self.push_insert(index, id, p)?;
        Ok(id)
    }

    /// Like [`Edit::insert_para`], but bring back id `id`, which was used before
    /// and is absent now (else `Mismatch`). This is the only way an id is ever
    /// reused: it exists for restoring removed lines (`restore_run`: keep mine /
    /// undecide after a block suggestion was accepted). Every other insert takes
    /// a fresh id, and undo/redo replay the inverse ops directly, so they never
    /// allocate ids.
    pub fn insert_para_with_id(&mut self, index: usize, id: ParaId, p: NewPara) -> Result<(), DocError> {
        if id.0 >= self.doc.next_id || self.doc.paragraph(id).is_some() {
            return Err(DocError::Mismatch("insert paragraph with id: the id is live or was never used"));
        }
        self.check_insert(index)?;
        self.push_insert(index, id, p)
    }

    /// Remove paragraph `id`. Refused for a fixed paragraph, and for the last
    /// paragraph ([`DocError::LastParagraph`]): a document is never empty, so
    /// callers insert the new paragraphs before removing the old ones for a
    /// full replace.
    pub fn remove_para(&mut self, id: ParaId) -> Result<(), DocError> {
        self.refuse_fixed(id)?;
        let index = self.index(id)?;
        let para = self.doc.script.paragraphs[index].clone();
        self.push(Op::RemovePara { index, para })
    }

    /// Give paragraph `id` the element `kind`. Refused for a fixed paragraph,
    /// unless it already is that kind (then nothing is recorded).
    pub fn set_kind(&mut self, id: ParaId, kind: ElementKind) -> Result<(), DocError> {
        let from = self.para(id)?.kind.clone();
        if from == kind {
            return Ok(());
        }
        self.refuse_fixed(id)?;
        self.push(Op::SetKind { para: id, from, to: kind })
    }

    /// Split at `at` as Enter does, returning the new paragraph's id: at offset
    /// 0 of a non-empty paragraph a new empty paragraph of the same kind goes
    /// above; at the end a new empty one of [`rules::next_kind`] goes below; in
    /// the middle the original keeps the head and the tail (same kind) gets
    /// the new id. So the original id stays with the original text. Refused
    /// for a fixed paragraph.
    pub fn split(&mut self, at: Pos) -> Result<ParaId, DocError> {
        let p = self.para(at.para)?;
        let (len, kind) = (spans::len(&p.spans), p.kind.clone());
        if at.offset > len || !p.text().is_char_boundary(at.offset) {
            return Err(DocError::Mismatch("split: offset outside the text or not on a char boundary"));
        }
        self.refuse_fixed(at.para)?;
        let new_id = self.doc.fresh_id();
        if at.offset == 0 && len > 0 {
            let index = self.index(at.para)?;
            self.push(Op::InsertPara { index, para: Paragraph { id: new_id, kind, spans: vec![] } })?;
        } else {
            let new_kind = if at.offset == len { rules::next_kind(&kind) } else { kind };
            self.push(Op::Split { para: at.para, at: at.offset, new_para: new_id, new_kind })?;
        }
        Ok(new_id)
    }

    /// Join `second` onto `first`, which it must directly follow. An empty
    /// paragraph is removed instead, so the paragraph with text keeps its id
    /// and kind. Returns the join point. Refused if a fixed paragraph would be
    /// removed or merged; removing an empty plain neighbour of a fixed one is
    /// allowed.
    pub fn join(&mut self, first: ParaId, second: ParaId) -> Result<Pos, DocError> {
        let (p1, p2) = (self.para(first)?.clone(), self.para(second)?.clone());
        let i1 = self.index(first)?;
        if self.index(second)? != i1 + 1 {
            return Err(DocError::Mismatch("join: paragraphs not adjacent"));
        }
        if p1.spans.is_empty() && !p2.spans.is_empty() {
            self.refuse_fixed(first)?;
            self.push(Op::RemovePara { index: i1, para: p1 })?;
            return Ok(Pos::new(second, 0));
        }
        let at = spans::len(&p1.spans);
        self.refuse_fixed(second)?;
        if p2.spans.is_empty() {
            self.push(Op::RemovePara { index: i1 + 1, para: p2 })?;
        } else {
            self.refuse_fixed(first)?;
            self.push(Op::Merge { para: first, at, next: second, next_kind: p2.kind })?;
        }
        Ok(Pos::new(first, at))
    }

    /// Set metadata `key` to `to` (`None` removes it). Records nothing if it
    /// already has that value.
    pub fn set_meta(&mut self, key: &str, to: Option<&str>) -> Result<(), DocError> {
        if self.doc.meta(key) == to {
            return Ok(());
        }
        let from = self.doc.meta.get(key).cloned();
        self.push(Op::SetMeta { key: key.to_string(), from, to: to.map(str::to_string) })
    }

    // ---- internals ---------------------------------------------------------

    fn push(&mut self, op: Op) -> Result<(), DocError> {
        self.doc.apply_op(&op)?;
        self.ops.push(op);
        Ok(())
    }

    fn push_insert(&mut self, index: usize, id: ParaId, p: NewPara) -> Result<(), DocError> {
        let mut spans = p.spans;
        spans::normalize(&mut spans);
        self.push(Op::InsertPara { index, para: Paragraph { id, kind: p.kind, spans } })
    }

    fn refuse_fixed(&self, id: ParaId) -> Result<(), DocError> {
        if self.doc.script.is_fixed(id) { Err(DocError::Fixed(id)) } else { Ok(()) }
    }

    /// A paragraph may go at `index` unless that puts it inside a fixed group
    /// (between two of its paragraphs); between two different groups is fine.
    fn check_insert(&self, index: usize) -> Result<(), DocError> {
        let script = &self.doc.script;
        let paras = &script.paragraphs;
        if index > paras.len() {
            return Err(DocError::Mismatch("insert paragraph: index past the end"));
        }
        if index > 0 && index < paras.len() {
            let (above, below) = (paras[index - 1].id, paras[index].id);
            if let (Some(a), Some(b)) = (script.fixed_group(above), script.fixed_group(below))
                && a == b
            {
                return Err(DocError::Fixed(below));
            }
        }
        Ok(())
    }

    fn para(&self, id: ParaId) -> Result<&Paragraph, DocError> {
        self.doc.paragraph(id).ok_or(DocError::MissingParagraph(id))
    }

    fn index(&self, id: ParaId) -> Result<usize, DocError> {
        self.doc.index_of(id).ok_or(DocError::MissingParagraph(id))
    }

    /// Delete the selection. Across paragraphs this records no empty splices
    /// (older builds did; `next_undo_paras` now counts one paragraph there).
    pub(crate) fn delete_selection(&mut self, sel: Selection) -> Result<Pos, DocError> {
        let (a, b) = self.doc.ordered(sel);
        self.delete_range(a, b)
    }

    /// Delete from `a` to `b` (document order). Returns `a`.
    fn delete_range(&mut self, a: Pos, b: Pos) -> Result<Pos, DocError> {
        if a == b {
            return Ok(a);
        }
        if a.para == b.para {
            self.splice(a.para, a.offset..b.offset, vec![])?;
            return Ok(a);
        }
        self.delete_between(a, b)?;
        self.join(a.para, b.para)
    }

    /// Delete from `a` to `b` (document order, different paragraphs) without
    /// joining: `a`'s paragraph keeps its text before `a`, `b`'s its text
    /// after `b`, and the paragraphs between are removed. Both keep their ids
    /// and kinds.
    pub(crate) fn delete_between(&mut self, a: Pos, b: Pos) -> Result<(), DocError> {
        let len_a = spans::len(&self.para(a.para)?.spans);
        self.splice(a.para, a.offset..len_a, vec![])?;
        self.splice(b.para, 0..b.offset, vec![])?;
        let ia = self.index(a.para)?;
        while let Some(id) = self.doc.script.paragraphs.get(ia + 1).map(|p| p.id).filter(|id| *id != b.para) {
            self.remove_para(id)?;
        }
        Ok(())
    }

    fn insert_plain(&mut self, pos: Pos, text: &str) -> Result<Pos, DocError> {
        if text.is_empty() {
            return Ok(pos);
        }
        let style = spans::style_at(&self.para(pos.para)?.spans, pos.offset);
        self.splice(pos.para, pos.offset..pos.offset, vec![Span { text: text.to_string(), style }])?;
        Ok(Pos::new(pos.para, pos.offset + text.len()))
    }

    /// Enter at `pos` ([`Edit::split`]); returns where the cursor goes.
    fn enter_at(&mut self, pos: Pos) -> Result<Pos, DocError> {
        let above = pos.offset == 0 && self.doc.para_len(pos.para) > 0;
        let new_id = self.split(pos)?;
        Ok(if above { pos } else { Pos::new(new_id, 0) })
    }
}

/// Lengths in bytes of the common prefix and (non-overlapping) common suffix
/// of `a` and `b`, on char boundaries.
fn common_ends(a: &str, b: &str) -> (usize, usize) {
    let pre: usize = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).map(|(x, _)| x.len_utf8()).sum();
    let suf: usize =
        a[pre..].chars().rev().zip(b[pre..].chars().rev()).take_while(|(x, y)| x == y).map(|(x, _)| x.len_utf8()).sum();
    (pre, suf)
}

fn inserted_text(t: &Transaction) -> String {
    t.ops
        .iter()
        .filter_map(|op| match op {
            Op::Splice { inserted, .. } => Some(spans::text(inserted)),
            _ => None,
        })
        .collect()
}

/// Whether edit `t`, logged right after an edit (`after_edit`) or not,
/// joins the undo group whose last transaction is `prev`.
fn coalesces(after_edit: bool, prev: Option<&Transaction>, t: &Transaction) -> bool {
    after_edit
        && t.group != Group::Other
        && prev.is_some_and(|prev| {
            prev.group == t.group
                && prev.after == t.before
                && !(t.group == Group::Typing && starts_word(t) && !ends_with_space(prev))
        })
}

fn starts_word(t: &Transaction) -> bool {
    inserted_text(t).chars().next().is_some_and(char::is_whitespace)
}

fn ends_with_space(t: &Transaction) -> bool {
    inserted_text(t).chars().last().is_some_and(char::is_whitespace)
}

fn prev_grapheme(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true).rev().find_map(|(i, _)| (i < offset).then_some(i)).unwrap_or(0)
}

fn next_grapheme(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true).find_map(|(i, _)| (i > offset).then_some(i)).unwrap_or(text.len())
}

fn is_word(seg: &str) -> bool {
    seg.chars().any(char::is_alphanumeric)
}

fn prev_word(text: &str, offset: usize) -> usize {
    text.split_word_bound_indices().rev().find(|(i, s)| *i < offset && is_word(s)).map_or(0, |(i, _)| i)
}

fn next_word(text: &str, offset: usize) -> usize {
    text.split_word_bound_indices()
        .find(|(i, s)| i + s.len() > offset && is_word(s))
        .map_or(text.len(), |(i, s)| i + s.len())
}
