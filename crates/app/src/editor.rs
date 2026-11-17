//! The script editor view: holds a `scriptorium_core::Document` and turns
//! keyboard, mouse and IME input into document commands.
//!
//! The document owns all content, undo/redo and paragraph ids. This view owns
//! only presentation state: the selection, IME composition, scroll position and
//! the layouts painted last frame (used for hit-testing and up/down movement).
//!
//! The window around it ([`crate::workspace`]) runs review decisions through
//! [`ScriptEditor::run`], sets [`Decor`] (current line, gutter marks), and
//! listens for [`EditorEvent`]s. Editing is structural everywhere: Enter
//! splits, Backspace/Delete at an edge join, Tab retypes, a selection across
//! paragraphs deletes as one; edits [`scriptorium_core::Script::fixed`]
//! paragraphs (dual dialogue) can't take are refused with a notice. Copy puts
//! typed paragraphs beside the plain text ([`ClipboardMeta`]), so a paste back
//! keeps kinds; plain text from elsewhere is classified
//! ([`scriptorium_core::paste`]).

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, Bounds, ClipboardEntry, ClipboardItem, Context, CursorStyle, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, IntoElement, ListAlignment, ListOffset, ListState, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, Render, Styled, UTF16Selection, Window, WrappedLine, actions, div,
    list, point, prelude::*, px, size,
};
use scriptorium_core::doc::{DocError, Document};
use scriptorium_core::paste::{self, ClipboardMeta, Pasted};
use scriptorium_core::{FIXED_MESSAGE, ParaId, Paragraph, Pos, Selection};

use crate::elements::{InputArea, ParaElement};
use crate::layout;
use crate::links_view::Columns;

actions!(
    editor,
    [
        Backspace,
        Delete,
        DeleteWordLeft,
        DeleteWordRight,
        Left,
        Right,
        Up,
        Down,
        WordLeft,
        WordRight,
        Home,
        End,
        DocStart,
        DocEnd,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectWordLeft,
        SelectWordRight,
        SelectHome,
        SelectEnd,
        SelectAll,
        PageUp,
        PageDown,
        Enter,
        CommitLine,
        Tab,
        ShiftTab,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
        Escape,
    ]
);

/// What the window paints over the script: the line under review and a mark
/// in the left margin for each suggestion of the review being shown.
#[derive(Clone, Default)]
pub struct Decor {
    /// The lines of the suggestion on the card (a block: every line it covers).
    pub current: HashSet<ParaId>,
    pub marks: HashMap<ParaId, Mark>,
    /// Paragraphs as opened from the file, to put Final Draft's `*` beside changed lines.
    pub originals: Rc<HashMap<ParaId, Paragraph>>,
    /// Hints shown dimmed in blank lines: on a version, the original's line
    /// of the row (what is left to translate).
    pub ghosts: Rc<HashMap<ParaId, String>>,
    /// The other member of the script's pair lined up on the left, row by
    /// row: each paragraph's band. `None`: one column.
    pub columns: Option<Rc<Columns>>,
}

/// Whether `p` differs from the file: new, retyped, or with other text.
/// Nothing is changed before the originals are known (an empty map).
pub fn is_changed(originals: &HashMap<ParaId, Paragraph>, p: &Paragraph) -> bool {
    !originals.is_empty() && originals.get(&p.id).is_none_or(|o| o.kind != p.kind || o.text() != p.text())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Pending,
    /// Pending, but the line has changed since the suggestion was written.
    Outdated,
    Accepted,
    Kept,
    Rewritten,
}

pub enum EditorEvent {
    /// The document changed (typing, a decision, undo…).
    Edited,
    /// The user clicked in a paragraph.
    Clicked(ParaId),
    Escape,
    /// Cmd/Ctrl+Enter: "I'm done with this line".
    Commit,
    /// Enter split a paragraph (the window may explain the key, once).
    Split,
    /// Something the user should be told (e.g. why a key did nothing).
    Notice(String),
}

