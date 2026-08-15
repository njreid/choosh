//! The terminal grid: a fixed-size (cols x rows) array of styled cells,
//! plus per-row damage tracking so the renderer only re-shapes rows that
//! actually changed — porting Zelland's per-row `row_cache`/dirty-bit
//! design (`njreid/zelland@8bf9cf5:src-tauri/src/renderer/mod.rs`'s
//! `row_cache: Vec<Vec<CellRun>>` and `ghostty.rs`'s per-row `dirty` flag
//! from `GhosttyRenderStateRowData_..._DIRTY`) onto a pure-Rust grid
//! instead of libghostty's C render-state iterator.
//!
//! Per `docs/specs/terminal-experience.md`, Zellij (not this client) owns
//! scrollback, so — matching Zelland's own "Virtual Viewport &
//! Zero-Scrollback Optimization" (`WGPU_GHOSTTY_PLAN.md`) — this grid holds
//! only the visible viewport, never a scrollback buffer.

use crate::color::Color;

/// One character cell. `text` holds a full grapheme cluster (not just one
/// `char`) so combining marks and other multi-codepoint clusters render as
/// a single glyph run, per `terminal-experience.md`'s "wide and combining
/// characters" correctness requirement.
// Each bool is an independent SGR/DEC attribute a real terminal cell
// carries simultaneously (bold, italic, underline, ...); modelling them as
// a state machine or enum would just re-derive the same cross-product.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub text: String,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub inverse: bool,
    /// `true` for the leading half of a double-width (e.g. CJK) glyph;
    /// the following cell is a zero-width continuation placeholder so
    /// column arithmetic stays cell-accurate.
    pub wide: bool,
    /// `true` for the placeholder cell following a wide glyph. The
    /// renderer skips drawing these — the wide glyph itself is drawn in
    /// the two-cell box starting at the preceding cell.
    pub wide_continuation: bool,
    /// Set by OSC 8 (`\x1b]8;;URL\x1b\\`); empty when the cell carries no
    /// hyperlink. Selection/copy and a future tap-to-open action read this.
    pub hyperlink: Option<String>,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            text: String::new(),
            fg: Color::Default,
            bg: Color::Default,
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            inverse: false,
            wide: false,
            wide_continuation: false,
            hyperlink: None,
        }
    }
}

impl Cell {
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.text.is_empty() || self.text == " "
    }
}

/// Cursor shapes a host can request via DECSCUSR (`\x1b[<n> q`), used by
/// the renderer to pick which cursor shader geometry to draw.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Cursor {
    pub col: u16,
    pub row: u16,
    pub visible: bool,
    pub shape: CursorShape,
    pub blink: bool,
}

/// A fixed-size grid of cells representing exactly the visible viewport.
pub struct Grid {
    cols: u16,
    rows: u16,
    cells: Vec<Cell>,
    /// One bit per row: `true` means the row's content differs from what
    /// the renderer last drew and its glyph run must be rebuilt.
    dirty: Vec<bool>,
}

