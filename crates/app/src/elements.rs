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
use crate::theme;

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
    geometry: layout::Geometry,
    /// x shift inside the column for centred / right-aligned text.
    align_offset: Pixels,
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
                ParaState { line: None, geometry, align_offset: px(0.) },
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
        let line = if text.is_empty() {
            None
        } else {
            window
                .text_system()
                .shape_text(SharedString::from(text), layout::font_size(), &runs, Some(geometry.width), None)
                .ok()
                .and_then(|mut lines| (!lines.is_empty()).then(|| Rc::new(lines.remove(0))))
        };
        let text_h = line.as_ref().map_or(lh, |l| l.size(lh).height.max(lh));
        let align_offset = match (&line, geometry.align) {
            (Some(l), Align::Right) => (geometry.width - l.width()).max(px(0.)),
            (Some(l), Align::Center) => ((geometry.width - l.width()) / 2.).max(px(0.)),
            _ => px(0.),
        };
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = (self.top_pad() + lh * geometry.space_before + text_h).into();
        (window.request_layout(style, [], cx), ParaState { line, geometry, align_offset })
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
        let page_left = bounds.left() + layout::page_left(bounds.size.width);
        window.paint_quad(fill(
            Bounds::new(point(page_left, bounds.top()), size(page_w, bounds.size.height)),
            rgb(0xffffff),
        ));

        let g = state.geometry;
        let text_origin =
            point(page_left + g.left + state.align_offset, bounds.top() + self.top_pad() + lh * g.space_before);
        let para_layout =
            ParaLayout { top: bounds.top(), bottom: bounds.bottom(), text_origin, line: state.line.clone() };
        let text_h = para_layout.line.as_ref().map_or(lh, |l| l.size(lh).height.max(lh));

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
            paint_mark(window, mark, point(page_left + layout::inch(1.5) - px(26.), text_origin.y + lh / 2.));
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

        if let Some(line) = &state.line {
            let _ = line.paint(text_origin, lh, TextAlign::Left, None, window, cx);
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