impl EventEmitter<EditorEvent> for ScriptEditor {}

/// Said when a join would take away the line being rewritten.
pub(crate) const PROTECTED_JOIN: &str =
    "That would join away the line you're rewriting. Finish or cancel the rewrite first.";

/// What to tell the writer about a refused edit.
pub(crate) fn refusal(e: &DocError) -> String {
    match e {
        DocError::Fixed(_) => FIXED_MESSAGE.to_string(),
        e => format!("edit failed: {e}"),
    }
}

/// Text being composed by an IME or a dead key, shown inline but not yet in the document.
#[derive(Debug, Clone)]
pub struct Marked {
    pub pos: Pos,
    pub text: String,
    /// Cursor position within `text`, in bytes.
    pub cursor: usize,
}

/// Where a paragraph was painted last frame, in window coordinates.
#[derive(Clone)]
pub struct ParaLayout {
    pub top: Pixels,
    pub bottom: Pixels,
    pub text_origin: Point<Pixels>,
    /// Laid out from the *displayed* text (document text plus any IME composition).
    pub line: Option<Rc<WrappedLine>>,
}

impl ParaLayout {
    fn text_height(&self) -> Pixels {
        self.line
            .as_ref()
            .map_or(layout::line_height(), |l| l.size(layout::line_height()).height.max(layout::line_height()))
    }

    /// Where the cursor for `index` goes, relative to `text_origin`. At a soft
    /// wrap, the cursor belongs at the start of the next line (GPUI's
    /// `position_for_index` reports the end of the previous one).
    pub(crate) fn position_for_index(&self, index: usize) -> Point<Pixels> {
        let lh = layout::line_height();
        let Some(line) = &self.line else { return Point::default() };
        for (row, b) in line.wrap_boundaries().iter().enumerate() {
            if line.runs()[b.run_ix].glyphs[b.glyph_ix].index == index {
                return point(px(0.), lh * (row + 1) as f32);
            }
        }
        line.position_for_index(index, lh).unwrap_or_default()
    }

    fn index_for_local(&self, local: Point<Pixels>) -> usize {
        let Some(line) = &self.line else { return 0 };
        let y = local.y.clamp(px(0.), self.text_height() - px(1.));
        let x = local.x.max(px(0.));
        match line.closest_index_for_position(point(x, y), layout::line_height()) {
            Ok(i) | Err(i) => i.min(line.len()),
        }
    }
}

pub struct ScriptEditor {
    pub(crate) doc: Document,
    pub(crate) sel: Selection,
    pub(crate) marked: Option<Marked>,
    pub(crate) focus: FocusHandle,
    pub(crate) layouts: HashMap<ParaId, ParaLayout>,
    list: ListState,
    /// The paragraphs the list was last told about, to compute minimal list updates.
    shown: Vec<Paragraph>,
    goal_x: Option<Pixels>,
    selecting: bool,
    pub decor: Decor,
    /// The line under review while it's being rewritten (E): a join that
    /// would remove it or merge it into another line is refused.
    pub protect: Option<ParaId>,
    /// Where the paragraph list was painted last frame.
    pub(crate) viewport: Bounds<Pixels>,
}

impl ScriptEditor {
    pub fn new(doc: Document, cx: &mut Context<Self>) -> Self {
        let sel = Selection::cursor(doc.start());
        let n = doc.paragraphs().len();
        Self {
            shown: doc.paragraphs().to_vec(),
            doc,
            sel,
            marked: None,
            focus: cx.focus_handle(),
            layouts: HashMap::new(),
            list: ListState::new(n, ListAlignment::Top, px(800.)),
            goal_x: None,
            selecting: false,
            decor: Decor::default(),
            protect: None,
            viewport: Bounds::default(),
        }
    }

