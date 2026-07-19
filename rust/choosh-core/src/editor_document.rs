//! Composition of editor feedback suppression, UTF-16 ranges, and revisioned documents.

use std::collections::BTreeMap;

use crate::document::{ChangeId, Document, DocumentError, EditCommand, EditOutcome};
use crate::ports::PortError;
use crate::text::{EditorAdapter, EditorChange, SuppressionToken, Utf16Range};

#[derive(Clone, Debug, Eq, PartialEq)]
struct AcceptedEditorEdit {
    base_revision: u64,
    range: Utf16Range,
    replacement: String,
    resulting_revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditorDocumentOutcome {
    Suppressed,
    Document(EditOutcome),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditorDocumentError {
    Editor(PortError),
    Document(DocumentError),
}

/// Owns one editor binding and its revisioned document as one explicit seam.
///
/// UTF-16 offsets are always translated against the owned document text. A
/// bounded accepted-change index preserves idempotence when a duplicate editor
/// callback refers to text from an older revision.
#[derive(Debug)]
pub struct RevisionedEditor {
    adapter: EditorAdapter,
    document: Document,
    accepted: BTreeMap<ChangeId, AcceptedEditorEdit>,
}

impl RevisionedEditor {
    #[must_use]
    pub fn new(adapter: EditorAdapter, document: Document) -> Self {
        Self {
            adapter,
            document,
            accepted: BTreeMap::new(),
        }
    }

    #[must_use]
    pub const fn document(&self) -> &Document {
        &self.document
    }

    #[must_use]
    pub const fn adapter(&self) -> &EditorAdapter {
        &self.adapter
    }

    pub fn adapter_mut(&mut self) -> &mut EditorAdapter {
        &mut self.adapter
    }

    /// Classifies one platform callback and applies genuine user edits exactly once.
    ///
    /// Suppressed projection callbacks never enter document history. Duplicate
    /// user callbacks return the original resulting revision without translating
    /// their now-stale UTF-16 range against newer text.
    ///
    /// # Errors
    ///
    /// Returns editor errors for invalid ranges or suppression tokens, and
    /// document errors for bounded content, editability, or reused change IDs
    /// whose command differs.
    pub fn apply_change(
        &mut self,
        change_id: ChangeId,
        base_revision: u64,
        range: Utf16Range,
        replacement: String,
        suppression: Option<SuppressionToken>,
    ) -> Result<EditorDocumentOutcome, EditorDocumentError> {
        if suppression.is_none()
            && let Some(previous) = self.accepted.get(&change_id)
        {
            if previous.base_revision == base_revision
                && previous.range == range
                && previous.replacement == replacement
            {
                return Ok(EditorDocumentOutcome::Document(EditOutcome::Duplicate {
                    revision: previous.resulting_revision,
                }));
            }
            return Err(EditorDocumentError::Document(
                DocumentError::ChangeIdMismatch,
            ));
        }

        if suppression.is_none() && base_revision != self.document.revision() {
            return Ok(EditorDocumentOutcome::Document(
                EditOutcome::ResyncRequired {
                    current_revision: self.document.revision(),
                },
            ));
        }

        let classified = self
            .adapter
            .classify_change(
                self.document.text(),
                range,
                replacement.clone(),
                suppression,
            )
            .map_err(EditorDocumentError::Editor)?;
        let EditorChange::Local(edit) = classified else {
            return Ok(EditorDocumentOutcome::Suppressed);
        };
        let record = AcceptedEditorEdit {
            base_revision,
            range,
            replacement: replacement.clone(),
            resulting_revision: 0,
        };
        let outcome = self
            .document
            .apply(EditCommand {
                change_id: change_id.clone(),
                base_revision,
                start: edit.start,
                end: edit.end,
                replacement,
            })
            .map_err(EditorDocumentError::Document)?;
        if let EditOutcome::Applied { revision } | EditOutcome::Duplicate { revision } = outcome {
            let record = AcceptedEditorEdit {
                resulting_revision: revision,
                ..record
            };
            self.accepted.insert(change_id, record);
        }
        Ok(EditorDocumentOutcome::Document(outcome))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{DocumentId, DocumentLimits, RemoteIdentity};

    const LIMITS: DocumentLimits = DocumentLimits {
        max_utf8_bytes: 64,
        max_lines: 4,
        max_change_ids: 8,
    };

    fn editor(text: &str) -> RevisionedEditor {
        RevisionedEditor::new(
            EditorAdapter::new(7, 2).unwrap(),
            Document::open(
                DocumentId::new("doc"),
                text.to_owned(),
                RemoteIdentity::new("remote"),
                LIMITS,
            )
            .unwrap(),
        )
    }

    #[test]
    fn unicode_utf16_edit_applies_at_document_byte_boundaries() {
        let mut editor = editor("a😀b");
        assert_eq!(
            editor.apply_change(
                ChangeId::new("unicode"),
                0,
                Utf16Range { start: 1, end: 3 },
                "猫".into(),
                None,
            ),
            Ok(EditorDocumentOutcome::Document(EditOutcome::Applied {
                revision: 1
            }))
        );
        assert_eq!(editor.document().text(), "a猫b");
    }

    #[test]
    fn duplicate_old_revision_callback_is_idempotent() {
        let mut editor = editor("old");
        let apply = |editor: &mut RevisionedEditor| {
            editor.apply_change(
                ChangeId::new("same"),
                0,
                Utf16Range { start: 0, end: 3 },
                "new text".into(),
                None,
            )
        };
        assert_eq!(
            apply(&mut editor),
            Ok(EditorDocumentOutcome::Document(EditOutcome::Applied {
                revision: 1
            }))
        );
        assert_eq!(
            apply(&mut editor),
            Ok(EditorDocumentOutcome::Document(EditOutcome::Duplicate {
                revision: 1
            }))
        );
        assert_eq!(editor.document().text(), "new text");
    }

    #[test]
    fn stale_revision_requests_resync_without_range_translation() {
        let mut editor = editor("a😀b");
        editor
            .apply_change(
                ChangeId::new("first"),
                0,
                Utf16Range { start: 0, end: 1 },
                "A".into(),
                None,
            )
            .unwrap();
        assert_eq!(
            editor.apply_change(
                ChangeId::new("stale"),
                0,
                Utf16Range {
                    start: 99,
                    end: 100,
                },
                "ignored".into(),
                None,
            ),
            Ok(EditorDocumentOutcome::Document(
                EditOutcome::ResyncRequired {
                    current_revision: 1
                }
            ))
        );
        assert_eq!(editor.document().text(), "A😀b");
    }

    #[test]
    fn projection_callback_is_suppressed_without_document_mutation() {
        let mut editor = editor("before");
        let token = editor.adapter_mut().begin_projection().unwrap();
        assert_eq!(
            editor.apply_change(
                ChangeId::new("projection"),
                0,
                Utf16Range { start: 0, end: 6 },
                "after".into(),
                Some(token),
            ),
            Ok(EditorDocumentOutcome::Suppressed)
        );
        assert_eq!(editor.document().text(), "before");
        assert_eq!(editor.document().revision(), 0);
    }

    #[test]
    fn invalid_utf16_range_is_atomic() {
        let mut editor = editor("a😀b");
        let error = editor
            .apply_change(
                ChangeId::new("split"),
                0,
                Utf16Range { start: 2, end: 3 },
                String::new(),
                None,
            )
            .unwrap_err();
        assert_eq!(
            error,
            EditorDocumentError::Editor(PortError::new("invalid_utf16_range", false))
        );
        assert_eq!(editor.document().text(), "a😀b");
        assert_eq!(editor.document().revision(), 0);
    }
}
