//! Pure-Rust terminal engine: VT100/ANSI parsing, grid/cursor/mode state,
//! and key/mouse encoding, with no platform (JNI, wgpu, Android) code in
//! this crate — `choosh-android-bridge` drives it from JNI, and could
//! equally drive a `libghostty-vt` implementation of the same shape later.
//! See `terminal::Terminal`'s module doc for the real, reported Zig/
//! `libghostty-vt` attempt and why this crate is what backs
//! `docs/specs/terminal-experience.md` today instead.
#![forbid(unsafe_code)]

pub mod color;
pub mod grid;
pub mod keys;
pub mod modes;
pub mod mouse;
mod terminal;

pub use color::Color;
pub use grid::{Cell, Cursor, CursorShape, Grid};
pub use keys::{Key, Modifiers};
pub use modes::{Modes, MouseEncoding, MouseTracking};
pub use mouse::{MouseAction, MouseButton, MouseEvent};
pub use terminal::Terminal;

use vte::Parser;

/// The public entry point: feed it PTY bytes, read grid/cursor state back,
/// encode input to send to the PTY. One `Engine` per terminal surface, per
/// `terminal-experience.md`'s "one retained renderer per visible terminal
/// surface".
pub struct Engine {
    parser: Parser,
    terminal: Terminal,
}

impl Engine {
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        Self { parser: Parser::new(), terminal: Terminal::new(cols, rows) }
    }

    /// Feeds raw PTY output bytes through the VT parser, mutating grid,
    /// cursor, and mode state in place.
    pub fn write(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.terminal, bytes);
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.terminal.resize(cols, rows);
    }

    #[must_use]
    pub fn terminal(&self) -> &Terminal {
        &self.terminal
    }

    /// Rows whose content changed since the caller last called
    /// [`Self::clear_row_dirty`] — the renderer's per-row damage cache
    /// input, porting Zelland's `row_cache`/dirty-row design (see
    /// `terminal::Terminal`'s module doc).
    #[must_use]
    pub fn dirty_rows(&self) -> Vec<u16> {
        (0..self.terminal.grid().rows()).filter(|&row| self.terminal.grid().is_dirty(row)).collect()
    }

    pub fn clear_row_dirty(&mut self, row: u16) {
        // `Grid::clear_dirty` only needs `&mut Grid`; `Terminal` exposes no
        // direct mutable grid accessor to callers outside this crate on
        // purpose (all mutation goes through VT bytes), so this goes
        // through the one escape hatch built for exactly this.
        self.terminal.clear_row_dirty(row);
    }

    pub fn mark_all_dirty(&mut self) {
        self.terminal.mark_all_dirty();
    }

    /// Encodes a mouse event, or `None` if the terminal's current tracking
    /// mode means it must not be forwarded.
    #[must_use]
    pub fn encode_mouse(&self, event: MouseEvent) -> Option<Vec<u8>> {
        mouse::encode(&self.terminal.modes, event)
    }

    #[must_use]
    pub fn encode_key(&self, key: Key, mods: Modifiers) -> Vec<u8> {
        keys::encode(&self.terminal.modes, key, mods)
    }

    #[must_use]
    pub fn encode_paste(&self, paste: &[u8]) -> Vec<u8> {
        keys::encode_paste(&self.terminal.modes, paste)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirty_rows_clears_after_read() {
        let mut engine = Engine::new(10, 3);
        assert_eq!(engine.dirty_rows(), vec![0, 1, 2]);
        for row in 0..3 {
            engine.clear_row_dirty(row);
        }
        assert!(engine.dirty_rows().is_empty());
        engine.write(b"x");
        assert_eq!(engine.dirty_rows(), vec![0]);
    }

    #[test]
    fn resize_after_construction_marks_full_redraw() {
        let mut engine = Engine::new(10, 3);
        for row in 0..3 {
            engine.clear_row_dirty(row);
        }
        engine.resize(20, 6);
        assert_eq!(engine.dirty_rows().len(), 6);
    }
}