    /// Run a document command from outside (a review decision, undo). Unlike
    /// typing, it doesn't scroll to the cursor: the caller decides what to show.
    pub fn run(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Document, Selection) -> Result<Selection, DocError>,
    ) {
        self.apply(cx, false, f);
    }

    /// Put the cursor at the end of paragraph `para`.
    pub fn cursor_to_end(&mut self, para: ParaId, cx: &mut Context<Self>) {
        let len = self.doc.paragraph(para).map_or(0, |p| p.text().len());
        self.move_to(Pos::new(para, len), false, cx);
    }

    /// Scroll so `para` is comfortably in view (a few paragraphs of context
    /// above it), unless it already is.
    pub fn reveal(&mut self, para: ParaId, cx: &mut Context<Self>) {
        let Some(ix) = self.doc.index_of(para) else { return };
        let v = self.viewport;
        let comfortable = self.layouts.get(&para).is_some_and(|l| {
            v.size.height > px(0.) && l.top >= v.top() + px(60.) && l.bottom <= v.bottom() - v.size.height * 0.35
        });
        if !comfortable {
            self.list.scroll_to(ListOffset { item_ix: ix.saturating_sub(3), offset_in_item: px(0.) });
        }
        cx.notify();
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    // ---- applying changes --------------------------------------------------

    /// Run a document command, then update selection, list and view.
    /// Returns whether the document changed.
    fn edit(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Document, Selection) -> Result<Selection, DocError>,
    ) -> bool {
        self.apply(cx, true, f)
    }

    fn apply(
        &mut self,
        cx: &mut Context<Self>,
        reveal: bool,
        f: impl FnOnce(&mut Document, Selection) -> Result<Selection, DocError>,
    ) -> bool {
        self.marked = None;
        let before = self.doc.log().len();
        match f(&mut self.doc, self.sel) {
            Ok(sel) => self.sel = sel,
            Err(e) => cx.emit(EditorEvent::Notice(refusal(&e))),
        }
        self.sync_list();
        self.goal_x = None;
        if reveal {
            self.reveal_head();
        }
        let changed = self.doc.log().len() != before;
        if changed {
            cx.emit(EditorEvent::Edited);
        }
        cx.notify();
        changed
    }

    /// Set what is painted over the script. Two columns shown or hidden, or
    /// a pair loaded again, measure every paragraph again; rows laid out
    /// again measure only the paragraphs whose band changed (with the rest
    /// of their row's run, which the left cell flows over).
    pub fn set_decor(&mut self, decor: Decor) {
        match (&self.decor.columns, &decor.columns) {
            (None, None) => {}
            (Some(a), Some(b)) if Rc::ptr_eq(a, b) => {}
            (Some(a), Some(b)) if Rc::ptr_eq(&a.left, &b.left) => {
                let changed: Vec<usize> = (self.doc.paragraphs().iter().enumerate())
                    .filter(|(_, p)| a.bands.get(&p.id) != b.bands.get(&p.id))
                    .map(|(k, _)| k)
                    .collect();
                for range in runs_of(self.doc.paragraphs(), b, changed) {
                    self.list.remeasure_items(range);
                }
            }
            _ => self.list.remeasure(),
        }
        self.decor = decor;
    }

    /// Tell the list which items changed, by comparing with what it showed before.
    fn sync_list(&mut self) {
        let new = self.doc.paragraphs();
        let old = &self.shown;
        let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
        let max_suffix = old.len().min(new.len()) - prefix;
        let suffix = old.iter().rev().zip(new.iter().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
        if prefix + suffix < old.len().max(new.len()) || old.len() != new.len() {
            self.list.splice(prefix..old.len() - suffix, new.len() - prefix - suffix);
            // In two columns a paragraph's height moves the left cell for
            // the rest of its row's run: measure those again too. (Bands
            // for new paragraphs come with the next `set_decor`.)
            if let Some(c) = &self.decor.columns {
                for range in runs_of(new, c, prefix..new.len() - suffix) {
                    self.list.remeasure_items(range);
                }
            }
        }
        self.shown = new.to_vec();
    }

    fn remeasure_head(&mut self) {
        if let Some(ix) = self.doc.index_of(self.sel.head.para) {
            self.list.splice(ix..ix + 1, 1);
            // A composition's height moves the left cell for the rest of the
            // head's row's run.
            if let Some(c) = &self.decor.columns {
                for range in runs_of(self.doc.paragraphs(), c, [ix]) {
                    self.list.remeasure_items(range);
                }
            }
        }
    }

    /// Whether `position` is on the left column (the script lined up there,
    /// read-only) or the gutter.
    fn on_left(&self, position: Point<Pixels>) -> bool {
        self.decor.columns.is_some()
            && position.x < self.viewport.left() + px(layout::two_columns(f32::from(self.viewport.size.width)).right_x)
    }

    fn reveal_head(&self) {
        if let Some(ix) = self.doc.index_of(self.sel.head.para) {
            self.list.scroll_to_reveal_item(ix);
        }
    }

    fn move_to(&mut self, pos: Pos, extend: bool, cx: &mut Context<Self>) {
        if self.marked.take().is_some() {
            self.remeasure_head();
        }
        self.sel = if extend { Selection { anchor: self.sel.anchor, head: pos } } else { Selection::cursor(pos) };
        self.reveal_head();
        cx.notify();
    }

    // ---- hit testing and vertical movement ---------------------------------

    fn hit_test(&self, position: Point<Pixels>) -> Option<Pos> {
        let (id, l) = self.layouts.iter().min_by_key(|(_, l)| {
            let d = if position.y < l.top {
                l.top - position.y
            } else if position.y >= l.bottom {
                position.y - l.bottom
            } else {
                px(0.)
            };
            (f32::from(d) * 100.0) as i64
        })?;
        let local = position - l.text_origin;
        Some(self.doc.clamp(Pos::new(*id, l.index_for_local(local))))
    }

    /// One visual line up (`dir` = -1) or down (+1), keeping the horizontal goal.
    fn vertical(&mut self, dir: i32) -> Pos {
        let lh = layout::line_height();
        let head = self.sel.head;
        let Some(l) = self.layouts.get(&head.para) else { return head };
        let p = l.position_for_index(head.offset);
        let goal = *self.goal_x.get_or_insert(l.text_origin.x + p.x);
        let target_y = p.y + lh / 2. + lh * dir as f32;
        if target_y >= px(0.) && target_y < l.text_height() {
            let ix = l.index_for_local(point(goal - l.text_origin.x, target_y));
            return self.doc.clamp(Pos::new(head.para, ix));
        }
        let Some(ix) = self.doc.index_of(head.para) else { return head };
        let neighbour = if dir < 0 { ix.checked_sub(1) } else { Some(ix + 1) };
        let Some(next) = neighbour.and_then(|i| self.doc.paragraphs().get(i)) else {
            return if dir < 0 { self.doc.start() } else { self.doc.end() };
        };
        let next_id = next.id;
        let next_len = next.text().len();
        match self.layouts.get(&next_id) {
            Some(nl) => {
                let y = if dir < 0 { nl.text_height() - lh / 2. } else { lh / 2. };
                self.doc.clamp(Pos::new(next_id, nl.index_for_local(point(goal - nl.text_origin.x, y))))
            }
            None => Pos::new(next_id, if dir < 0 { next_len } else { 0 }),
        }
    }

    // ---- actions -------------------------------------------------------------

    fn delete_dir(&mut self, forward: bool, word: bool, cx: &mut Context<Self>) {
        if self.protect.is_some() && self.doc.removed_by_join(self.sel, forward) == self.protect {
            cx.emit(EditorEvent::Notice(PROTECTED_JOIN.into()));
            return;
        }
        if forward {
            self.edit(cx, |d, s| d.delete_forward(s, word));
        } else {
            self.edit(cx, |d, s| d.delete_backward(s, word));
        }
    }
    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        self.delete_dir(false, false, cx);
    }
    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        self.delete_dir(true, false, cx);
    }
    fn delete_word_left(&mut self, _: &DeleteWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.delete_dir(false, true, cx);
    }
    fn delete_word_right(&mut self, _: &DeleteWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.delete_dir(true, true, cx);
    }
    fn enter(&mut self, _: &Enter, _: &mut Window, cx: &mut Context<Self>) {
        if self.edit(cx, |d, s| d.enter(s)) {
            cx.emit(EditorEvent::Split);
        }
    }
    fn commit_line(&mut self, _: &CommitLine, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(EditorEvent::Commit);
    }
    fn escape(&mut self, _: &Escape, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(EditorEvent::Escape);
    }
    fn tab(&mut self, _: &Tab, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, |d, s| d.cycle_kind(s, true));
    }
    fn shift_tab(&mut self, _: &ShiftTab, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, |d, s| d.cycle_kind(s, false));
    }
    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, |d, s| Ok(d.undo().unwrap_or(s)));
    }
    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        self.edit(cx, |d, s| Ok(d.redo().unwrap_or(s)));
    }

    fn horizontal(&mut self, right: bool, word: bool, extend: bool, cx: &mut Context<Self>) {
        self.goal_x = None;
        let (a, b) = self.doc.ordered(self.sel);
        let pos = if !extend && !self.sel.is_collapsed() && !word {
            if right { b } else { a }
        } else if right {
            self.doc.move_right(self.sel.head, word)
        } else {
            self.doc.move_left(self.sel.head, word)
        };
        self.move_to(pos, extend, cx);
    }
    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.horizontal(false, false, false, cx);
    }
    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.horizontal(true, false, false, cx);
    }
    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.horizontal(false, true, false, cx);
    }
    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.horizontal(true, true, false, cx);
    }
    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.horizontal(false, false, true, cx);
    }
    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.horizontal(true, false, true, cx);
    }
    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.horizontal(false, true, true, cx);
    }
    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.horizontal(true, true, true, cx);
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        let goal = self.goal_x;
        let pos = self.vertical(-1);
        self.move_to(pos, false, cx);
        self.goal_x = self.goal_x.or(goal);
    }
    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        let pos = self.vertical(1);
        self.move_to(pos, false, cx);
    }
    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        let pos = self.vertical(-1);
        self.move_to(pos, true, cx);
    }
    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        let pos = self.vertical(1);
        self.move_to(pos, true, cx);
    }

    fn para_edge(&self, end: bool) -> Pos {
        let id = self.sel.head.para;
        Pos::new(id, if end { self.doc.paragraph(id).map_or(0, |p| p.text().len()) } else { 0 })
    }
    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.move_to(self.para_edge(false), false, cx);
    }
    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.move_to(self.para_edge(true), false, cx);
    }
    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.para_edge(false), true, cx);
    }
    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.para_edge(true), true, cx);
    }
    fn doc_start(&mut self, _: &DocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.doc.start(), false, cx);
    }
    fn doc_end(&mut self, _: &DocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.doc.end(), false, cx);
    }
    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.sel = Selection { anchor: self.doc.start(), head: self.doc.end() };
        cx.notify();
    }
    fn page(&mut self, down: bool, window: &mut Window, cx: &mut Context<Self>) {
        let h = window.viewport_size().height * 0.85;
        self.list.scroll_by(if down { h } else { -h });
        cx.notify();
    }
    fn page_up(&mut self, _: &PageUp, window: &mut Window, cx: &mut Context<Self>) {
        self.page(false, window, cx);
    }
    fn page_down(&mut self, _: &PageDown, window: &mut Window, cx: &mut Context<Self>) {
        self.page(true, window, cx);
    }

    /// Plain text for other programs (Final Draft takes it untyped), and the
    /// typed paragraphs as JSON metadata for a paste back into Scriptorium.
    fn copy_selection(&self, cx: &mut Context<Self>) {
        let paras = self.doc.fragment(self.sel);
        let text = paste::plain_text(&paras);
        cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(text, ClipboardMeta::new(paras)));
    }
    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.sel.is_collapsed() {
            self.copy_selection(cx);
        }
    }
    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if !self.sel.is_collapsed() {
            self.copy_selection(cx);
            self.edit(cx, |d, s| d.delete_backward(s, false));
        }
    }
    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else { return };
        let Some(text) = item.text() else { return };
        let meta = match item.entries() {
            [ClipboardEntry::String(s)] => s.metadata_json::<ClipboardMeta>(),
            _ => None,
        };
        // `for_clipboard` drops characters a script file can't hold.
        match paste::for_clipboard(&text, meta) {
            Pasted::Text(t) if t.is_empty() => {}
            Pasted::Text(t) => {
                self.edit(cx, |d, s| d.insert_text(s, &t));
            }
            Pasted::Paras(paras) => {
                self.edit(cx, |d, s| d.paste_fragment(s, &paras));
            }
        }
    }

    // ---- mouse -------------------------------------------------------------

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let Some(pos) = self.hit_test(event.position) else { return };
        // A click on the left only puts the cursor at the start of the row's
        // first line here: nothing there to select.
        if self.on_left(event.position) {
            let first = (self.decor.columns.as_ref())
                .and_then(|c| c.bands.get(&pos.para))
                .and_then(|b| b.above.first().copied())
                .unwrap_or(pos.para);
            cx.emit(EditorEvent::Clicked(first));
            self.goal_x = None;
            if self.marked.take().is_some() {
                self.remeasure_head();
            }
            self.sel = Selection::cursor(Pos::new(first, 0));
            self.selecting = false;
            cx.notify();
            return;
        }
        cx.emit(EditorEvent::Clicked(pos.para));
        self.goal_x = None;
        if self.marked.take().is_some() {
            self.remeasure_head();
        }
        self.sel = match event.click_count {
            2 => self.doc.word_at(pos),
            3 => Selection {
                anchor: Pos::new(pos.para, 0),
                head: Pos::new(pos.para, self.doc.paragraph(pos.para).map_or(0, |p| p.text().len())),
            },
            _ if event.modifiers.shift => Selection { anchor: self.sel.anchor, head: pos },
            _ => Selection::cursor(pos),
        };
        self.selecting = event.click_count == 1;
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting
            && event.dragging()
            && let Some(pos) = self.hit_test(event.position)
            && pos != self.sel.head
        {
            self.sel.head = pos;
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    // ---- IME helpers ---------------------------------------------------------
    //
    // The platform IME sees one paragraph at a time: the one with the cursor,
    // with any composition text included. Offsets are UTF-16 within it.

    fn ime_text(&self) -> String {
        let mut text = self.doc.paragraph(self.sel.head.para).map(|p| p.text()).unwrap_or_default();
        if let Some(m) = &self.marked {
            text.insert_str(m.pos.offset.min(text.len()), &m.text);
        }
        text
    }

    fn to_utf16(text: &str, byte: usize) -> usize {
        text[..byte.min(text.len())].encode_utf16().count()
    }

    fn from_utf16(text: &str, utf16: usize) -> usize {
        let mut units = 0;
        for (i, c) in text.char_indices() {
            if units >= utf16 {
                return i;
            }
            units += c.len_utf16();
        }
        text.len()
    }

    /// The selection within the cursor's paragraph, in bytes of `ime_text`.
    fn ime_selection(&self) -> Range<usize> {
        if let Some(m) = &self.marked {
            let c = m.pos.offset + m.cursor;
            return c..c;
        }
        let (a, h) = (self.sel.anchor, self.sel.head);
        if a.para == h.para { a.offset.min(h.offset)..a.offset.max(h.offset) } else { h.offset..h.offset }
    }
}

