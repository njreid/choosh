//! Revisioned, bounded document buffers and save-state transitions.

use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DocumentId(String);

impl DocumentId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ChangeId(String);

impl ChangeId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SaveId(String);

impl SaveId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteIdentity(String);

impl RemoteIdentity {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentLimits {
    pub max_utf8_bytes: usize,
    pub max_lines: usize,
    pub max_change_ids: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadOnlyReason {
    UnsupportedEncoding,
    MixedLineEndings,
    Binary,
    Oversized,
    UnsupportedRemoteKind,
    PathChanged,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentState {
    Clean {
        remote: RemoteIdentity,
    },
    Dirty {
        base_remote: RemoteIdentity,
    },
    Saving {
        save_id: SaveId,
        base_remote: RemoteIdentity,
    },
    OfflineDirty {
        base_remote: RemoteIdentity,
    },
    Conflicted {
        base_remote: RemoteIdentity,
        observed_remote: RemoteIdentity,
    },
    ReadOnly {
        reason: ReadOnlyReason,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditCommand {
    pub change_id: ChangeId,
    pub base_revision: u64,
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditOutcome {
    Applied { revision: u64 },
    Duplicate { revision: u64 },
    ResyncRequired { current_revision: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaveSnapshot {
    pub document_id: DocumentId,
    pub save_id: SaveId,
    pub revision: u64,
    pub base_remote: RemoteIdentity,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SaveResult {
    Written {
        remote: RemoteIdentity,
    },
    IdentityMismatch {
        observed: RemoteIdentity,
    },
    TransportFailed {
        connected: bool,
        temp_may_remain: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentError {
    InvalidLimits,
    ContentTooLarge,
    TooManyLines,
    InvalidRange,
    SplitCodePoint,
    ChangeIdMismatch,
    ChangeHistoryFull,
    RevisionOverflow,
    NotEditable,
    NotSaveable,
    SaveAlreadyPending,
    NoMatchingSave,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Document {
    id: DocumentId,
    revision: u64,
    text: String,
    state: DocumentState,
    limits: DocumentLimits,
    changes: BTreeMap<ChangeId, EditCommand>,
    pending_save: Option<SaveSnapshot>,
}

impl Document {
    /// Opens a writable document at revision zero.
    ///
    /// # Errors
    ///
    /// Rejects zero limits or initial content outside the configured bounds.
    pub fn open(
        id: DocumentId,
        text: String,
        remote: RemoteIdentity,
        limits: DocumentLimits,
    ) -> Result<Self, DocumentError> {
        validate_limits(limits)?;
        validate_content(&text, limits)?;
        Ok(Self {
            id,
            revision: 0,
            text,
            state: DocumentState::Clean { remote },
            limits,
            changes: BTreeMap::new(),
            pending_save: None,
        })
    }

    /// Opens bounded content in a permanently read-only state.
    ///
    /// # Errors
    ///
    /// Rejects zero limits or initial content outside the configured bounds.
    pub fn open_read_only(
        id: DocumentId,
        text: String,
        reason: ReadOnlyReason,
        limits: DocumentLimits,
    ) -> Result<Self, DocumentError> {
        validate_limits(limits)?;
        validate_content(&text, limits)?;
        Ok(Self {
            id,
            revision: 0,
            text,
            state: DocumentState::ReadOnly { reason },
            limits,
            changes: BTreeMap::new(),
            pending_save: None,
        })
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub const fn state(&self) -> &DocumentState {
        &self.state
    }

    /// Applies one complete UTF-8 byte-range transaction.
    ///
    /// # Errors
    ///
    /// Rejects invalid boundaries, oversized results, non-editable states,
    /// revision overflow, exhausted change history, and reused IDs whose command
    /// differs. Stale revisions return [`EditOutcome::ResyncRequired`].
    pub fn apply(&mut self, command: EditCommand) -> Result<EditOutcome, DocumentError> {
        if let Some(previous) = self.changes.get(&command.change_id) {
            return if previous == &command {
                Ok(EditOutcome::Duplicate {
                    revision: command.base_revision + 1,
                })
            } else {
                Err(DocumentError::ChangeIdMismatch)
            };
        }
        if command.base_revision != self.revision {
            return Ok(EditOutcome::ResyncRequired {
                current_revision: self.revision,
            });
        }
        if !matches!(
            self.state,
            DocumentState::Clean { .. }
                | DocumentState::Dirty { .. }
                | DocumentState::Saving { .. }
                | DocumentState::OfflineDirty { .. }
        ) {
            return Err(DocumentError::NotEditable);
        }
        validate_range(&self.text, command.start, command.end)?;
        let resulting_len = self
            .text
            .len()
            .checked_sub(command.end - command.start)
            .and_then(|length| length.checked_add(command.replacement.len()))
            .ok_or(DocumentError::ContentTooLarge)?;
        if resulting_len > self.limits.max_utf8_bytes {
            return Err(DocumentError::ContentTooLarge);
        }
        if self.changes.len() == self.limits.max_change_ids {
            return Err(DocumentError::ChangeHistoryFull);
        }
        let next_revision = self
            .revision
            .checked_add(1)
            .ok_or(DocumentError::RevisionOverflow)?;
        let mut candidate = self.text.clone();
        candidate.replace_range(command.start..command.end, &command.replacement);
        validate_content(&candidate, self.limits)?;

        self.transition_after_edit();
        self.text = candidate;
        self.revision = next_revision;
        self.changes.insert(command.change_id.clone(), command);
        Ok(EditOutcome::Applied {
            revision: next_revision,
        })
    }

    fn transition_after_edit(&mut self) {
        if let DocumentState::Clean { remote } = &self.state {
            self.state = DocumentState::Dirty {
                base_remote: remote.clone(),
            };
        }
    }

    /// Records loss of transport without discarding local bytes.
    pub fn disconnect(&mut self) {
        match &self.state {
            DocumentState::Dirty { base_remote } | DocumentState::Saving { base_remote, .. } => {
                self.state = DocumentState::OfflineDirty {
                    base_remote: base_remote.clone(),
                };
                self.pending_save = None;
            }
            _ => {}
        }
    }

    /// Reconciles an offline buffer against one freshly observed identity.
    ///
    /// A matching identity leaves the document dirty; a mismatch conflicts.
    pub fn reconnect(&mut self, observed: RemoteIdentity) {
        if let DocumentState::OfflineDirty { base_remote } = &self.state {
            self.state = if base_remote == &observed {
                DocumentState::Dirty {
                    base_remote: base_remote.clone(),
                }
            } else {
                DocumentState::Conflicted {
                    base_remote: base_remote.clone(),
                    observed_remote: observed,
                }
            };
        }
    }

    /// Freezes a save snapshot while retaining the live editable buffer.
    ///
    /// # Errors
    ///
    /// Rejects non-dirty/offline states and a second concurrent save.
    pub fn begin_save(&mut self, save_id: SaveId) -> Result<SaveSnapshot, DocumentError> {
        if self.pending_save.is_some() {
            return Err(DocumentError::SaveAlreadyPending);
        }
        let base_remote = match &self.state {
            DocumentState::Dirty { base_remote } => base_remote.clone(),
            _ => return Err(DocumentError::NotSaveable),
        };
        let snapshot = SaveSnapshot {
            document_id: self.id.clone(),
            save_id: save_id.clone(),
            revision: self.revision,
            base_remote: base_remote.clone(),
            text: self.text.clone(),
        };
        self.state = DocumentState::Saving {
            save_id,
            base_remote,
        };
        self.pending_save = Some(snapshot.clone());
        Ok(snapshot)
    }

    /// Applies a terminal result for the matching frozen save.
    ///
    /// # Errors
    ///
    /// Rejects results when no save is pending or when the save ID differs.
    pub fn finish_save(
        &mut self,
        save_id: &SaveId,
        result: SaveResult,
    ) -> Result<(), DocumentError> {
        let snapshot = self
            .pending_save
            .as_ref()
            .filter(|snapshot| &snapshot.save_id == save_id)
            .ok_or(DocumentError::NoMatchingSave)?
            .clone();
        self.pending_save = None;
        self.state = match result {
            SaveResult::Written { remote } if snapshot.revision == self.revision => {
                DocumentState::Clean { remote }
            }
            SaveResult::Written { remote } => DocumentState::Dirty {
                base_remote: remote,
            },
            SaveResult::IdentityMismatch { observed } => DocumentState::Conflicted {
                base_remote: snapshot.base_remote,
                observed_remote: observed,
            },
            SaveResult::TransportFailed { connected, .. } if connected => DocumentState::Dirty {
                base_remote: snapshot.base_remote,
            },
            SaveResult::TransportFailed { .. } => DocumentState::OfflineDirty {
                base_remote: snapshot.base_remote,
            },
        };
        Ok(())
    }
}

fn validate_limits(limits: DocumentLimits) -> Result<(), DocumentError> {
    if limits.max_utf8_bytes == 0 || limits.max_lines == 0 || limits.max_change_ids == 0 {
        Err(DocumentError::InvalidLimits)
    } else {
        Ok(())
    }
}

fn validate_content(text: &str, limits: DocumentLimits) -> Result<(), DocumentError> {
    if text.len() > limits.max_utf8_bytes {
        return Err(DocumentError::ContentTooLarge);
    }
    let lines = text
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        .checked_add(1)
        .ok_or(DocumentError::TooManyLines)?;
    if lines > limits.max_lines {
        Err(DocumentError::TooManyLines)
    } else {
        Ok(())
    }
}

fn validate_range(text: &str, start: usize, end: usize) -> Result<(), DocumentError> {
    if start > end || end > text.len() {
        return Err(DocumentError::InvalidRange);
    }
    if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return Err(DocumentError::SplitCodePoint);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: DocumentLimits = DocumentLimits {
        max_utf8_bytes: 32,
        max_lines: 3,
        max_change_ids: 4,
    };

    fn document(text: &str) -> Document {
        Document::open(
            DocumentId::new("doc-1"),
            text.into(),
            RemoteIdentity::new("remote-1"),
            LIMITS,
        )
        .unwrap()
    }

    fn edit(id: &str, revision: u64, start: usize, end: usize, replacement: &str) -> EditCommand {
        EditCommand {
            change_id: ChangeId::new(id),
            base_revision: revision,
            start,
            end,
            replacement: replacement.into(),
        }
    }

    #[test]
    fn edit_is_revisioned_idempotent_and_resyncs_stale_input() {
        let mut doc = document("old");
        let command = edit("c1", 0, 0, 3, "new");
        assert_eq!(
            doc.apply(command.clone()).unwrap(),
            EditOutcome::Applied { revision: 1 }
        );
        assert_eq!(
            doc.apply(command).unwrap(),
            EditOutcome::Duplicate { revision: 1 }
        );
        assert_eq!(doc.text(), "new");
        assert_eq!(
            doc.apply(edit("stale", 0, 0, 0, "x")).unwrap(),
            EditOutcome::ResyncRequired {
                current_revision: 1
            }
        );
        assert_eq!(doc.text(), "new");
        assert_eq!(
            doc.apply(edit("c1", 1, 0, 0, "x")),
            Err(DocumentError::ChangeIdMismatch)
        );
    }

    #[test]
    fn rejects_split_codepoints_and_content_limits_atomically() {
        let mut doc = document("aé");
        assert_eq!(
            doc.apply(edit("split", 0, 1, 2, "")),
            Err(DocumentError::SplitCodePoint)
        );
        assert_eq!(doc.text(), "aé");
        assert_eq!(
            doc.apply(edit("lines", 0, 0, 0, "1\n2\n3\n")),
            Err(DocumentError::TooManyLines)
        );
        assert_eq!(doc.revision(), 0);
    }

    #[test]
    fn offline_edits_continue_and_reconnect_can_conflict() {
        let mut doc = document("a");
        doc.apply(edit("c1", 0, 1, 1, "b")).unwrap();
        doc.disconnect();
        assert!(matches!(doc.state(), DocumentState::OfflineDirty { .. }));
        doc.apply(edit("c2", 1, 2, 2, "c")).unwrap();
        doc.reconnect(RemoteIdentity::new("changed"));
        assert!(matches!(doc.state(), DocumentState::Conflicted { .. }));
        assert_eq!(
            doc.apply(edit("c3", 2, 0, 0, "x")),
            Err(DocumentError::NotEditable)
        );
    }

    #[test]
    fn save_success_accounts_for_edits_after_frozen_revision() {
        let mut doc = document("a");
        doc.apply(edit("c1", 0, 1, 1, "b")).unwrap();
        let save = doc.begin_save(SaveId::new("save-1")).unwrap();
        assert_eq!(save.text, "ab");
        doc.apply(edit("c2", 1, 2, 2, "c")).unwrap();
        doc.finish_save(
            &SaveId::new("save-1"),
            SaveResult::Written {
                remote: RemoteIdentity::new("remote-2"),
            },
        )
        .unwrap();
        assert_eq!(doc.text(), "abc");
        assert_eq!(
            doc.state(),
            &DocumentState::Dirty {
                base_remote: RemoteIdentity::new("remote-2")
            }
        );
    }

    #[test]
    fn save_outcomes_are_fail_closed_and_id_bound() {
        let mut doc = document("a");
        doc.apply(edit("c1", 0, 0, 1, "b")).unwrap();
        doc.begin_save(SaveId::new("save-1")).unwrap();
        assert_eq!(
            doc.finish_save(
                &SaveId::new("wrong"),
                SaveResult::Written {
                    remote: RemoteIdentity::new("remote-2")
                }
            ),
            Err(DocumentError::NoMatchingSave)
        );
        doc.finish_save(
            &SaveId::new("save-1"),
            SaveResult::IdentityMismatch {
                observed: RemoteIdentity::new("remote-other"),
            },
        )
        .unwrap();
        assert!(matches!(doc.state(), DocumentState::Conflicted { .. }));
    }

    #[test]
    fn read_only_documents_reject_mutation_and_save() {
        let mut doc = Document::open_read_only(
            DocumentId::new("doc-ro"),
            "text".into(),
            ReadOnlyReason::MixedLineEndings,
            LIMITS,
        )
        .unwrap();
        assert_eq!(
            doc.apply(edit("c1", 0, 0, 0, "x")),
            Err(DocumentError::NotEditable)
        );
        assert_eq!(
            doc.begin_save(SaveId::new("s")),
            Err(DocumentError::NotSaveable)
        );
    }
}
