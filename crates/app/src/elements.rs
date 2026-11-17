//! Custom GPUI elements for the editor.
//!
//! - [`InputArea`] wraps the paragraph list: it clears last frame's layouts
//!   before the list paints, and registers the editor as the window's text
//!   input handler (keyboard text and IME go there).
//! - [`ParaElement`] is one paragraph: lays out its text in its element's
//!   column, paints page background, selection, text and cursor, and records
//!   where it was painted for hit-testing.
//!
//! Patterned after GPUI's `examples/input.rs` (Apache-2.0).

use gpui::{
    AnyElement, App, BorderStyle, Bounds, ElementId, ElementInputHandler, Entity, FontStyle, FontWeight,
    GlobalElementId, InspectorElementId, LayoutId, Pixels, SharedString, Style, TextAlign, TextRun, UnderlineStyle,
    Window, WrappedLine, fill, font, point, prelude::*, px, quad, relative, rgb, rgba, size,
};

use std::rc::Rc;

use crate::editor::{Mark, ParaLayout, ScriptEditor};
use crate::layout::{self, Align};
use crate::links_view::{Band, Columns};
use crate::theme;
use gpui::{ContentMask, PathBuilder};
use scriptorium_core::{Document, Paragraph};
use scriptorium_versions::RowStatus;

pub struct InputArea {
    pub editor: Entity<ScriptEditor>,
    pub child: AnyElement,
}

impl IntoElement for InputArea {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for InputArea {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let child = self.child.request_layout(window, cx);
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [child], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.editor.update(cx, |e, _| e.layouts.clear());
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.editor.update(cx, |e, cx| {
            if e.viewport.size.width != bounds.size.width {
                cx.notify();
            }
            e.viewport = bounds;
        });
        let focus = self.editor.read(cx).focus_handle().clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.editor.clone()), cx);
        self.child.paint(window, cx);
    }
}

pub struct ParaElement {
    pub editor: Entity<ScriptEditor>,
    pub ix: usize,
}

pub struct ParaState {
    line: Option<Rc<WrappedLine>>,
    /// A blank line's hint ([`crate::editor::Decor::ghosts`]), dimmed.
    ghost: Option<Rc<WrappedLine>>,
    geometry: layout::Geometry,
    /// x shift inside the column for centred / right-aligned text.
    align_offset: Pixels,
    /// Two columns: what is drawn on the left, and where this paragraph's
    /// own row starts below the top pad.
    cols: Option<ColState>,
}

/// One paragraph shaped for a column: its text's top below its cell's top,
/// x inside the page, and its lines.
struct Shaped {
    y: Pixels,
    x: Pixels,
    line: Option<Rc<WrappedLine>>,
}

/// A left cell shaped: its paragraphs one under the other, as on a page.
struct Cell {
    paras: Vec<Shaped>,
    height: Pixels,
}

/// One band of an item in two columns: a row's left cell, or the part of it
/// this paragraph shows (`shift`: how far down the cell this part starts).
struct Seg {
    /// Top, below the item's top.
    top: Pixels,
    height: Pixels,
    cell: Cell,
    shift: Pixels,
    status: Option<RowStatus>,
    /// Nothing on the right in this band (a row with nothing on this side).
    right_empty: bool,
    /// Nothing on the left: hatched.
    left_empty: bool,
    /// Where the status bar starts, below `top`: the row's first line of
    /// text, or the band's top past the first paragraph of a run (the bar
    /// runs on from the item above). It ends at the band's bottom.
    bar_top: Pixels,
}

struct ColState {
    segs: Vec<Seg>,
    /// The row has text on the left: a blank line here gets a rule to type
    /// on (the dimmed original's line is off in two columns).
    left_text: bool,
    /// Where this paragraph's own band starts, below the item's top.
    own_top: Pixels,
}

impl ParaElement {
    fn top_pad(&self) -> Pixels {
        if self.ix == 0 { layout::page_top() } else { px(0.) }
    }
}