/// The list ranges covering the paragraphs at `changed` (positions in
/// `paras`, ascending) and, for each, the rest of its row's run in `c`.
fn runs_of(paras: &[Paragraph], c: &Columns, changed: impl IntoIterator<Item = usize>) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = vec![];
    for k in changed {
        let Some(b) = paras.get(k).and_then(|p| c.bands.get(&p.id)) else {
            out.push(k..k + 1);
            continue;
        };
        let mut end = k + 1;
        if !b.last {
            while let Some(n) = paras.get(end).and_then(|p| c.bands.get(&p.id)) {
                end += 1;
                if n.last || n.slice == 0 {
                    break;
                }
            }
        }
        let start = k.saturating_sub(b.slice);
        match out.last_mut() {
            Some(r) if r.end >= start => r.end = r.end.max(end),
            _ => out.push(start..end),
        }
    }
    out
}

impl Focusable for ScriptEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EntityInputHandler for ScriptEditor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let text = self.ime_text();
        let range = Self::from_utf16(&text, range_utf16.start)..Self::from_utf16(&text, range_utf16.end);
        adjusted.replace(Self::to_utf16(&text, range.start)..Self::to_utf16(&text, range.end));
        Some(text[range].to_string())
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        let text = self.ime_text();
        let r = self.ime_selection();
        Some(UTF16Selection {
            range: Self::to_utf16(&text, r.start)..Self::to_utf16(&text, r.end),
            reversed: self.sel.head.para == self.sel.anchor.para && self.sel.head.offset < self.sel.anchor.offset,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let m = self.marked.as_ref()?;
        let text = self.ime_text();
        Some(Self::to_utf16(&text, m.pos.offset)..Self::to_utf16(&text, m.pos.offset + m.text.len()))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        // Unmarking keeps the composed text: commit it to the document.
        if let Some(m) = self.marked.take() {
            self.edit(cx, |d, _| d.insert_text(Selection::cursor(m.pos), &m.text));
        }
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = if let Some(m) = self.marked.take() {
            Selection::cursor(m.pos)
        } else if let Some(r) = range_utf16 {
            let t = self.ime_text();
            let para = self.sel.head.para;
            Selection {
                anchor: Pos::new(para, Self::from_utf16(&t, r.start)),
                head: Pos::new(para, Self::from_utf16(&t, r.end)),
            }
        } else {
            self.sel
        };
        let text = paste::writable(text);
        if text.is_empty() && target.is_collapsed() {
            return;
        }
        self.edit(cx, move |d, _| d.insert_text(target, &text));
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Composed text a script file can't hold never reaches the document.
        let new_text = &paste::writable(new_text);
        let pos = match self.marked.take() {
            Some(m) => m.pos,
            None => {
                // Starting a composition: first remove whatever it replaces.
                let target = match range_utf16 {
                    Some(r) => {
                        let t = self.ime_text();
                        let para = self.sel.head.para;
                        Selection {
                            anchor: Pos::new(para, Self::from_utf16(&t, r.start)),
                            head: Pos::new(para, Self::from_utf16(&t, r.end)),
                        }
                    }
                    None => self.sel,
                };
                if !target.is_collapsed() {
                    self.edit(cx, |d, _| d.insert_text(target, ""));
                }
                self.doc.ordered(target).0
            }
        };
        self.sel = Selection::cursor(pos);
        self.marked = (!new_text.is_empty()).then(|| Marked {
            pos,
            text: new_text.to_string(),
            cursor: new_selected_range.map_or(new_text.len(), |r| Self::from_utf16(new_text, r.end)),
        });
        self.remeasure_head();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let l = self.layouts.get(&self.sel.head.para)?;
        let text = self.ime_text();
        let start = l.position_for_index(Self::from_utf16(&text, range_utf16.start));
        let end = l.position_for_index(Self::from_utf16(&text, range_utf16.end));
        let origin = l.text_origin + start;
        let width = if end.y == start.y { (end.x - start.x).max(px(2.)) } else { px(2.) };
        Some(Bounds::new(origin, size(width, layout::line_height())))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let pos = self.hit_test(point)?;
        (pos.para == self.sel.head.para).then(|| Self::to_utf16(&self.ime_text(), pos.offset))
    }
}

