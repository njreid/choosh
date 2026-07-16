//! Small, bounded, renderer-independent terminal screen engine.

use std::collections::VecDeque;

const MAX_SEQUENCE_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Style {
    pub bold: bool,
    pub foreground: Option<u8>,
    pub background: Option<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cell {
    pub character: char,
    pub style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            character: ' ',
            style: Style::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Damage {
    pub row: usize,
    pub start_column: usize,
    pub end_column: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScreenSnapshot {
    pub rows: usize,
    pub columns: usize,
    pub cursor_row: usize,
    pub cursor_column: usize,
    pub alternate_screen: bool,
    cells: Vec<Cell>,
}

impl ScreenSnapshot {
    #[must_use]
    pub fn cell(&self, row: usize, column: usize) -> Option<Cell> {
        if row >= self.rows || column >= self.columns {
            None
        } else {
            self.cells.get(row * self.columns + column).copied()
        }
    }

    #[must_use]
    pub fn row_text(&self, row: usize) -> Option<String> {
        (row < self.rows).then(|| {
            self.cells[row * self.columns..(row + 1) * self.columns]
                .iter()
                .map(|cell| cell.character)
                .collect()
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VtError {
    InvalidDimensions,
    DimensionOverflow,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Buffer {
    cells: Vec<Cell>,
    cursor_row: usize,
    cursor_column: usize,
    style: Style,
}

impl Buffer {
    fn blank(rows: usize, columns: usize) -> Result<Self, VtError> {
        let length = rows
            .checked_mul(columns)
            .ok_or(VtError::DimensionOverflow)?;
        Ok(Self {
            cells: vec![Cell::default(); length],
            cursor_row: 0,
            cursor_column: 0,
            style: Style::default(),
        })
    }
}

#[derive(Debug)]
pub struct VtEngine {
    rows: usize,
    columns: usize,
    scrollback_limit: usize,
    scrollback: VecDeque<Vec<Cell>>,
    active: Buffer,
    saved_main: Option<Buffer>,
    pending: Vec<u8>,
    damaged: Vec<bool>,
}

impl VtEngine {
    /// Creates a screen with fixed positive dimensions and bounded scrollback.
    ///
    /// # Errors
    ///
    /// Returns an error for zero dimensions or multiplication overflow.
    pub fn new(rows: usize, columns: usize, scrollback_limit: usize) -> Result<Self, VtError> {
        if rows == 0 || columns == 0 {
            return Err(VtError::InvalidDimensions);
        }
        Ok(Self {
            rows,
            columns,
            scrollback_limit,
            scrollback: VecDeque::new(),
            active: Buffer::blank(rows, columns)?,
            saved_main: None,
            pending: Vec::new(),
            damaged: vec![true; rows],
        })
    }

    /// Feeds an arbitrary byte chunk. Incomplete UTF-8 and escape sequences are retained.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        let mut consumed = 0;
        while consumed < self.pending.len() {
            match self.pending[consumed] {
                0x1b => match escape_length(&self.pending[consumed..]) {
                    Sequence::Complete(length) => {
                        let sequence = self.pending[consumed..consumed + length].to_vec();
                        self.apply_escape(&sequence);
                        consumed += length;
                    }
                    Sequence::Incomplete if self.pending.len() - consumed <= MAX_SEQUENCE_BYTES => {
                        break;
                    }
                    Sequence::Incomplete => consumed += 1,
                },
                b'\n' => {
                    self.line_feed();
                    consumed += 1;
                }
                b'\r' => {
                    self.active.cursor_column = 0;
                    consumed += 1;
                }
                0x08 => {
                    self.active.cursor_column = self.active.cursor_column.saturating_sub(1);
                    consumed += 1;
                }
                byte if byte < 0x20 || byte == 0x7f => consumed += 1,
                _ => match next_character(&self.pending[consumed..]) {
                    Character::Complete(character, length) => {
                        self.put(character);
                        consumed += length;
                    }
                    Character::Incomplete => break,
                    Character::Invalid => {
                        self.put('\u{fffd}');
                        consumed += 1;
                    }
                },
            }
        }
        self.pending.drain(..consumed);
        if self.pending.len() > MAX_SEQUENCE_BYTES {
            self.pending.clear();
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> ScreenSnapshot {
        ScreenSnapshot {
            rows: self.rows,
            columns: self.columns,
            cursor_row: self.active.cursor_row,
            cursor_column: self.active.cursor_column,
            alternate_screen: self.saved_main.is_some(),
            cells: self.active.cells.clone(),
        }
    }

    /// Returns and clears row-level damage accumulated since the last call.
    pub fn take_damage(&mut self) -> Vec<Damage> {
        let mut result = Vec::new();
        for (row, damaged) in self.damaged.iter_mut().enumerate() {
            if *damaged {
                result.push(Damage {
                    row,
                    start_column: 0,
                    end_column: self.columns,
                });
                *damaged = false;
            }
        }
        result
    }

    #[must_use]
    pub fn scrollback_rows(&self) -> usize {
        self.scrollback.len()
    }

    /// Resizes the active screen, preserving its top-left intersection.
    ///
    /// # Errors
    ///
    /// Returns an error for zero dimensions or multiplication overflow, leaving
    /// the existing screen unchanged.
    pub fn resize(&mut self, rows: usize, columns: usize) -> Result<(), VtError> {
        if rows == 0 || columns == 0 {
            return Err(VtError::InvalidDimensions);
        }
        let mut resized = Buffer::blank(rows, columns)?;
        copy_intersection(
            &self.active,
            self.rows,
            self.columns,
            &mut resized,
            rows,
            columns,
        );
        resized.cursor_row = self.active.cursor_row.min(rows - 1);
        resized.cursor_column = self.active.cursor_column.min(columns - 1);
        resized.style = self.active.style;
        self.active = resized;
        self.rows = rows;
        self.columns = columns;
        self.damaged = vec![true; rows];
        Ok(())
    }

    fn put(&mut self, character: char) {
        if self.active.cursor_column >= self.columns {
            self.active.cursor_column = 0;
            self.line_feed();
        }
        let index = self.active.cursor_row * self.columns + self.active.cursor_column;
        self.active.cells[index] = Cell {
            character,
            style: self.active.style,
        };
        self.damaged[self.active.cursor_row] = true;
        self.active.cursor_column += 1;
    }

    fn line_feed(&mut self) {
        if self.active.cursor_row + 1 < self.rows {
            self.active.cursor_row += 1;
            return;
        }
        let removed: Vec<_> = self.active.cells.drain(..self.columns).collect();
        self.active
            .cells
            .extend(std::iter::repeat_n(Cell::default(), self.columns));
        if self.saved_main.is_none() && self.scrollback_limit > 0 {
            self.scrollback.push_back(removed);
            while self.scrollback.len() > self.scrollback_limit {
                self.scrollback.pop_front();
            }
        }
        self.damaged.fill(true);
    }

    fn apply_escape(&mut self, sequence: &[u8]) {
        if sequence.len() < 3 || sequence[1] != b'[' {
            return;
        }
        let final_byte = sequence[sequence.len() - 1];
        let body = &sequence[2..sequence.len() - 1];
        if body == b"?1049" && final_byte == b'h' {
            if self.saved_main.is_none() {
                self.saved_main = Some(self.active.clone());
                if let Ok(blank) = Buffer::blank(self.rows, self.columns) {
                    self.active = blank;
                    self.damaged.fill(true);
                }
            }
            return;
        }
        if body == b"?1049" && final_byte == b'l' {
            if let Some(main) = self.saved_main.take() {
                self.active = main;
                self.damaged.fill(true);
            }
            return;
        }
        let Some(parameters) = parse_parameters(body) else {
            return;
        };
        match final_byte {
            b'A' => {
                self.active.cursor_row =
                    self.active
                        .cursor_row
                        .saturating_sub(parameter(&parameters, 0, 1));
            }
            b'B' => {
                self.active.cursor_row =
                    (self.active.cursor_row + parameter(&parameters, 0, 1)).min(self.rows - 1);
            }
            b'C' => {
                self.active.cursor_column = (self.active.cursor_column
                    + parameter(&parameters, 0, 1))
                .min(self.columns - 1);
            }
            b'D' => {
                self.active.cursor_column =
                    self.active
                        .cursor_column
                        .saturating_sub(parameter(&parameters, 0, 1));
            }
            b'H' | b'f' => {
                self.active.cursor_row = parameter(&parameters, 0, 1)
                    .saturating_sub(1)
                    .min(self.rows - 1);
                self.active.cursor_column = parameter(&parameters, 1, 1)
                    .saturating_sub(1)
                    .min(self.columns - 1);
            }
            b'J' => self.erase_display(parameter(&parameters, 0, 0)),
            b'K' => self.erase_line(parameter(&parameters, 0, 0)),
            b'm' => self.sgr(&parameters),
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: usize) {
        let (start, end) = match mode {
            0 => (self.active.cursor_column.min(self.columns), self.columns),
            1 => (0, (self.active.cursor_column + 1).min(self.columns)),
            2 => (0, self.columns),
            _ => return,
        };
        let offset = self.active.cursor_row * self.columns;
        self.active.cells[offset + start..offset + end].fill(Cell::default());
        self.damaged[self.active.cursor_row] = true;
    }

    fn erase_display(&mut self, mode: usize) {
        match mode {
            2 => {
                self.active.cells.fill(Cell::default());
                self.damaged.fill(true);
            }
            0 => {
                self.erase_line(0);
                for row in self.active.cursor_row + 1..self.rows {
                    let start = row * self.columns;
                    self.active.cells[start..start + self.columns].fill(Cell::default());
                    self.damaged[row] = true;
                }
            }
            1 => {
                self.erase_line(1);
                for row in 0..self.active.cursor_row {
                    let start = row * self.columns;
                    self.active.cells[start..start + self.columns].fill(Cell::default());
                    self.damaged[row] = true;
                }
            }
            _ => {}
        }
    }

    fn sgr(&mut self, parameters: &[usize]) {
        let values = if parameters.is_empty() {
            &[0][..]
        } else {
            parameters
        };
        for value in values {
            match value {
                0 => self.active.style = Style::default(),
                1 => self.active.style.bold = true,
                22 => self.active.style.bold = false,
                30..=37 => self.active.style.foreground = u8::try_from(*value - 30).ok(),
                39 => self.active.style.foreground = None,
                40..=47 => self.active.style.background = u8::try_from(*value - 40).ok(),
                49 => self.active.style.background = None,
                _ => {}
            }
        }
    }
}

fn copy_intersection(
    source: &Buffer,
    source_rows: usize,
    source_columns: usize,
    target: &mut Buffer,
    target_rows: usize,
    target_columns: usize,
) {
    for row in 0..source_rows.min(target_rows) {
        let count = source_columns.min(target_columns);
        target.cells[row * target_columns..row * target_columns + count]
            .copy_from_slice(&source.cells[row * source_columns..row * source_columns + count]);
    }
}

enum Sequence {
    Complete(usize),
    Incomplete,
}

fn escape_length(bytes: &[u8]) -> Sequence {
    if bytes.len() == 1 {
        return Sequence::Incomplete;
    }
    if bytes[1] != b'[' {
        return Sequence::Complete(2);
    }
    bytes[2..]
        .iter()
        .position(|byte| (0x40..=0x7e).contains(byte))
        .map_or(Sequence::Incomplete, |position| {
            Sequence::Complete(position + 3)
        })
}

enum Character {
    Complete(char, usize),
    Incomplete,
    Invalid,
}

fn next_character(bytes: &[u8]) -> Character {
    let width = match bytes[0] {
        0x00..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return Character::Invalid,
    };
    if bytes.len() < width {
        return Character::Incomplete;
    }
    std::str::from_utf8(&bytes[..width])
        .ok()
        .and_then(|value| value.chars().next())
        .map_or(Character::Invalid, |character| {
            Character::Complete(character, width)
        })
}

fn parse_parameters(body: &[u8]) -> Option<Vec<usize>> {
    if body.is_empty() {
        return Some(Vec::new());
    }
    body.split(|byte| *byte == b';')
        .map(|part| {
            if part.is_empty() {
                Some(0)
            } else {
                std::str::from_utf8(part).ok()?.parse().ok()
            }
        })
        .collect()
}

fn parameter(parameters: &[usize], index: usize, default: usize) -> usize {
    match parameters.get(index).copied() {
        Some(0) | None => default,
        Some(value) => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_utf8_wrap_and_bounded_scrollback_are_deterministic() {
        let mut vt = VtEngine::new(2, 4, 1).unwrap();
        vt.feed(b"abc");
        vt.feed(&[0xc3]);
        vt.feed(&[0xa9, b'x', b'y', b'z', b'q', b'r']);
        assert_eq!(vt.snapshot().row_text(0).unwrap(), "xyzq");
        assert_eq!(vt.scrollback_rows(), 1);
    }

    #[test]
    fn cursor_erase_and_sgr_produce_expected_cells() {
        let mut vt = VtEngine::new(2, 5, 0).unwrap();
        vt.feed(b"hello\x1b[1;2H\x1b[1;31mX\x1b[0m\x1b[K");
        let snapshot = vt.snapshot();
        assert_eq!(snapshot.row_text(0).unwrap(), "hX   ");
        let styled = snapshot.cell(0, 1).unwrap();
        assert!(styled.style.bold);
        assert_eq!(styled.style.foreground, Some(1));
    }

    #[test]
    fn alternate_screen_restores_main_and_does_not_add_scrollback() {
        let mut vt = VtEngine::new(2, 4, 2).unwrap();
        vt.feed(b"main\x1b[?1049halt\nmore\x1b[?1049l");
        assert_eq!(vt.snapshot().row_text(0).unwrap(), "main");
        assert!(!vt.snapshot().alternate_screen);
        assert_eq!(vt.scrollback_rows(), 0);
    }

    #[test]
    fn resize_preserves_intersection_and_reports_full_damage() {
        let mut vt = VtEngine::new(2, 3, 0).unwrap();
        vt.take_damage();
        vt.feed(b"abc");
        vt.resize(3, 2).unwrap();
        assert_eq!(vt.snapshot().row_text(0).unwrap(), "ab");
        assert_eq!(vt.take_damage().len(), 3);
        assert!(vt.take_damage().is_empty());
    }

    #[test]
    fn malformed_and_incomplete_sequences_never_apply_partial_actions() {
        let mut vt = VtEngine::new(2, 8, 0).unwrap();
        vt.feed(b"A\x1b[2;");
        assert_eq!(vt.snapshot().row_text(0).unwrap(), "A       ");
        vt.feed(b"HZ");
        assert_eq!(vt.snapshot().cell(1, 0).unwrap().character, 'Z');
        vt.feed(&[0xff, b'Q']);
        assert_eq!(vt.snapshot().cell(1, 1).unwrap().character, '\u{fffd}');
        assert_eq!(vt.snapshot().cell(1, 2).unwrap().character, 'Q');
    }

    #[test]
    fn dimensions_and_erase_modes_are_bounded() {
        assert_eq!(
            VtEngine::new(0, 1, 0).unwrap_err(),
            VtError::InvalidDimensions
        );
        let mut vt = VtEngine::new(2, 3, 0).unwrap();
        vt.feed(b"abcdef\x1b[1;2H\x1b[1J");
        assert_eq!(vt.snapshot().row_text(0).unwrap(), "  c");
        assert_eq!(vt.snapshot().row_text(1).unwrap(), "def");
    }
}
