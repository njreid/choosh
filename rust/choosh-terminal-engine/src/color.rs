//! Terminal colour representation shared by the grid and the renderer.
//!
//! Mirrors the three-way colour tag Zelland's `GhosttyStyleColor` carried
//! (`GHOSTTY_STYLE_COLOR_NONE`/`_PALETTE`/`_RGB` in
//! `njreid/zelland@8bf9cf5:src-tauri/src/ghostty.rs`) so the renderer's
//! `ghostty_color_to_rgb`/`ansi_palette_color` logic ported almost verbatim
//! into [`Color::to_rgb`] below.

/// A terminal colour as carried by SGR sequences: unset (renderer picks the
/// theme default), one of the 256 indexed colours, or a direct 24-bit RGB
/// triple (SGR 38/48;2;r;g;b).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl Color {
    /// Resolves to a concrete RGB triple. `default_rgb` is the caller's
    /// theme default (white foreground / black background, typically) used
    /// when no colour was ever set — matching Zelland's renderer, which
    /// substituted white/black at the same point (`renderer/mod.rs`'s
    /// `ghostty_color_to_rgb` returning `None` for
    /// `GHOSTTY_STYLE_COLOR_NONE`, resolved by its caller).
    #[must_use]
    pub fn to_rgb(self, default_rgb: (u8, u8, u8)) -> (u8, u8, u8) {
        match self {
            Color::Default => default_rgb,
            Color::Indexed(index) => palette_color(index),
            Color::Rgb(r, g, b) => (r, g, b),
        }
    }
}

/// Maps a 256-colour palette index to an RGB triple using the standard
/// xterm palette. Ported from Zelland's `renderer/mod.rs::ansi_palette_color`.
#[allow(clippy::cast_possible_truncation)] // every arm's arithmetic is bounded to u8 range by construction
#[must_use]
pub fn palette_color(index: u8) -> (u8, u8, u8) {
    const PALETTE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (170, 0, 0),
        (0, 170, 0),
        (170, 170, 0),
        (0, 0, 170),
        (170, 0, 170),
        (0, 170, 170),
        (170, 170, 170),
        (85, 85, 85),
        (255, 85, 85),
        (85, 255, 85),
        (255, 255, 85),
        (85, 85, 255),
        (255, 85, 255),
        (85, 255, 255),
        (255, 255, 255),
    ];
    let idx = index as usize;
    if idx < 16 {
        return PALETTE[idx];
    }
    if idx < 232 {
        let i = idx - 16;
        let to_level = |v: usize| if v == 0 { 0u8 } else { (55 + v * 40) as u8 };
        return (to_level(i / 36), to_level((i / 6) % 6), to_level(i % 6));
    }
    let v = (8 + (idx - 232) * 10) as u8;
    (v, v, v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_color_resolves_to_caller_default() {
        assert_eq!(Color::Default.to_rgb((1, 2, 3)), (1, 2, 3));
    }

    #[test]
    fn indexed_black_and_white_match_ansi() {
        assert_eq!(Color::Indexed(0).to_rgb((9, 9, 9)), (0, 0, 0));
        assert_eq!(Color::Indexed(15).to_rgb((9, 9, 9)), (255, 255, 255));
    }

    #[test]
    fn rgb_passes_through() {
        assert_eq!(Color::Rgb(10, 20, 30).to_rgb((0, 0, 0)), (10, 20, 30));
    }

    #[test]
    fn greyscale_ramp_is_monotonic() {
        let (a, _, _) = palette_color(232);
        let (b, _, _) = palette_color(255);
        assert!(b > a);
    }
}