impl Render for ScriptEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = cx.entity();
        let items =
            list(self.list.clone(), move |ix, _, _| ParaElement { editor: editor.clone(), ix }.into_any_element())
                .size_full();
        // Fit the page to the space the editor has (the window minus side panels).
        let width =
            if self.viewport.size.width > px(0.) { self.viewport.size.width } else { window.viewport_size().width };
        // Two columns share one zoom, fitted to the right one.
        let width =
            if self.decor.columns.is_some() { px(layout::two_columns(f32::from(width)).right_w) } else { width };
        if layout::fit_width(width) {
            self.list.remeasure();
        }
        div()
            .key_context("ScriptEditor")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(crate::theme::DESK)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::enter))
            .on_action(cx.listener(Self::commit_line))
            .on_action(cx.listener(Self::tab))
            .on_action(cx.listener(Self::shift_tab))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::escape))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .cursor(CursorStyle::IBeam)
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                    .on_mouse_move(cx.listener(Self::on_mouse_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
                    .child(InputArea { editor: cx.entity(), child: items.into_any_element() }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_retyped_and_edited_lines_count_as_changed() {
        use scriptorium_core::ElementKind::*;
        let orig = Paragraph::plain(ParaId(0), Action, "Hello.");
        let originals: HashMap<ParaId, Paragraph> = [(orig.id, orig.clone())].into();
        assert!(!is_changed(&originals, &orig));
        assert!(is_changed(&originals, &Paragraph { kind: Dialogue, ..orig.clone() }));
        assert!(is_changed(&originals, &Paragraph::plain(ParaId(0), Action, "Hello!")));
        assert!(is_changed(&originals, &Paragraph::plain(ParaId(9), Action, "Hello.")));
    }

    /// A changed paragraph inside a row's run measures the rest of the run
    /// again (the left cell flows over it), and nothing else.
    #[test]
    fn a_change_measures_its_rows_run_again() {
        use scriptorium_core::ElementKind::Action;
        use scriptorium_versions::{LaidRow, RowStatus};
        let row = |this: &[u64], other: &[u64]| LaidRow {
            cells: vec![this.iter().map(|&i| ParaId(i)).collect(), other.iter().map(|&i| ParaId(i)).collect()],
            status: RowStatus::InSync,
            needs: false,
            note: None,
            suggestions: vec![],
        };
        let rows = [row(&[0], &[10]), row(&[1, 2, 3], &[11]), row(&[4], &[12])];
        let paras: Vec<Paragraph> = (0..5).map(|i| Paragraph::plain(ParaId(i), Action, "x")).collect();
        let ids: Vec<ParaId> = paras.iter().map(|p| p.id).collect();
        let bands = crate::links_view::bands(&rows, 0, &ids);
        let c = Columns { bands: ids.iter().copied().zip(bands).collect(), left: Rc::default() };
        assert_eq!(runs_of(&paras, &c, [0]), vec![0..1]);
        assert_eq!(runs_of(&paras, &c, [1]), vec![1..4]);
        assert_eq!(runs_of(&paras, &c, [2]), vec![1..4]);
        assert_eq!(runs_of(&paras, &c, [0, 4]), vec![0..1, 4..5]);
        assert_eq!(runs_of(&paras, &c, [3, 4]), vec![1..5]);
        // A paragraph with no band yet (just made): itself.
        let mut more = paras.clone();
        more.push(Paragraph::plain(ParaId(9), Action, "y"));
        assert_eq!(runs_of(&more, &c, [5]), vec![5..6]);
    }

    #[test]
    fn dual_dialogue_refusals_read_as_the_shared_message() {
        assert_eq!(refusal(&DocError::Fixed(ParaId(3))), FIXED_MESSAGE);
        assert!(refusal(&DocError::LastParagraph).starts_with("edit failed: "));
    }
}
