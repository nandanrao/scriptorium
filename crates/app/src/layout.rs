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

/// The part of the page that must stay on screen when the page is wider than
/// the space it has, in inches from the page's left edge: the review mark left
/// of the text column (1.5 in), the column, and the revision `*` at 7.75 in.
pub const VISIBLE_LEFT: f32 = 1.1;
pub const VISIBLE_RIGHT: f32 = 8.0;

/// The smallest zoom the page ever takes.
pub const MIN_ZOOM: f32 = 0.5;

/// The list width that shows the text column with its marks at zoom `z`
/// (the inverse of [`zoom_for`]'s column fit).
pub fn column_width_at(z: f32) -> f32 {
    (VISIBLE_RIGHT - VISIBLE_LEFT) * 96.0 * z + 16.0
}

/// The zoom for `available_width`: the whole page up to [`MAX_ZOOM`], but never
/// smaller than 1.0 while the text column with its marks still fits (the page's
/// outer margins are cropped instead, see [`page_left`]); below that, the
/// smallest zoom that keeps the column readable without cropping text.
pub fn zoom_for(available_width: f32) -> f32 {
    let room = available_width - 16.0;
    let page = room / (8.5 * 96.0);
    let column = room / ((VISIBLE_RIGHT - VISIBLE_LEFT) * 96.0);
    page.max(column.min(1.0)).clamp(MIN_ZOOM, MAX_ZOOM)
}

/// Fit the page to `available_width` (see [`zoom_for`]). Returns true if the zoom changed.
pub fn fit_width(available_width: Pixels) -> bool {
    let z = zoom_for(f32::from(available_width));
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

/// Where the page's left edge goes in a list `width` wide (relative to the
/// list's left): centred when it fits; otherwise the visible part (text column
/// and marks) is centred and the outer margins are cut off.
pub fn page_left(width: Pixels) -> Pixels {
    px(page_left_at(f32::from(width), zoom()))
}

/// [`page_left`] at zoom `z`, in plain numbers.
fn page_left_at(width: f32, z: f32) -> f32 {
    let page = 8.5 * 96.0 * z;
    if width >= page {
        return (width - page) / 2.;
    }
    let visible_mid = (VISIBLE_LEFT + VISIBLE_RIGHT) / 2. * 96.0 * z;
    // Never leave a gap at the right edge of the list.
    (width / 2. - visible_mid).max(width - page).min(0.)
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
    fn zoom_keeps_normal_size_while_the_column_fits() {
        // Wide: the whole page, up to the maximum.
        assert_eq!(zoom_for(2000.0), MAX_ZOOM);
        // 1500 px window with panel and card (~700 px): normal size, margins cropped.
        assert_eq!(zoom_for(708.0), 1.0);
        // Too narrow even for the column: shrink, but never below 0.5.
        assert!(zoom_for(500.0) < 1.0);
        assert_eq!(zoom_for(100.0), 0.5);
    }

    #[test]
    fn column_width_is_zoom_for_inverted() {
        assert!((zoom_for(column_width_at(0.6)) - 0.6).abs() < 1e-4);
        assert_eq!(zoom_for(column_width_at(MIN_ZOOM)), MIN_ZOOM);
    }

    #[test]
    fn zoom_hands_over_from_cropping_to_shrinking() {
        // The column with its marks is 6.9 in = 662.4 px at 1.0, plus 16 px.
        let edge = (VISIBLE_RIGHT - VISIBLE_LEFT) * 96.0 + 16.0;
        assert_eq!(zoom_for(edge), 1.0);
        assert_eq!(zoom_for(edge + 50.0), 1.0);
        let below = zoom_for(edge - 20.0);
        assert!(below < 1.0 && below > 0.95, "{below}");
        // The whole page fits from 8.5 in + 16 px: then it grows past 1.0.
        assert!(zoom_for(8.5 * 96.0 + 16.0 + 50.0) > 1.0);
    }

    #[test]
    fn page_left_centres_crops_and_never_leaves_a_gap() {
        let page = 8.5 * 96.0;
        // Room to spare: centred.
        assert_eq!(page_left_at(page + 100.0, 1.0), 50.0);
        assert_eq!(page_left_at(page, 1.0), 0.0);
        // A little short: the visible part (1.1–8.0 in) is centred, the
        // margins cropped, and the page's right edge stays at the list's.
        let w = 700.0;
        let left = page_left_at(w, 1.0);
        assert!(left < 0.0);
        assert!(left + VISIBLE_LEFT * 96.0 >= 0.0, "the left mark is cut: {left}");
        assert!(left + VISIBLE_RIGHT * 96.0 <= w, "the * is cut: {left}");
        assert!(left + page >= w, "a gap at the right: {left}");
        // Just short of the page: cropping is only what doesn't fit.
        let left = page_left_at(page - 10.0, 1.0);
        assert!((-10.0..0.0).contains(&left), "{left}");
        // At the hand-over to shrinking the visible part fills the list.
        let w = (VISIBLE_RIGHT - VISIBLE_LEFT) * 96.0;
        assert!((page_left_at(w, 1.0) + VISIBLE_LEFT * 96.0).abs() < 0.01);
        let z = zoom_for(500.0);
        let left = page_left_at(500.0 - 16.0, z);
        assert!(left + VISIBLE_LEFT * 96.0 * z >= -0.01 && left + VISIBLE_RIGHT * 96.0 * z <= 484.01, "{left}");
    }

    #[test]
    fn caps_keeps_byte_offsets() {
        for s in ["cesar", "señor ñandú", "straße", "ǆ"] {
            assert_eq!(display_caps(s).len(), s.len(), "{s}");
        }
        assert_eq!(display_caps("señor"), "SEÑOR");
    }
}