impl IntoElement for ParaElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for ParaElement {
    type RequestLayoutState = ParaState;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ParaState) {
        let ed = self.editor.read(cx);
        let Some(p) = ed.doc.paragraphs().get(self.ix) else {
            let geometry = layout::geometry(&scriptorium_core::ElementKind::Action);
            return (
                window.request_layout(Style::default(), [], cx),
                ParaState { line: None, ghost: None, geometry, align_offset: px(0.), cols: None },
            );
        };
        let geometry = layout::geometry(&p.kind);
        let marked = ed.marked.as_ref().filter(|m| m.pos.para == p.id).cloned();

        // Display text and runs: document spans, plus the IME composition (underlined).
        let base_font = font("Courier Prime");
        let run = |len: usize, style: scriptorium_core::Style, underline: bool| TextRun {
            len,
            font: gpui::Font {
                weight: if style.bold { FontWeight::BOLD } else { FontWeight::NORMAL },
                style: if style.italic { FontStyle::Italic } else { FontStyle::Normal },
                ..base_font.clone()
            },
            color: rgb(0x111111).into(),
            background_color: None,
            underline: (style.underline || underline).then(|| UnderlineStyle {
                color: Some(rgb(0x111111).into()),
                thickness: px(1.),
                wavy: false,
            }),
            strikethrough: None,
        };
        let mut segs: Vec<(String, scriptorium_core::Style, bool)> =
            p.spans.iter().map(|s| (s.text.clone(), s.style, false)).collect();
        if let Some(m) = &marked {
            let mut out = Vec::new();
            let (mut pos, mut inserted) = (0, false);
            for (t, st, u) in segs {
                let end = pos + t.len();
                if !inserted && m.pos.offset >= pos && m.pos.offset <= end {
                    let cut = m.pos.offset - pos;
                    out.push((t[..cut].to_string(), st, u));
                    out.push((m.text.clone(), scriptorium_core::Style::default(), true));
                    out.push((t[cut..].to_string(), st, u));
                    inserted = true;
                } else {
                    out.push((t, st, u));
                }
                pos = end;
            }
            if !inserted {
                out.push((m.text.clone(), scriptorium_core::Style::default(), true));
            }
            segs = out.into_iter().filter(|s| !s.0.is_empty()).collect();
        }
        let text: String = segs.iter().map(|s| s.0.as_str()).collect();
        let runs: Vec<TextRun> = segs.iter().map(|(t, st, u)| run(t.len(), *st, *u)).collect();
        let text = if geometry.caps { layout::display_caps(&text) } else { text };

        let lh = layout::line_height();
        let blank = text.is_empty();
        let line = if text.is_empty() {
            None
        } else {
            window
                .text_system()
                .shape_text(SharedString::from(text), layout::font_size(), &runs, Some(geometry.width), None)
                .ok()
                .and_then(|mut lines| (!lines.is_empty()).then(|| Rc::new(lines.remove(0))))
        };
        // A blank line with a hint (the original's line on a version): shown
        // dimmed, and the paragraph made as tall as it.
        let columns = ed.decor.columns.clone();
        let ghost =
            ed.decor.ghosts.get(&p.id).filter(|_| blank && marked.is_none() && columns.is_none()).and_then(|g| {
                let g = if geometry.caps { layout::display_caps(g) } else { g.clone() };
                let runs = [TextRun {
                    color: rgb(0xb0b0b8).into(),
                    ..run(g.len(), scriptorium_core::Style::default(), false)
                }];
                window
                    .text_system()
                    .shape_text(SharedString::from(g), layout::font_size(), &runs, Some(geometry.width), None)
                    .ok()
                    .and_then(|mut lines| (!lines.is_empty()).then(|| Rc::new(lines.remove(0))))
            });
        let text_h = line.as_ref().or(ghost.as_ref()).map_or(lh, |l| l.size(lh).height.max(lh));
        let align_offset = match (&line, geometry.align) {
            (Some(l), Align::Right) => (geometry.width - l.width()).max(px(0.)),
            (Some(l), Align::Center) => ((geometry.width - l.width()) / 2.).max(px(0.)),
            _ => px(0.),
        };
        let own_h = lh * geometry.space_before + text_h;
        let cols = columns.as_ref().and_then(|c| {
            let band = c.bands.get(&p.id)?;
            Some(lay_out_columns(
                window,
                &ed.doc,
                c,
                band,
                (own_h, lh * geometry.space_before),
                self.top_pad(),
                ed.marked.as_ref(),
            ))
        });
        let height = match &cols {
            Some(c) => c.segs.iter().map(|s| s.top + s.height).fold(c.own_top + own_h, Pixels::max),
            None => self.top_pad() + own_h,
        };
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = height.into();
        (window.request_layout(style, [], cx), ParaState { line, ghost, geometry, align_offset, cols })
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut ParaState,
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut ParaState,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let lh = layout::line_height();
        let page_w = layout::page_width();
        let page_left = match &state.cols {
            None => {
                let page_left = bounds.left() + layout::page_left(bounds.size.width);
                window.paint_quad(fill(
                    Bounds::new(point(page_left, bounds.top()), size(page_w, bounds.size.height)),
                    rgb(0xffffff),
                ));
                page_left
            }
            Some(cols) => paint_columns(window, cx, bounds, cols),
        };
        let own_top = state.cols.as_ref().map_or(self.top_pad(), |c| c.own_top);

        let g = state.geometry;
        let text_origin = point(page_left + g.left + state.align_offset, bounds.top() + own_top + lh * g.space_before);
        let para_layout =
            ParaLayout { top: bounds.top(), bottom: bounds.bottom(), text_origin, line: state.line.clone() };
        let text_h = para_layout.line.as_ref().or(state.ghost.as_ref()).map_or(lh, |l| l.size(lh).height.max(lh));

        // Review decorations: the line under review, a mark in the left margin,
        // and Final Draft's revision `*` in the right margin for changed lines.
        let (is_current, mark, changed) = {
            let ed = self.editor.read(cx);
            let Some(p) = ed.doc.paragraphs().get(self.ix) else { return };
            let changed = crate::editor::is_changed(&ed.decor.originals, p);
            (ed.decor.current.contains(&p.id), ed.decor.marks.get(&p.id).copied(), changed)
        };
        let pad = px(5.);
        // With no blank line above (dialogue under its cue), a full pad would
        // run the box's border through the line above.
        let pad_top = if g.space_before >= 1.0 { pad } else { px(1.) };
        let block = Bounds::from_corners(
            point(page_left + layout::inch(1.5) - pad * 2., text_origin.y - pad_top),
            // Stay inside this paragraph's bounds: the next one paints its page over anything below.
            point(
                page_left + layout::inch(7.5) + pad * 2.,
                (text_origin.y + text_h + pad).min(bounds.bottom() - px(1.)),
            ),
        );
        if is_current {
            window.paint_quad(quad(block, px(4.), theme::ACCENT_SOFT, px(1.5), theme::ACCENT, BorderStyle::Solid));
        }
        if let Some(mark) = mark {
            paint_mark(
                window,
                mark,
                point(page_left + layout::inch(layout::COLUMN_LEFT) - px(layout::MARK_OFFSET), text_origin.y + lh / 2.),
            );
        }
        if changed {
            let star = window.text_system().shape_line(
                "*".into(),
                layout::font_size(),
                &[TextRun {
                    len: 1,
                    font: gpui::Font { weight: FontWeight::BOLD, ..font("Courier Prime") },
                    color: rgb(0x111111).into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            );
            let _ =
                star.paint(point(page_left + layout::inch(7.75), text_origin.y), lh, TextAlign::Left, None, window, cx);
        }

        let ed = self.editor.read(cx);
        let Some(p) = ed.doc.paragraphs().get(self.ix) else { return };
        let id = p.id;
        let len = p.text().len();
        let focused = ed.focus_handle().is_focused(window);
        let pos_at = |i: usize| para_layout.position_for_index(i);

        // Selection.
        let (a, b) = ed.doc.ordered(ed.sel);
        let (ia, ib) = (ed.doc.index_of(a.para).unwrap_or(0), ed.doc.index_of(b.para).unwrap_or(0));
        if !ed.sel.is_collapsed() && (ia..=ib).contains(&self.ix) {
            let lo = if self.ix == ia { a.offset } else { 0 };
            let hi = if self.ix == ib { b.offset } else { len };
            let (s, e) = (pos_at(lo), pos_at(hi));
            let color = rgba(0x3b82f655);
            let full_w = g.width.max(e.x);
            let mut rects = Vec::new();
            if s.y == e.y {
                let end_x = if self.ix < ib { e.x + px(8.) } else { e.x };
                rects.push(Bounds::from_corners(s, point(end_x, s.y + lh)));
            } else {
                rects.push(Bounds::from_corners(s, point(full_w, s.y + lh)));
                if e.y > s.y + lh {
                    rects.push(Bounds::from_corners(point(px(0.), s.y + lh), point(full_w, e.y)));
                }
                let end_x = if self.ix < ib { e.x + px(8.) } else { e.x };
                rects.push(Bounds::from_corners(point(px(0.), e.y), point(end_x, e.y + lh)));
            }
            for r in rects {
                window.paint_quad(fill(Bounds::new(text_origin + r.origin, r.size), color));
            }
        }

        // Cursor position (inside a composition if there is one).
        let cursor = (focused && ed.sel.head.para == id).then(|| {
            let extra = ed.marked.as_ref().filter(|m| m.pos.para == id).map_or(0, |m| m.cursor);
            pos_at(ed.sel.head.offset + extra)
        });

        if state.line.is_none() && state.cols.as_ref().is_some_and(|c| c.left_text) {
            let y = text_origin.y + lh - px(2.);
            window.paint_quad(fill(Bounds::new(point(text_origin.x, y), size(g.width, px(1.))), theme::RULE));
        }
        if let Some(line) = &state.line {
            let _ = line.paint(text_origin, lh, TextAlign::Left, None, window, cx);
        } else if let Some(ghost) = &state.ghost {
            let _ = ghost.paint(text_origin, lh, TextAlign::Left, None, window, cx);
        }
        if let Some(c) = cursor {
            window.paint_quad(fill(Bounds::new(text_origin + c, size(px(2.), lh)), rgb(0x1d4ed8)));
        }
        let _ = text_h;
        self.editor.update(cx, |e, _| {
            e.layouts.insert(id, para_layout);
        });
    }
}

/// The left paper: a shade off white, so the script lined up there reads as
/// the one not being edited.
const LEFT_PAPER: u32 = 0xf7f6f1;
/// The left page's edges: a hairline, so it reads as a page beside the
/// sidebar's grey.
const LEFT_EDGE: u32 = 0xd8d6cc;
/// Its ink.
const LEFT_INK: u32 = 0x2a2a30;

/// The text runs of `p`'s spans in Courier, in `color`.
fn runs_of(p: &Paragraph, color: u32) -> Vec<TextRun> {
    p.spans
        .iter()
        .map(|s| TextRun {
            len: s.text.len(),
            font: gpui::Font {
                weight: if s.style.bold { FontWeight::BOLD } else { FontWeight::NORMAL },
                style: if s.style.italic { FontStyle::Italic } else { FontStyle::Normal },
                ..font("Courier Prime")
            },
            color: rgb(color).into(),
            background_color: None,
            underline: s.style.underline.then(|| UnderlineStyle {
                color: Some(rgb(color).into()),
                thickness: px(1.),
                wavy: false,
            }),
            strikethrough: None,
        })
        .filter(|r| r.len > 0)
        .collect()
}

/// `p` shaped in its element's column: its geometry, its lines (none when
/// blank) and its height with the blank lines above it.
fn shape(window: &mut Window, p: &Paragraph, color: u32) -> (layout::Geometry, Option<Rc<WrappedLine>>, Pixels) {
    let lh = layout::line_height();
    let g = layout::geometry(&p.kind);
    let text = p.text();
    let text = if g.caps { layout::display_caps(&text) } else { text };
    let line = (!text.is_empty())
        .then(|| {
            window
                .text_system()
                .shape_text(SharedString::from(text), layout::font_size(), &runs_of(p, color), Some(g.width), None)
                .ok()
                .and_then(|mut lines| (!lines.is_empty()).then(|| Rc::new(lines.remove(0))))
        })
        .flatten();
    let text_h = line.as_ref().map_or(lh, |l| l.size(lh).height.max(lh));
    (g, line, lh * g.space_before + text_h)
}

/// The other member's paragraphs `ids` one under the other, as its page
/// sets them.
fn shape_cell(window: &mut Window, c: &Columns, ids: &[scriptorium_core::ParaId]) -> Cell {
    let lh = layout::line_height();
    let mut y = px(0.);
    let mut paras = vec![];
    for p in ids.iter().filter_map(|id| c.left.get(id)) {
        let (g, line, h) = shape(window, p, LEFT_INK);
        let x = match (&line, g.align) {
            (Some(l), Align::Right) => g.left + (g.width - l.width()).max(px(0.)),
            (Some(l), Align::Center) => g.left + ((g.width - l.width()) / 2.).max(px(0.)),
            _ => g.left,
        };
        paras.push(Shaped { y: y + lh * g.space_before, x, line });
        y += h;
    }
    Cell { paras, height: y }
}

/// What an item draws beside its paragraph in two columns (B2's spike chose
/// *slices*): rows with nothing on this side above it, its row's left cell
/// (or, past the first paragraph of the row's run, the part of it below the
/// run's paragraphs above), then rows after it. `own_h`: the paragraph's
/// own height with its blank lines, `own_lead` of it blank lines; `top_pad`: the page's top margin on the
/// first item; `marked`: the IME composition, wherever it is.
///
/// Every item draws only inside its own bounds, so the list can measure,
/// scroll and paint it alone; the run's last paragraph is made tall enough
/// for what is left of the left cell. Heights are whole lines, so a slice
/// never cuts a line of text.
fn lay_out_columns(
    window: &mut Window,
    doc: &Document,
    c: &Columns,
    band: &Band,
    (own_h, own_lead): (Pixels, Pixels),
    top_pad: Pixels,
    marked: Option<&crate::editor::Marked>,
) -> ColState {
    let mut segs = vec![];
    let mut top = top_pad;
    let one_sided = |window: &mut Window, top: Pixels, cell: &crate::links_view::LeftCell| {
        let cell_shaped = shape_cell(window, c, &cell.left);
        let bar_top = cell_shaped.paras.first().map_or(px(0.), |p| p.y);
        Seg {
            bar_top,
            top,
            height: cell_shaped.height,
            cell: cell_shaped,
            shift: px(0.),
            status: cell.status,
            right_empty: true,
            left_empty: false,
        }
    };
    for cell in &band.before {
        let seg = one_sided(window, top, cell);
        top += seg.height;
        segs.push(seg);
    }
    let own_top = top;
    // The run's first paragraph carries the row's left cell.
    let first = band.above.first().and_then(|id| c.bands.get(id)).unwrap_or(band);
    let cell = shape_cell(window, c, &first.cell.left);
    // The run's paragraphs above, as they are shown: with any IME
    // composition in them.
    let shift = band
        .above
        .iter()
        .filter_map(|id| doc.paragraph(*id))
        .map(|p| {
            let h = match marked.filter(|m| m.pos.para == p.id) {
                Some(m) => {
                    let mut text = p.text();
                    text.insert_str(m.pos.offset.min(text.len()), &m.text);
                    text_height(window, &Paragraph::plain(p.id, p.kind.clone(), &text))
                }
                None => text_height(window, p),
            };
            layout::geometry(&p.kind).space_before * layout::line_height() + h
        })
        .fold(px(0.), |a, b| a + b);
    let height = if band.last { own_h.max(cell.height - shift) } else { own_h };
    let left_empty = first.cell.left.is_empty();
    // The whole run shows its row's status: one bar down the gutter.
    let status = first.cell.status;
    let bar_top = if band.slice == 0 { cell.paras.first().map_or(own_lead, |p| p.y.min(own_lead)) } else { px(0.) };
    segs.push(Seg { top, height, cell, shift, status, right_empty: false, left_empty, bar_top });

    top += height;
    for cell in &band.after {
        let seg = one_sided(window, top, cell);
        top += seg.height;
        segs.push(seg);
    }
    ColState { segs, own_top, left_text: !first.cell.left.is_empty() }
}

/// `p`'s text height (at least one line), as [`shape`] lays it out.
fn text_height(window: &mut Window, p: &Paragraph) -> Pixels {
    let (g, _, h) = shape(window, p, LEFT_INK);
    h - g.space_before * layout::line_height()
}

/// Paint two columns' paper, the left cells, hatching for an empty side and
/// the status dots; returns where the right page's left edge is.
fn paint_columns(window: &mut Window, cx: &mut App, bounds: Bounds<Pixels>, cols: &ColState) -> Pixels {
    let lh = layout::line_height();
    let page_w = layout::page_width();
    let split = layout::two_columns(f32::from(bounds.size.width));
    let left_col = Bounds::new(bounds.origin, size(px(split.left_w), bounds.size.height));
    let right_col = Bounds::new(
        point(bounds.left() + px(split.right_x), bounds.top()),
        size(px(split.right_w), bounds.size.height),
    );
    let left_page = left_col.left() + layout::page_left_unmarked(left_col.size.width);
    let right_page = right_col.left() + layout::page_left(right_col.size.width);
    let mask = |b: Bounds<Pixels>| Some(ContentMask { bounds: b });
    window.with_content_mask(mask(left_col), |w| {
        w.paint_quad(fill(
            Bounds::new(point(left_page, bounds.top()), size(page_w, bounds.size.height)),
            rgb(LEFT_PAPER),
        ));
        for x in [left_page, left_page + page_w - px(1.)] {
            w.paint_quad(fill(Bounds::new(point(x, bounds.top()), size(px(1.), bounds.size.height)), rgb(LEFT_EDGE)));
        }
    });
    window.with_content_mask(mask(right_col), |w| {
        w.paint_quad(fill(
            Bounds::new(point(right_page, bounds.top()), size(page_w, bounds.size.height)),
            rgb(0xffffff),
        ))
    });
    // The text column of each page, for hatching an empty side.
    let column = |page: Pixels, top: Pixels, h: Pixels| {
        Bounds::new(point(page + layout::inch(layout::COLUMN_LEFT), bounds.top() + top), size(layout::inch(6.0), h))
    };
    for seg in &cols.segs {
        let band = Bounds::new(point(left_col.left(), bounds.top() + seg.top), size(left_col.size.width, seg.height));
        window.with_content_mask(mask(band), |w| {
            let origin = point(left_page, bounds.top() + seg.top - seg.shift);
            for p in &seg.cell.paras {
                if let Some(line) = &p.line {
                    let _ = line.paint(origin + point(p.x, p.y), lh, TextAlign::Left, None, w, cx);
                }
            }
        });
        if seg.left_empty {
            hatch(window, column(left_page, seg.top, seg.height), left_col);
        }
        if seg.right_empty {
            hatch(window, column(right_page, seg.top, seg.height), right_col);
        }
        if let Some(look) = seg.status.and_then(status_look) {
            let x = bounds.left() + px(split.left_w + layout::GUTTER / 2.) - STATUS_BAR / 2.;
            let bar = Bounds::from_corners(
                point(x, bounds.top() + seg.top + seg.bar_top),
                point(x + STATUS_BAR, bounds.top() + seg.top + seg.height),
            );
            paint_status(window, look, bar);
        }
    }
    right_page
}

/// Thin diagonal strokes over `area` (inside `clip`): this side has
/// nothing in the row.
fn hatch(window: &mut Window, area: Bounds<Pixels>, clip: Bounds<Pixels>) {
    if area.size.height <= px(0.) {
        return;
    }
    let step = px(9.);
    let mut b = PathBuilder::stroke(px(1.));
    let h = area.size.height;
    let mut x = area.left() - h;
    while x < area.right() {
        b.move_to(point(x, area.bottom()));
        b.line_to(point(x + h, area.top()));
        x += step;
    }
    if let Ok(path) = b.build() {
        window.with_content_mask(Some(ContentMask { bounds: area.intersect(&clip) }), |w| {
            w.paint_path(path, rgba(0x8b919b40))
        });
    }
}

/// The status bar's width: a thin bar down the gutter, the row's height
/// (unlike the round review marks).
const STATUS_BAR: Pixels = px(4.);

/// How a row's status shows in the gutter, and its legend label. The one
/// place for these: the bar here, the sidebar's legend (B3), B4.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StatusLook {
    pub color: gpui::Rgba,
    /// Both changed: the bar's right half in this colour.
    pub second: Option<gpui::Rgba>,
    /// Drawn as an outline (nothing in the version yet).
    pub outline: bool,
    pub label: &'static str,
}

