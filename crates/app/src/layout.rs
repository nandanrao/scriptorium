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

/// The text column's left edge (1.5 in), and how far left of it the review
/// mark's centre sits (in screen px, whatever the zoom).
pub const COLUMN_LEFT: f32 = 1.5;
pub const MARK_OFFSET: f32 = 26.0;
/// Room left of the mark's centre for the mark itself (radius 4.5, border).
const MARK_ROOM: f32 = 7.0;
/// The right edge of what must stay on screen, in inches: past the revision
/// `*` at 7.75 in.
pub const VISIBLE_RIGHT: f32 = 8.0;

/// The part of the page that must stay on screen when the page is wider
/// than the space it has, in px from the page's left edge at zoom `z`: the
/// review mark, the text column and the revision `*`.
fn visible_at(z: f32) -> (f32, f32) {
    (COLUMN_LEFT * 96.0 * z - MARK_OFFSET - MARK_ROOM, VISIBLE_RIGHT * 96.0 * z)
}

/// The smallest zoom the page ever takes.
pub const MIN_ZOOM: f32 = 0.5;

/// The smallest zoom the script is shown at (about 7 pt Courier); a column
/// too narrow for it gives way instead (`agent_panel::columns`).
pub const READABLE_ZOOM: f32 = 0.6;

/// The list width that shows the text column with its marks at zoom `z`
/// (the inverse of [`zoom_for`]'s column fit).
pub fn column_width_at(z: f32) -> f32 {
    let (l, r) = visible_at(z);
    r - l + 16.0
}

/// The zoom for `available_width`: the whole page up to [`MAX_ZOOM`], but never
/// smaller than 1.0 while the text column with its marks still fits (the page's
/// outer margins are cropped instead, see [`page_left`]); below that, the
/// smallest zoom that keeps the column readable without cropping text.
pub fn zoom_for(available_width: f32) -> f32 {
    let room = available_width - 16.0;
    let page = room / (8.5 * 96.0);
    let column = (room - MARK_OFFSET - MARK_ROOM) / ((VISIBLE_RIGHT - COLUMN_LEFT) * 96.0);
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
    let (l, r) = visible_at(z);
    let visible_mid = (l + r) / 2.;
    // Never leave a gap at the right edge of the list.
    (width / 2. - visible_mid).max(width - page).min(0.)
}

/// The gap between the two columns (the script lined up on the left, the
/// open one on the right), where each row's status dot sits.
pub const GUTTER: f32 = 28.0;

/// Where the two columns go in a list `width` wide, in px from its left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TwoColumns {
    pub left_w: f32,
    pub right_x: f32,
    pub right_w: f32,
}

/// The two columns in a list `width` wide: an even split around the
/// [`GUTTER`] for now. Both share one zoom, fitted to the right column
/// ([`zoom_for`]). B4 sizes the script area for two columns at
/// [`READABLE_ZOOM`] (`agent_panel::columns`, `lined_up`) and decides when
/// the left gives way; this split is what it hands the columns.
pub fn two_columns(width: f32) -> TwoColumns {
    let left_w = ((width - GUTTER) / 2.).max(0.).floor();
    let right_x = left_w + GUTTER;
    TwoColumns { left_w, right_x, right_w: (width - right_x).max(0.) }
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
        for z in [MIN_ZOOM, READABLE_ZOOM, 0.8, 1.0] {
            assert!((zoom_for(column_width_at(z)) - z).abs() < 1e-4, "{z}");
        }
    }

    #[test]
    fn zoom_hands_over_from_cropping_to_shrinking() {
        let edge = column_width_at(1.0);
        assert_eq!(zoom_for(edge), 1.0);
        assert_eq!(zoom_for(edge + 50.0), 1.0);
        let below = zoom_for(edge - 20.0);
        assert!(below < 1.0 && below > 0.95, "{below}");
        // The whole page fits from 8.5 in + 16 px: then it grows past 1.0.
        assert!(zoom_for(8.5 * 96.0 + 16.0 + 50.0) > 1.0);
    }

    /// The mark, column and `*` are inside a list `w` wide at zoom `z`.
    fn all_visible(w: f32, z: f32) -> bool {
        let left = page_left_at(w, z);
        let (l, r) = visible_at(z);
        left + l >= -0.01 && left + r <= w + 0.01
    }

    #[test]
    fn page_left_centres_crops_and_never_leaves_a_gap() {
        let page = 8.5 * 96.0;
        // Room to spare: centred.
        assert_eq!(page_left_at(page + 100.0, 1.0), 50.0);
        assert_eq!(page_left_at(page, 1.0), 0.0);
        // A little short: the margins are cropped, the marks stay, and the
        // page's right edge stays at the list's.
        let left = page_left_at(700.0, 1.0);
        assert!(left < 0.0 && all_visible(700.0, 1.0) && left + page >= 700.0, "{left}");
        // Just short of the page: cropping is only what doesn't fit.
        let left = page_left_at(page - 10.0, 1.0);
        assert!((-10.0..0.0).contains(&left), "{left}");
        // Whatever width the editor fits to, the review mark (a fixed 26 px
        // left of the column) is never cut, down to the readable floor.
        let mut w = column_width_at(READABLE_ZOOM);
        while w < 1200.0 {
            assert!(all_visible(w, zoom_for(w)), "{w}");
            w += 3.0;
        }
    }

    #[test]
    fn two_columns_split_evenly_around_the_gutter() {
        let c = two_columns(1000.0);
        assert_eq!(c.left_w + GUTTER + c.right_w, 1000.0);
        assert!((c.left_w - c.right_w).abs() <= 1.0);
        assert_eq!(c.right_x, c.left_w + GUTTER);
        assert_eq!(two_columns(10.0).left_w, 0.0);
    }

    #[test]
    fn caps_keeps_byte_offsets() {
        for s in ["cesar", "señor ñandú", "straße", "ǆ"] {
            assert_eq!(display_caps(s).len(), s.len(), "{s}");
        }
        assert_eq!(display_caps("señor"), "SEÑOR");
    }
}
