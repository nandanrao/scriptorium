//! The script editor view: holds a `scriptorium_core::Document` and turns
//! keyboard, mouse and IME input into document commands.
//!
//! The document owns all content, undo/redo and paragraph ids. This view owns
//! only presentation state: the selection, IME composition, scroll position and
//! the layouts painted last frame (used for hit-testing and up/down movement).
//!
//! The window around it ([`crate::workspace`]) runs review decisions through
//! [`ScriptEditor::run`], sets [`Decor`] (current line, gutter marks), and
//! listens for [`EditorEvent`]s. While `structural` is false (the default),
//! editing stays inside paragraphs: saving can't write new, removed or retyped
//! paragraphs yet.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    IntoElement, ListAlignment, ListOffset, ListState, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Point, Render, Styled, UTF16Selection, Window, WrappedLine, actions, div, list, point,
    prelude::*, px, size,
};
use scriptorium_core::doc::{DocError, Document};
use scriptorium_core::{ParaId, Paragraph, Pos, Selection};

use crate::elements::{InputArea, ParaElement};
use crate::layout;

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
    pub current: Option<ParaId>,
    pub marks: HashMap<ParaId, Mark>,
    /// Text as opened from the file, to put Final Draft's `*` beside changed lines.
    pub originals: Rc<HashMap<ParaId, String>>,
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
    /// Enter while structural edits are off: "I'm done with this line".
    Commit,
    /// Something the user should be told (e.g. why a key did nothing).
    Notice(String),
}

impl EventEmitter<EditorEvent> for ScriptEditor {}

const NO_STRUCTURE: &str = "Adding, joining and retyping paragraphs isn't supported yet: do that in Final Draft.";

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
    /// Allow edits that add, remove, join or retype paragraphs.
    pub structural: bool,
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
            structural: false,
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

    /// Refuse an edit that would change the paragraph structure, if those are off.
    fn refuse(&self, cx: &mut Context<Self>) -> bool {
        if !self.structural {
            cx.emit(EditorEvent::Notice(NO_STRUCTURE.into()));
        }
        !self.structural
    }

    fn spans_paragraphs(&self) -> bool {
        self.sel.anchor.para != self.sel.head.para
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    // ---- applying changes --------------------------------------------------

    /// Run a document command, then update selection, list and view.
    fn edit(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Document, Selection) -> Result<Selection, DocError>,
    ) {
        self.apply(cx, true, f);
    }

    fn apply(
        &mut self,
        cx: &mut Context<Self>,
        reveal: bool,
        f: impl FnOnce(&mut Document, Selection) -> Result<Selection, DocError>,
    ) {
        self.marked = None;
        let before = self.doc.log().len();
        match f(&mut self.doc, self.sel) {
            Ok(sel) => self.sel = sel,
            Err(e) => cx.emit(EditorEvent::Notice(format!("edit failed: {e}"))),
        }
        self.sync_list();
        self.goal_x = None;
        if reveal {
            self.reveal_head();
        }
        if self.doc.log().len() != before {
            cx.emit(EditorEvent::Edited);
        }
        cx.notify();
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
        }
        self.shown = new.to_vec();
    }

    fn remeasure_head(&mut self) {
        if let Some(ix) = self.doc.index_of(self.sel.head.para) {
            self.list.splice(ix..ix + 1, 1);
        }
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

    /// Would deleting backward/forward from here join paragraphs?
    fn deletes_across(&self, forward: bool) -> bool {
        if !self.sel.is_collapsed() {
            return self.spans_paragraphs();
        }
        let h = self.sel.head;
        if forward { h.offset >= self.doc.paragraph(h.para).map_or(0, |p| p.text().len()) } else { h.offset == 0 }
    }

    fn delete_dir(&mut self, forward: bool, word: bool, cx: &mut Context<Self>) {
        if self.deletes_across(forward) && self.refuse(cx) {
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
        if self.structural {
            self.edit(cx, |d, s| d.enter(s));
        } else {
            cx.emit(EditorEvent::Commit);
        }
    }
    fn escape(&mut self, _: &Escape, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(EditorEvent::Escape);
    }
    fn tab(&mut self, _: &Tab, _: &mut Window, cx: &mut Context<Self>) {
        if !self.refuse(cx) {
            self.edit(cx, |d, s| d.cycle_kind(s, true));
        }
    }
    fn shift_tab(&mut self, _: &ShiftTab, _: &mut Window, cx: &mut Context<Self>) {
        if !self.refuse(cx) {
            self.edit(cx, |d, s| d.cycle_kind(s, false));
        }
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

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.sel.is_collapsed() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.doc.text_in(self.sel)));
        }
    }
    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if self.spans_paragraphs() && self.refuse(cx) {
            return;
        }
        if !self.sel.is_collapsed() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.doc.text_in(self.sel)));
            self.edit(cx, |d, s| d.delete_backward(s, false));
        }
    }
    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        if self.spans_paragraphs() && self.refuse(cx) {
            return;
        }
        let lines = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty());
        // Screenplay text copied from elsewhere separates paragraphs with blank
        // lines. Without structural edits, it all goes into this paragraph.
        let text = lines.collect::<Vec<_>>().join(if self.structural { "\n" } else { " " });
        self.edit(cx, |d, s| d.insert_text(s, &text));
    }

    // ---- mouse -------------------------------------------------------------

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let Some(pos) = self.hit_test(event.position) else { return };
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
        let text = text.to_string();
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