impl Grid {
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        let count = usize::from(cols) * usize::from(rows);
        Self { cols, rows, cells: vec![Cell::default(); count], dirty: vec![true; usize::from(rows)] }
    }

    #[must_use]
    pub fn cols(&self) -> u16 {
        self.cols
    }

    #[must_use]
    pub fn rows(&self) -> u16 {
        self.rows
    }

    fn index(&self, col: u16, row: u16) -> Option<usize> {
        if col < self.cols && row < self.rows {
            Some(usize::from(row) * usize::from(self.cols) + usize::from(col))
        } else {
            None
        }
    }

    #[must_use]
    pub fn cell(&self, col: u16, row: u16) -> Option<&Cell> {
        self.index(col, row).map(|i| &self.cells[i])
    }

    pub fn set_cell(&mut self, col: u16, row: u16, cell: Cell) {
        if let Some(i) = self.index(col, row) {
            self.cells[i] = cell;
            self.mark_dirty(row);
        }
    }

    #[must_use]
    pub fn row(&self, row: u16) -> &[Cell] {
        if row >= self.rows {
            return &[];
        }
        let start = usize::from(row) * usize::from(self.cols);
        &self.cells[start..start + usize::from(self.cols)]
    }

    pub fn mark_dirty(&mut self, row: u16) {
        if let Some(flag) = self.dirty.get_mut(usize::from(row)) {
            *flag = true;
        }
    }

    pub fn mark_all_dirty(&mut self) {
        self.dirty.iter_mut().for_each(|d| *d = true);
    }

    #[must_use]
    pub fn is_dirty(&self, row: u16) -> bool {
        self.dirty.get(usize::from(row)).copied().unwrap_or(false)
    }

    pub fn clear_dirty(&mut self, row: u16) {
        if let Some(flag) = self.dirty.get_mut(usize::from(row)) {
            *flag = false;
        }
    }

    /// Clears one row to blank cells (used by EL/ED and scroll).
    pub fn clear_row(&mut self, row: u16) {
        if row >= self.rows {
            return;
        }
        let start = usize::from(row) * usize::from(self.cols);
        for cell in &mut self.cells[start..start + usize::from(self.cols)] {
            *cell = Cell::default();
        }
        self.mark_dirty(row);
    }

    /// Clears columns `[from, to)` on one row.
    pub fn clear_row_range(&mut self, row: u16, from: u16, to: u16) {
        let to = to.min(self.cols);
        for col in from..to {
            self.set_cell(col, row, Cell::default());
        }
    }

    /// Scrolls the whole grid up by `count` rows (used on LF at the bottom
    /// of the scroll region and by IND/`RI`), discarding the top rows —
    /// per this module's doc comment, there is no scrollback to append
    /// them to; Zellij retains the real history.
    pub fn scroll_up(&mut self, top: u16, bottom: u16, count: u16) {
        let top = top.min(self.rows.saturating_sub(1));
        let bottom = bottom.min(self.rows.saturating_sub(1));
        if top >= bottom {
            return;
        }
        let count = count.min(bottom - top + 1);
        for row in top..=(bottom - count) {
            let src = self.row(row + count).to_vec();
            let start = usize::from(row) * usize::from(self.cols);
            self.cells[start..start + usize::from(self.cols)].clone_from_slice(&src);
            self.mark_dirty(row);
        }
        for row in (bottom - count + 1)..=bottom {
            self.clear_row(row);
        }
    }

    pub fn scroll_down(&mut self, top: u16, bottom: u16, count: u16) {
        let top = top.min(self.rows.saturating_sub(1));
        let bottom = bottom.min(self.rows.saturating_sub(1));
        if top >= bottom {
            return;
        }
        let count = count.min(bottom - top + 1);
        let mut row = bottom;
        while row >= top + count {
            let src = self.row(row - count).to_vec();
            let start = usize::from(row) * usize::from(self.cols);
            self.cells[start..start + usize::from(self.cols)].clone_from_slice(&src);
            self.mark_dirty(row);
            row -= 1;
        }
        for row in top..(top + count) {
            self.clear_row(row);
        }
    }

    /// Renders every visible row as plain text, one row per line, trailing
    /// blank cells on each row trimmed and wide-glyph continuation
    /// placeholder cells skipped (so a double-width CJK glyph contributes
    /// one character, not two). This is the minimal accessible-text
    /// equivalent of the GPU-rendered grid — Kotlin's
    /// `TerminalSurfaceView` exposes it to `TalkBack` via a virtual
    /// accessibility node (`docs/accessibility-device-report.md`'s item 1,
    /// gap 2: "the Terminal surface has zero accessible content"), since a
    /// screen reader has no other way to read a `SurfaceView`'s pixels.
    /// Deliberately row-by-row plain text, not styled runs — accessibility
    /// content, unlike the renderer's [`Cell`] runs, has no use for
    /// color/bold/italic.
    #[must_use]
    pub fn visible_text(&self) -> String {
        let mut lines: Vec<String> = Vec::with_capacity(usize::from(self.rows));
        for row in 0..self.rows {
            let mut line = String::new();
            for cell in self.row(row) {
                if cell.wide_continuation {
                    continue;
                }
                line.push_str(if cell.text.is_empty() { " " } else { cell.text.as_str() });
            }
            while line.ends_with(' ') {
                line.pop();
            }
            lines.push(line);
        }
        lines.join("\n")
    }

    /// Resizes in place, preserving the top-left overlapping region and
    /// re-marking everything dirty (a resize is always a full redraw, per
    /// `terminal-experience.md`'s "render only after damage ... resize").
    pub fn resize(&mut self, cols: u16, rows: u16) {
        if cols == self.cols && rows == self.rows {
            return;
        }
        let mut next = Grid::new(cols, rows);
        for row in 0..self.rows.min(rows) {
            for col in 0..self.cols.min(cols) {
                if let Some(cell) = self.cell(col, row) {
                    next.set_cell(col, row, cell.clone());
                }
            }
        }
        *self = next;
        self.mark_all_dirty();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_grid_is_blank_and_fully_dirty() {
        let grid = Grid::new(4, 2);
        assert!(grid.cell(0, 0).unwrap().is_blank());
        assert!(grid.is_dirty(0) && grid.is_dirty(1));
    }

    #[test]
    fn set_cell_marks_only_its_row_dirty() {
        let mut grid = Grid::new(4, 2);
        grid.clear_dirty(0);
        grid.clear_dirty(1);
        grid.set_cell(1, 0, Cell { text: "x".into(), ..Cell::default() });
        assert!(grid.is_dirty(0));
        assert!(!grid.is_dirty(1));
    }

    #[test]
    fn scroll_up_discards_top_and_blanks_bottom() {
        let mut grid = Grid::new(2, 3);
        for row in 0..3 {
            grid.set_cell(0, row, Cell { text: row.to_string(), ..Cell::default() });
        }
        grid.scroll_up(0, 2, 1);
        assert_eq!(grid.cell(0, 0).unwrap().text, "1");
        assert_eq!(grid.cell(0, 1).unwrap().text, "2");
        assert!(grid.cell(0, 2).unwrap().is_blank());
    }

    #[test]
    fn visible_text_trims_trailing_blanks_and_joins_rows_with_newlines() {
        let mut grid = Grid::new(5, 2);
        grid.set_cell(0, 0, Cell { text: "h".into(), ..Cell::default() });
        grid.set_cell(1, 0, Cell { text: "i".into(), ..Cell::default() });
        grid.set_cell(0, 1, Cell { text: "!".into(), ..Cell::default() });
        assert_eq!(grid.visible_text(), "hi\n!");
    }

    #[test]
    fn visible_text_skips_wide_continuation_cells() {
        let mut grid = Grid::new(4, 1);
        grid.set_cell(0, 0, Cell { text: "你".into(), wide: true, ..Cell::default() });
        grid.set_cell(1, 0, Cell { wide_continuation: true, ..Cell::default() });
        grid.set_cell(2, 0, Cell { text: "x".into(), ..Cell::default() });
        assert_eq!(grid.visible_text(), "你x");
    }

    #[test]
    fn resize_preserves_overlap_and_marks_all_dirty() {
        let mut grid = Grid::new(3, 3);
        grid.set_cell(0, 0, Cell { text: "a".into(), ..Cell::default() });
        for row in 0..3 {
            grid.clear_dirty(row);
        }
        grid.resize(2, 2);
        assert_eq!(grid.cell(0, 0).unwrap().text, "a");
        assert!(grid.is_dirty(0) && grid.is_dirty(1));
        assert_eq!(grid.cols(), 2);
        assert_eq!(grid.rows(), 2);
    }
}
