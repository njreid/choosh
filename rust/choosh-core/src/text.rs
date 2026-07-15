//! Editor-boundary conversion and feedback-loop suppression.

use crate::actor::Generation;
use crate::ports::{PortError, PortResult};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Utf16Range {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ByteEdit {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}

/// Converts a UTF-16 code-unit range from an Android editor into Rust byte offsets.
///
/// # Errors
///
/// Returns `invalid_utf16_range` for reversed ranges, out-of-bounds indices, or
/// indices that split a surrogate pair. The input text is never modified.
pub fn translate_utf16_edit(
    text: &str,
    range: Utf16Range,
    replacement: String,
) -> PortResult<ByteEdit> {
    if range.start > range.end {
        return Err(PortError::new("invalid_utf16_range", false));
    }
    let start = utf16_index_to_byte(text, range.start)?;
    let end = utf16_index_to_byte(text, range.end)?;
    Ok(ByteEdit {
        start,
        end,
        replacement,
    })
}

fn utf16_index_to_byte(text: &str, requested: usize) -> PortResult<usize> {
    let mut utf16_index = 0;
    for (byte_index, character) in text.char_indices() {
        if requested == utf16_index {
            return Ok(byte_index);
        }
        let next = utf16_index + character.len_utf16();
        if requested < next {
            return Err(PortError::new("invalid_utf16_range", false));
        }
        utf16_index = next;
    }
    if requested == utf16_index {
        Ok(text.len())
    } else {
        Err(PortError::new("invalid_utf16_range", false))
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SuppressionToken {
    pub generation: Generation,
    pub sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditorChange {
    Local(ByteEdit),
    Suppressed,
}

/// State local to one editor binding. Tokens are generation-scoped, bounded,
/// opaque to UI code, and consumed at most once.
#[derive(Debug)]
pub struct EditorAdapter {
    generation: Generation,
    next_sequence: u64,
    pending: HashSet<SuppressionToken>,
    pending_limit: usize,
}

impl EditorAdapter {
    /// Creates an adapter with a positive bound on outstanding projections.
    ///
    /// # Errors
    ///
    /// Returns `invalid_suppression_limit` when the bound is zero.
    pub fn new(generation: Generation, pending_limit: usize) -> PortResult<Self> {
        if pending_limit == 0 {
            return Err(PortError::new("invalid_suppression_limit", false));
        }
        Ok(Self {
            generation,
            next_sequence: 1,
            pending: HashSet::new(),
            pending_limit,
        })
    }

    #[must_use]
    pub const fn generation(&self) -> Generation {
        self.generation
    }

    #[must_use]
    pub fn pending_projections(&self) -> usize {
        self.pending.len()
    }

    /// Allocates a one-shot token before applying a programmatic editor update.
    ///
    /// # Errors
    ///
    /// Returns `suppression_limit_exceeded` when too many projection callbacks are
    /// outstanding, or `suppression_sequence_overflow` instead of wrapping.
    pub fn begin_projection(&mut self) -> PortResult<SuppressionToken> {
        if self.pending.len() >= self.pending_limit {
            return Err(PortError::new("suppression_limit_exceeded", true));
        }
        let token = SuppressionToken {
            generation: self.generation,
            sequence: self.next_sequence,
        };
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| PortError::new("suppression_sequence_overflow", false))?;
        self.pending.insert(token);
        Ok(token)
    }

    /// Removes a projection token when the platform reports that no callback will
    /// arrive. The operation is idempotent.
    pub fn abandon_projection(&mut self, token: SuppressionToken) {
        self.pending.remove(&token);
    }

    /// Classifies one editor callback and translates genuine user edits.
    ///
    /// # Errors
    ///
    /// Returns `stale_suppression_token` for an unknown, consumed, or old-generation
    /// token. Genuine edits return the range-conversion errors documented by
    /// [`translate_utf16_edit`].
    pub fn classify_change(
        &mut self,
        text_before: &str,
        range: Utf16Range,
        replacement: String,
        suppression: Option<SuppressionToken>,
    ) -> PortResult<EditorChange> {
        if let Some(token) = suppression {
            if !self.pending.remove(&token) {
                return Err(PortError::new("stale_suppression_token", false));
            }
            return Ok(EditorChange::Suppressed);
        }
        translate_utf16_edit(text_before, range, replacement).map(EditorChange::Local)
    }

    /// Rebinds the adapter after engine recreation and invalidates all old tokens.
    ///
    /// # Errors
    ///
    /// Returns `stale_generation` unless the new generation strictly increases.
    pub fn rebind(&mut self, generation: Generation) -> PortResult<()> {
        if generation <= self.generation {
            return Err(PortError::new("stale_generation", false));
        }
        self.generation = generation;
        self.next_sequence = 1;
        self.pending.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ByteEdit, EditorAdapter, EditorChange, SuppressionToken, Utf16Range, translate_utf16_edit,
    };

    #[test]
    fn translates_ascii_and_unicode_code_units_to_byte_offsets() {
        assert_eq!(
            translate_utf16_edit("a😀b", Utf16Range { start: 1, end: 3 }, "🙂".to_owned(),),
            Ok(ByteEdit {
                start: 1,
                end: 5,
                replacement: "🙂".to_owned(),
            })
        );
        assert_eq!(
            translate_utf16_edit("e\u{301}", Utf16Range { start: 1, end: 2 }, String::new(),),
            Ok(ByteEdit {
                start: 1,
                end: 3,
                replacement: String::new(),
            })
        );
    }

    #[test]
    fn rejects_surrogate_splits_reversed_ranges_and_out_of_bounds_indices() {
        for range in [
            Utf16Range { start: 2, end: 3 },
            Utf16Range { start: 3, end: 2 },
            Utf16Range { start: 0, end: 5 },
        ] {
            assert_eq!(
                translate_utf16_edit("a😀b", range, String::new())
                    .unwrap_err()
                    .code,
                "invalid_utf16_range"
            );
        }
    }

    #[test]
    fn programmatic_callback_is_suppressed_exactly_once() {
        let mut adapter = EditorAdapter::new(7, 2).unwrap();
        let token = adapter.begin_projection().unwrap();
        assert_eq!(adapter.pending_projections(), 1);
        assert_eq!(
            adapter.classify_change(
                "before",
                Utf16Range { start: 0, end: 6 },
                "after".to_owned(),
                Some(token),
            ),
            Ok(EditorChange::Suppressed)
        );
        assert_eq!(adapter.pending_projections(), 0);
        assert_eq!(
            adapter
                .classify_change(
                    "before",
                    Utf16Range { start: 0, end: 6 },
                    "after".to_owned(),
                    Some(token),
                )
                .unwrap_err()
                .code,
            "stale_suppression_token"
        );
    }

    #[test]
    fn invalid_token_does_not_suppress_or_consume_a_real_projection() {
        let mut adapter = EditorAdapter::new(1, 1).unwrap();
        let token = adapter.begin_projection().unwrap();
        let forged = SuppressionToken {
            generation: 1,
            sequence: token.sequence + 1,
        };
        assert_eq!(
            adapter
                .classify_change(
                    "a",
                    Utf16Range { start: 0, end: 1 },
                    "b".to_owned(),
                    Some(forged),
                )
                .unwrap_err()
                .code,
            "stale_suppression_token"
        );
        assert_eq!(adapter.pending_projections(), 1);
        assert_eq!(
            adapter.classify_change(
                "a",
                Utf16Range { start: 0, end: 1 },
                "b".to_owned(),
                Some(token),
            ),
            Ok(EditorChange::Suppressed)
        );
    }

    #[test]
    fn user_change_translates_without_allocating_suppression_state() {
        let mut adapter = EditorAdapter::new(1, 1).unwrap();
        assert_eq!(
            adapter.classify_change(
                "a😀b",
                Utf16Range { start: 3, end: 4 },
                "c".to_owned(),
                None,
            ),
            Ok(EditorChange::Local(ByteEdit {
                start: 5,
                end: 6,
                replacement: "c".to_owned(),
            }))
        );
        assert_eq!(adapter.pending_projections(), 0);
    }

    #[test]
    fn pending_tokens_are_bounded_and_invalidated_on_rebind() {
        let mut adapter = EditorAdapter::new(1, 1).unwrap();
        let old = adapter.begin_projection().unwrap();
        assert_eq!(
            adapter.begin_projection().unwrap_err().code,
            "suppression_limit_exceeded"
        );
        assert_eq!(adapter.rebind(2), Ok(()));
        assert_eq!(adapter.pending_projections(), 0);
        assert_eq!(
            adapter
                .classify_change(
                    "a",
                    Utf16Range { start: 0, end: 1 },
                    "b".to_owned(),
                    Some(old),
                )
                .unwrap_err()
                .code,
            "stale_suppression_token"
        );
        assert_eq!(adapter.rebind(2).unwrap_err().code, "stale_generation");
    }
}
