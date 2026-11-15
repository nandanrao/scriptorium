//! Colours, carried over from script-review (purple accent, paper on a grey desk).
//! Light only for now.

use gpui::Rgba;

pub const DESK: Rgba = rgb_const(0xe3e5ea);
pub const CHROME: Rgba = rgb_const(0xeef0f3);
pub const SIDE: Rgba = rgb_const(0xe8eaee);
pub const PAPER: Rgba = rgb_const(0xffffff);
pub const INK: Rgba = rgb_const(0x1c1f24);
pub const INK_2: Rgba = rgb_const(0x555c67);
pub const INK_3: Rgba = rgb_const(0x8b919b);
pub const RULE: Rgba = rgb_const(0xd3d7de);
pub const ACCENT: Rgba = rgb_const(0x5b3fa3);
pub const ACCENT_SOFT: Rgba = rgba_const(0x5b3fa31a);
pub const OK: Rgba = rgb_const(0x2c7a4b);
pub const OK_SOFT: Rgba = rgba_const(0x2c7a4b22);
pub const OWN: Rgba = rgb_const(0x9a5f0c);
pub const WARN: Rgba = rgb_const(0xa2361f);
pub const WARN_SOFT: Rgba = rgba_const(0xa2361f1a);
pub const INS: Rgba = rgba_const(0x2c7a4b33);
pub const DEL: Rgba = rgba_const(0xa2361f26);
/// A block's BEFORE lines on the card: struck through in this red.
pub const STRUCK: Rgba = rgb_const(0xb3261e);
/// A block's AFTER lines on the card: this green.
pub const ADDED: Rgba = rgb_const(0x1f6e3f);

const fn rgb_const(hex: u32) -> Rgba {
    rgba_const((hex << 8) | 0xff)
}

const fn rgba_const(hex: u32) -> Rgba {
    Rgba {
        r: ((hex >> 24) & 0xff) as f32 / 255.0,
        g: ((hex >> 16) & 0xff) as f32 / 255.0,
        b: ((hex >> 8) & 0xff) as f32 / 255.0,
        a: (hex & 0xff) as f32 / 255.0,
    }
}
