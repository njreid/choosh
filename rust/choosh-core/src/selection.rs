//! Bounded logical terminal scrollback and selection extraction.

use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LogicalPosition {
    pub line_id: u64,
    pub cell: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Selection {
    pub start: LogicalPosition,
    pub end: LogicalPosition,
}

impl Selection {
    #[must_use]
    pub fn normalized(self) -> Self {
        if self.start <= self.end {
            self
        } else {
            Self {
                start: self.end,
                end: self.start,
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogicalCell {
    text: String,
    width: u8,
}

impl LogicalCell {
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub const fn width(&self) -> u8 {
        self.width
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LogicalLine {
    id: u64,
    cells: Vec<LogicalCell>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SelectionLimits {
    pub max_lines: usize,
    pub max_cells: usize,
    pub max_cells_per_line: usize,
    pub max_clipboard_bytes: usize,
    pub max_columns: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionError {
    InvalidLimits,
    InvalidColumns,
    StaleGeneration,
    LineIdExhausted,
    LineTooLong,
    UnknownLine,
    InvalidCell,
    EvictedSelection,
    ClipboardTooLarge,
}

#[derive(Debug)]
pub struct TerminalHistory {
    generation: u64,
    columns: usize,
    next_line_id: u64,
    lines: VecDeque<LogicalLine>,
    cells: usize,
    limits: SelectionLimits,
}

impl TerminalHistory {
    /// Creates an empty logical history with injected bounds.
    ///
    /// # Errors
    ///
    /// Rejects zero generation, dimensions, or resource limits.
    pub fn new(
        generation: u64,
        columns: usize,
        limits: SelectionLimits,
    ) -> Result<Self, SelectionError> {
        if generation == 0
            || columns == 0
            || columns > limits.max_columns
            || limits.max_lines == 0
            || limits.max_cells == 0
            || limits.max_cells_per_line == 0
            || limits.max_clipboard_bytes == 0
            || limits.max_columns == 0
        {
            return Err(SelectionError::InvalidLimits);
        }
        Ok(Self {
            generation,
            columns,
            next_line_id: 1,
            lines: VecDeque::new(),
            cells: 0,
            limits,
        })
    }

    /// Appends one logical line. Combining marks attach to the preceding cell;
    /// orphan combining marks receive a replacement base. Wide cells remain one
    /// logical selection unit with a display width of two.
    ///
    /// # Errors
    ///
    /// Rejects stale generations, line-ID exhaustion, or a line above its cell
    /// bound. Oldest complete lines are evicted to satisfy aggregate bounds.
    pub fn append_line(&mut self, generation: u64, text: &str) -> Result<u64, SelectionError> {
        self.validate_generation(generation)?;
        let cells = parse_cells(text, self.limits.max_cells_per_line)?;
        let id = self.next_line_id;
        self.next_line_id = self
            .next_line_id
            .checked_add(1)
            .ok_or(SelectionError::LineIdExhausted)?;
        self.cells = self.cells.saturating_add(cells.len());
        self.lines.push_back(LogicalLine { id, cells });
        while self.lines.len() > self.limits.max_lines || self.cells > self.limits.max_cells {
            if let Some(evicted) = self.lines.pop_front() {
                self.cells -= evicted.cells.len();
            }
        }
        Ok(id)
    }

    /// Changes only projection width; logical line IDs and cell offsets remain stable.
    ///
    /// # Errors
    ///
    /// Rejects stale generations or widths outside configured bounds.
    pub fn resize(&mut self, generation: u64, columns: usize) -> Result<(), SelectionError> {
        self.validate_generation(generation)?;
        if columns == 0 || columns > self.limits.max_columns {
            return Err(SelectionError::InvalidColumns);
        }
        self.columns = columns;
        Ok(())
    }

    /// Extracts an inclusive logical selection with newlines between lines.
    ///
    /// # Errors
    ///
    /// Rejects stale generations, evicted/unknown endpoints, invalid cell
    /// offsets, and output above the clipboard byte cap. No partial string is
    /// returned.
    pub fn extract(&self, generation: u64, selection: Selection) -> Result<String, SelectionError> {
        self.validate_generation(generation)?;
        let selection = selection.normalized();
        let first_retained = self.lines.front().map(|line| line.id);
        if first_retained.is_some_and(|first| selection.start.line_id < first) {
            return Err(SelectionError::EvictedSelection);
        }
        let start_index = self
            .lines
            .iter()
            .position(|line| line.id == selection.start.line_id)
            .ok_or(SelectionError::UnknownLine)?;
        let end_index = self
            .lines
            .iter()
            .position(|line| line.id == selection.end.line_id)
            .ok_or(SelectionError::UnknownLine)?;
        let start_line = &self.lines[start_index];
        let end_line = &self.lines[end_index];
        if selection.start.cell >= start_line.cells.len()
            || selection.end.cell >= end_line.cells.len()
        {
            return Err(SelectionError::InvalidCell);
        }
        let mut output = String::new();
        for index in start_index..=end_index {
            let line = &self.lines[index];
            let start = if index == start_index {
                selection.start.cell
            } else {
                0
            };
            let end = if index == end_index {
                selection.end.cell + 1
            } else {
                line.cells.len()
            };
            for cell in &line.cells[start..end] {
                checked_push(&mut output, &cell.text, self.limits.max_clipboard_bytes)?;
            }
            if index != end_index {
                checked_push(&mut output, "\n", self.limits.max_clipboard_bytes)?;
            }
        }
        Ok(output)
    }

    #[must_use]
    pub fn line_cells(&self, line_id: u64) -> Option<&[LogicalCell]> {
        self.lines
            .iter()
            .find(|line| line.id == line_id)
            .map(|line| line.cells.as_slice())
    }

    #[must_use]
    pub const fn columns(&self) -> usize {
        self.columns
    }

    fn validate_generation(&self, generation: u64) -> Result<(), SelectionError> {
        if generation == self.generation {
            Ok(())
        } else {
            Err(SelectionError::StaleGeneration)
        }
    }
}

fn parse_cells(text: &str, maximum: usize) -> Result<Vec<LogicalCell>, SelectionError> {
    let mut cells: Vec<LogicalCell> = Vec::new();
    for character in text.chars() {
        if is_combining(character) {
            if let Some(cell) = cells.last_mut() {
                cell.text.push(character);
            } else {
                cells.push(LogicalCell {
                    text: format!("\u{fffd}{character}"),
                    width: 1,
                });
            }
            continue;
        }
        if cells.len() == maximum {
            return Err(SelectionError::LineTooLong);
        }
        cells.push(LogicalCell {
            text: character.to_string(),
            width: if is_wide(character) { 2 } else { 1 },
        });
    }
    Ok(cells)
}

fn checked_push(output: &mut String, value: &str, maximum: usize) -> Result<(), SelectionError> {
    if output
        .len()
        .checked_add(value.len())
        .is_none_or(|length| length > maximum)
    {
        Err(SelectionError::ClipboardTooLarge)
    } else {
        output.push_str(value);
        Ok(())
    }
}

fn is_combining(character: char) -> bool {
    matches!(character as u32, 0x0300..=0x036f | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff | 0x20d0..=0x20ff | 0xfe20..=0xfe2f)
}

fn is_wide(character: char) -> bool {
    matches!(character as u32, 0x1100..=0x115f | 0x2329..=0x232a | 0x2e80..=0xa4cf | 0xac00..=0xd7a3 | 0xf900..=0xfaff | 0xfe10..=0xfe19 | 0xfe30..=0xfe6f | 0xff00..=0xff60 | 0xffe0..=0xffe6 | 0x1f300..=0x1faff | 0x20000..=0x3fffd)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(max_lines: usize, clipboard: usize) -> TerminalHistory {
        TerminalHistory::new(
            3,
            80,
            SelectionLimits {
                max_lines,
                max_cells: 16,
                max_cells_per_line: 8,
                max_clipboard_bytes: clipboard,
                max_columns: 200,
            },
        )
        .unwrap()
    }

    #[test]
    fn reversed_multiline_selection_normalizes_and_extracts() {
        let mut history = history(4, 32);
        let first = history.append_line(3, "abcd").unwrap();
        let second = history.append_line(3, "efgh").unwrap();
        let selection = Selection {
            start: LogicalPosition {
                line_id: second,
                cell: 1,
            },
            end: LogicalPosition {
                line_id: first,
                cell: 2,
            },
        };
        assert_eq!(history.extract(3, selection).unwrap(), "cd\nef");
    }

    #[test]
    fn logical_positions_survive_resize() {
        let mut history = history(4, 32);
        let line = history.append_line(3, "abcdef").unwrap();
        let selection = Selection {
            start: LogicalPosition {
                line_id: line,
                cell: 2,
            },
            end: LogicalPosition {
                line_id: line,
                cell: 4,
            },
        };
        history.resize(3, 2).unwrap();
        assert_eq!(history.extract(3, selection).unwrap(), "cde");
        assert_eq!(history.columns(), 2);
    }

    #[test]
    fn combining_and_wide_characters_are_atomic_cells() {
        let mut history = history(4, 32);
        let line = history.append_line(3, "e\u{301}界x").unwrap();
        let cells = history.line_cells(line).unwrap();
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0].text(), "e\u{301}");
        assert_eq!(cells[1].width(), 2);
        let selection = Selection {
            start: LogicalPosition {
                line_id: line,
                cell: 0,
            },
            end: LogicalPosition {
                line_id: line,
                cell: 1,
            },
        };
        assert_eq!(history.extract(3, selection).unwrap(), "e\u{301}界");
    }

    #[test]
    fn eviction_and_stale_generation_fail_closed() {
        let mut history = history(1, 32);
        let old = history.append_line(3, "old").unwrap();
        history.append_line(3, "new").unwrap();
        let selection = Selection {
            start: LogicalPosition {
                line_id: old,
                cell: 0,
            },
            end: LogicalPosition {
                line_id: old,
                cell: 1,
            },
        };
        assert_eq!(
            history.extract(3, selection),
            Err(SelectionError::EvictedSelection)
        );
        assert_eq!(
            history.extract(2, selection),
            Err(SelectionError::StaleGeneration)
        );
    }

    #[test]
    fn line_cell_and_clipboard_limits_are_atomic() {
        let mut history = history(2, 2);
        assert_eq!(
            history.append_line(3, "123456789"),
            Err(SelectionError::LineTooLong)
        );
        let line = history.append_line(3, "abc").unwrap();
        let selection = Selection {
            start: LogicalPosition {
                line_id: line,
                cell: 0,
            },
            end: LogicalPosition {
                line_id: line,
                cell: 2,
            },
        };
        assert_eq!(
            history.extract(3, selection),
            Err(SelectionError::ClipboardTooLarge)
        );
    }
}