/// The statuses the legend lists, in its order.
pub(crate) const STATUS_LEGEND: [RowStatus; 5] =
    [RowStatus::InSync, RowStatus::Blank, RowStatus::ThisChanged, RowStatus::OtherChanged, RowStatus::BothChanged];

/// quiet grey = in sync (green is only "accepted", on review dots), grey
/// outline = not yet translated, amber = the version
/// changed since, red = the original changed since, red and amber = both.
/// The colours name the members, not the sides, so they read the same on
/// either script. None for a one-sided row: the hatching says it.
pub(crate) fn status_look(status: RowStatus) -> Option<StatusLook> {
    let look = |color, label| StatusLook { color, second: None, outline: false, label };
    Some(match status {
        RowStatus::InSync => look(theme::IN_SYNC, "in sync"),
        RowStatus::Blank => StatusLook { outline: true, ..look(theme::INK_3, "not yet translated") },
        RowStatus::ThisChanged => look(theme::OWN, "version changed"),
        RowStatus::OtherChanged => look(theme::WARN, "original changed"),
        RowStatus::BothChanged => StatusLook { second: Some(theme::OWN), ..look(theme::WARN, "both changed") },
        RowStatus::OneSided => return None,
    })
}

fn paint_status(window: &mut Window, look: StatusLook, bar: Bounds<Pixels>) {
    if bar.size.height <= px(0.) {
        return;
    }
    let r = px(1.5);
    if look.outline {
        window.paint_quad(quad(bar, r, theme::PAPER, px(1.), look.color, BorderStyle::Solid));
    } else if let Some(second) = look.second {
        let half = bar.size.width / 2.;
        window.paint_quad(quad(
            Bounds::new(bar.origin, size(half, bar.size.height)),
            px(0.),
            look.color,
            px(0.),
            look.color,
            BorderStyle::Solid,
        ));
        let right = Bounds::new(point(bar.left() + half, bar.top()), size(half, bar.size.height));
        window.paint_quad(quad(right, px(0.), second, px(0.), second, BorderStyle::Solid));
    } else {
        window.paint_quad(quad(bar, r, look.color, px(0.), look.color, BorderStyle::Solid));
    }
}

/// A small shape in the left margin: filled dot = pending, ring = outdated,
/// green dot = accepted, grey ring = kept, amber square = rewritten.
fn paint_mark(window: &mut Window, mark: Mark, center: gpui::Point<Pixels>) {
    let r = px(4.5);
    let b = Bounds::from_corners(center - point(r, r), center + point(r, r));
    let (bg, border, radius) = match mark {
        Mark::Pending => (theme::ACCENT, theme::ACCENT, r),
        Mark::Outdated => (theme::PAPER, theme::WARN, r),
        Mark::Accepted => (theme::OK, theme::OK, r),
        Mark::Kept => (theme::PAPER, theme::INK_3, r),
        Mark::Rewritten => (theme::OWN, theme::OWN, px(1.5)),
    };
    window.paint_quad(quad(b, radius, bg, px(1.5), border, BorderStyle::Solid));
}
