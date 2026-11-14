//! Page geometry: where each element sits on a screenplay page.
//!
//! Values are Final Draft's defaults (the same ones the PURPLE LIST carries in
//! its `<ElementSettings>`). Later these should come from the file itself.

use gpui::{Pixels, px};
use scriptorium_core::ElementKind;

use std::sync::atomic::{AtomicU32, Ordering};

/// Largest screen zoom. 1.0 = 96 px per inch (12 pt Courier = 16 px, 6 lines per inch).
pub const MAX_ZOOM: f32 = 1.25;

static ZOOM_BITS: AtomicU32 = AtomicU32::new(0x3fa0_0000); // 1.25f32

pub fn zoom() -> f32 {
    f32::from_bits(ZOOM_BITS.load(Ordering::Relaxed))
}

/// Fit the page to `available_width` (up to [`MAX_ZOOM`]). Returns true if the zoom changed.
pub fn fit_width(available_width: Pixels) -> bool {
    let fit = (f32::from(available_width) - 16.0) / (8.5 * 96.0);
    let z = fit.clamp(0.5, MAX_ZOOM);
    let changed = (z - zoom()).abs() > 0.005;
    if changed {
        ZOOM_BITS.store(z.to_bits(), Ordering::Relaxed);
    }
    changed
}

pub fn inch(x: f32) -> Pixels {
    px(x * 96.0 * zoom())
}

pub fn font_size() -> Pixels {
    px(16.0 * zoom())
}

/// One screenplay line (12 pt).
pub fn line_height() -> Pixels {
    px(16.0 * zoom())
}

pub fn page_width() -> Pixels {
    inch(8.5)
}

/// Blank space above the first paragraph (the page's top margin).
pub fn page_top() -> Pixels {
    inch(1.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    /// Text column, measured from the page's left edge.
    pub left: Pixels,
    pub width: Pixels,
    /// Blank lines above the paragraph.
    pub space_before: f32,
    pub align: Align,
    pub caps: bool,
}

pub fn geometry(kind: &ElementKind) -> Geometry {
    // (left inch, right inch, space before in 12 pt lines, align)
    let (l, r, sb, align) = match kind {
        ElementKind::SceneHeading => (1.5, 7.5, 2.0, Align::Left),
        ElementKind::Action => (1.5, 7.5, 1.0, Align::Left),
        ElementKind::Character => (3.5, 7.25, 1.0, Align::Left),
        ElementKind::Parenthetical => (3.0, 5.5, 0.0, Align::Left),
        ElementKind::Dialogue => (2.5, 6.0, 0.0, Align::Left),
        ElementKind::Transition => (5.5, 7.1, 1.0, Align::Right),
        ElementKind::Shot => (1.5, 7.5, 1.0, Align::Left),
        ElementKind::General => (1.5, 7.5, 0.0, Align::Left),
        ElementKind::Other(name) if name == "End of Act" => (1.5, 7.5, 2.0, Align::Center),
        ElementKind::Other(name) if name == "New Act" => (1.5, 7.5, 1.0, Align::Center),
        ElementKind::Other(_) => (1.5, 7.5, 1.0, Align::Left),
    };
    let caps = scriptorium_core::rules::is_caps(kind) || matches!(kind, ElementKind::Other(n) if n.contains("Act"));
    Geometry { left: inch(l), width: inch(r - l), space_before: sb, align, caps }
}

/// Uppercase for display without changing byte offsets: characters whose
/// uppercase form has a different UTF-8 length (e.g. ß → SS) are left alone.
pub fn display_caps(text: &str) -> String {
    text.chars()
        .map(|c| {
            let mut up = c.to_uppercase();
            match (up.next(), up.next()) {
                (Some(u), None) if u.len_utf8() == c.len_utf8() => u,
                _ => c,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_keeps_byte_offsets() {
        for s in ["cesar", "señor ñandú", "straße", "ǆ"] {
            assert_eq!(display_caps(s).len(), s.len(), "{s}");
        }
        assert_eq!(display_caps("señor"), "SEÑOR");
    }
}
