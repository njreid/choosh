//! Bounded annotation records, deterministic re-anchoring, and export snapshots.

use std::collections::BTreeMap;

const MAX_ID_BYTES: usize = 128;
const MAX_DOCUMENT_BYTES: usize = 512;

macro_rules! identity {
    ($name:ident, $limit:expr) => {
        #[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            /// Parses a bounded, non-empty identifier.
            ///
            /// # Errors
            ///
            /// Rejects empty, oversized, or control-containing values.
            pub fn parse(value: impl Into<String>) -> Result<Self, AnnotationError> {
                let value = value.into();
                if value.is_empty() || value.len() > $limit || value.chars().any(char::is_control) {
                    return Err(AnnotationError::InvalidIdentity);
                }
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

identity!(AnnotationId, MAX_ID_BYTES);
identity!(HostId, MAX_ID_BYTES);
identity!(WorkspaceId, MAX_ID_BYTES);
identity!(DocumentIdentity, MAX_DOCUMENT_BYTES);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextRange {
    pub start: usize,
    pub end: usize,
}

impl TextRange {
    #[must_use]
    pub fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.start >= self.end
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextFingerprint {
    pub selected_digest: [u8; 32],
    pub prefix: String,
    pub suffix: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnchorConfidence {
    Exact,
    Mapped,
    Context,
    Manual,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrphanReason {
    OverlappingRewrite,
    MissingHistory,
    NoCandidate,
    BudgetExceeded,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnchorStatus {
    Attached {
        range: TextRange,
        confidence: AnchorConfidence,
    },
    Ambiguous {
        candidate_count: usize,
    },
    Orphaned {
        reason: OrphanReason,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct AnnotationKey {
    pub host_id: HostId,
    pub workspace_id: WorkspaceId,
    pub document: DocumentIdentity,
    pub annotation_id: AnnotationId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Annotation {
    pub key: AnnotationKey,
    pub record_revision: u64,
    pub created_revision: u64,
    pub last_resolved_revision: u64,
    pub last_range: TextRange,
    pub context: ContextFingerprint,
    pub body_markdown: String,
    pub status: AnchorStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnnotationLimits {
    pub max_records: usize,
    pub max_body_bytes: usize,
    pub max_body_lines: usize,
    pub max_context_bytes: usize,
}

impl AnnotationLimits {
    /// Validates nonzero annotation resource ceilings.
    ///
    /// # Errors
    ///
    /// Returns [`AnnotationError::InvalidLimits`] when any ceiling is zero.
    pub fn new(
        max_records: usize,
        max_body_bytes: usize,
        max_body_lines: usize,
        max_context_bytes: usize,
    ) -> Result<Self, AnnotationError> {
        if [max_records, max_body_bytes, max_body_lines, max_context_bytes].contains(&0) {
            return Err(AnnotationError::InvalidLimits);
        }
        Ok(Self {
            max_records,
            max_body_bytes,
            max_body_lines,
            max_context_bytes,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewAnnotation {
    pub key: AnnotationKey,
    pub document_revision: u64,
    pub range: TextRange,
    pub context: ContextFingerprint,
    pub body_markdown: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnnotationRegistry {
    limits: AnnotationLimits,
    records: BTreeMap<AnnotationKey, Annotation>,
}

impl AnnotationRegistry {
    #[must_use]
    pub fn new(limits: AnnotationLimits) -> Self {
        Self {
            limits,
            records: BTreeMap::new(),
        }
    }

    /// Creates a record without replacing an existing identifier.
    ///
    /// # Errors
    ///
    /// Rejects duplicate keys, invalid content, and capacity overflow.
    pub fn create(&mut self, input: NewAnnotation) -> Result<&Annotation, AnnotationError> {
        if self.records.contains_key(&input.key) {
            return Err(AnnotationError::Duplicate);
        }
        if self.records.len() >= self.limits.max_records {
            return Err(AnnotationError::RecordLimit);
        }
        validate_fields(
            input.range,
            &input.context,
            &input.body_markdown,
            self.limits,
        )?;
        let key = input.key.clone();
        self.records.insert(
            key.clone(),
            Annotation {
                key: key.clone(),
                record_revision: 1,
                created_revision: input.document_revision,
                last_resolved_revision: input.document_revision,
                last_range: input.range,
                context: input.context,
                body_markdown: input.body_markdown,
                status: AnchorStatus::Attached {
                    range: input.range,
                    confidence: AnchorConfidence::Exact,
                },
            },
        );
        Ok(self.records.get(&key).expect("inserted annotation exists"))
    }

    #[must_use]
    pub fn get(&self, key: &AnnotationKey) -> Option<&Annotation> {
        self.records.get(key)
    }

    /// Updates an annotation body using optimistic concurrency.
    ///
    /// # Errors
    ///
    /// Rejects missing/stale records and invalid bodies without mutation.
    pub fn update_body(
        &mut self,
        key: &AnnotationKey,
        expected_record_revision: u64,
        body: String,
    ) -> Result<&Annotation, AnnotationError> {
        validate_body(&body, self.limits)?;
        let record = self.records.get_mut(key).ok_or(AnnotationError::NotFound)?;
        if record.record_revision != expected_record_revision {
            return Err(AnnotationError::StaleRevision);
        }
        record.body_markdown = body;
        record.record_revision = record.record_revision.saturating_add(1);
        Ok(record)
    }

    /// Deletes a record using optimistic concurrency.
    ///
    /// # Errors
    ///
    /// Rejects missing or stale records without mutation.
    pub fn delete(
        &mut self,
        key: &AnnotationKey,
        expected_record_revision: u64,
    ) -> Result<Annotation, AnnotationError> {
        let current = self.records.get(key).ok_or(AnnotationError::NotFound)?;
        if current.record_revision != expected_record_revision {
            return Err(AnnotationError::StaleRevision);
        }
        self.records.remove(key).ok_or(AnnotationError::NotFound)
    }

    /// Freezes a sorted, bounded export model. This method performs no I/O.
    ///
    /// # Errors
    ///
    /// Returns [`AnnotationError::ExportLimit`] when the selected records exceed
    /// the requested count or aggregate-byte ceiling.
    pub fn prepare_export(
        &self,
        workspace: &WorkspaceId,
        max_records: usize,
        max_bytes: usize,
        include_context: bool,
    ) -> Result<AnnotationExport, AnnotationError> {
        if max_records == 0 || max_bytes == 0 {
            return Err(AnnotationError::InvalidLimits);
        }
        let mut records = Vec::new();
        let mut bytes = 0_usize;
        for record in self
            .records
            .values()
            .filter(|record| &record.key.workspace_id == workspace)
        {
            if records.len() == max_records {
                return Err(AnnotationError::ExportLimit);
            }
            let context = include_context.then(|| record.context.clone());
            bytes = bytes
                .checked_add(record.body_markdown.len())
                .and_then(|size| size.checked_add(record.key.document.as_str().len()))
                .and_then(|size| size.checked_add(record.key.annotation_id.as_str().len()))
                .and_then(|size| {
                    context.as_ref().map_or(Some(size), |value| {
                        size.checked_add(value.prefix.len())
                            .and_then(|sum| sum.checked_add(value.suffix.len()))
                    })
                })
                .ok_or(AnnotationError::ExportLimit)?;
            if bytes > max_bytes {
                return Err(AnnotationError::ExportLimit);
            }
            records.push(ExportAnnotation {
                annotation_id: record.key.annotation_id.clone(),
                document: record.key.document.clone(),
                body_markdown: record.body_markdown.clone(),
                status: record.status.clone(),
                context,
            });
        }
        Ok(AnnotationExport {
            schema_version: 1,
            workspace_id: workspace.clone(),
            records,
            estimated_content_bytes: bytes,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportAnnotation {
    pub annotation_id: AnnotationId,
    pub document: DocumentIdentity,
    pub body_markdown: String,
    pub status: AnchorStatus,
    pub context: Option<ContextFingerprint>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnnotationExport {
    pub schema_version: u16,
    pub workspace_id: WorkspaceId,
    pub records: Vec<ExportAnnotation>,
    pub estimated_content_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReanchorLimits {
    pub max_document_bytes: usize,
    pub max_candidates: usize,
}

/// Deterministically resolves an annotation against a new UTF-8 snapshot.
///
/// The old snapshot enables a single bounded changed-region map. Context search
/// never chooses among multiple matches.
///
/// # Errors
///
/// Rejects oversized snapshots and invalid UTF-8 byte boundaries.
pub fn reanchor(
    annotation: &Annotation,
    old_source: Option<&str>,
    new_source: &str,
    _new_revision: u64,
    limits: ReanchorLimits,
) -> Result<AnchorStatus, AnnotationError> {
    if limits.max_document_bytes == 0
        || limits.max_candidates == 0
        || new_source.len() > limits.max_document_bytes
        || old_source.is_some_and(|source| source.len() > limits.max_document_bytes)
    {
        return Err(AnnotationError::ReanchorLimit);
    }
    let range = annotation.last_range;
    if valid_range(new_source, range)
        && digest(&new_source.as_bytes()[range.start..range.end])
            == annotation.context.selected_digest
        && contexts_match(new_source, range, &annotation.context)
    {
        return Ok(AnchorStatus::Attached {
            range,
            confidence: AnchorConfidence::Exact,
        });
    }
    let Some(old) = old_source else {
        return Ok(AnchorStatus::Orphaned {
            reason: OrphanReason::MissingHistory,
        });
    };
    if !valid_range(old, range) {
        return Err(AnnotationError::InvalidRange);
    }
    let (old_changed, new_changed) = changed_regions(old.as_bytes(), new_source.as_bytes());
    let mapped = if range.end <= old_changed.start {
        Some(range)
    } else if range.start >= old_changed.end {
        let shift = new_changed.len() as i128 - old_changed.len() as i128;
        Some(shift_range(range, shift).ok_or(AnnotationError::InvalidRange)?)
    } else {
        None
    };
    if let Some(mapped_range) = mapped {
        if valid_range(new_source, mapped_range)
            && digest(&new_source.as_bytes()[mapped_range.start..mapped_range.end])
                == annotation.context.selected_digest
        {
            return Ok(AnchorStatus::Attached {
                range: mapped_range,
                confidence: AnchorConfidence::Mapped,
            });
        }
    }

    let selected = &old.as_bytes()[range.start..range.end];
    let mut candidates = Vec::new();
    for (start, _) in new_source.match_indices(std::str::from_utf8(selected).map_err(|_| AnnotationError::InvalidRange)?) {
        let candidate = TextRange {
            start,
            end: start + selected.len(),
        };
        if contexts_match(new_source, candidate, &annotation.context) {
            candidates.push(candidate);
            if candidates.len() > limits.max_candidates {
                return Ok(AnchorStatus::Orphaned {
                    reason: OrphanReason::BudgetExceeded,
                });
            }
        }
    }
    match candidates.as_slice() {
        [candidate] => Ok(AnchorStatus::Attached {
            range: *candidate,
            confidence: AnchorConfidence::Context,
        }),
        [] if mapped.is_none() => Ok(AnchorStatus::Orphaned {
            reason: OrphanReason::OverlappingRewrite,
        }),
        [] => Ok(AnchorStatus::Orphaned {
            reason: OrphanReason::NoCandidate,
        }),
        _ => Ok(AnchorStatus::Ambiguous {
            candidate_count: candidates.len(),
        }),
    }
}

fn changed_regions(old: &[u8], new: &[u8]) -> (TextRange, TextRange) {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    (
        TextRange {
            start: prefix,
            end: old.len() - suffix,
        },
        TextRange {
            start: prefix,
            end: new.len() - suffix,
        },
    )
}

fn shift_range(range: TextRange, shift: i128) -> Option<TextRange> {
    Some(TextRange {
        start: usize::try_from(range.start as i128 + shift).ok()?,
        end: usize::try_from(range.end as i128 + shift).ok()?,
    })
}

fn contexts_match(source: &str, range: TextRange, context: &ContextFingerprint) -> bool {
    let bytes = source.as_bytes();
    range.start >= context.prefix.len()
        && range.end.saturating_add(context.suffix.len()) <= bytes.len()
        && &bytes[range.start - context.prefix.len()..range.start] == context.prefix.as_bytes()
        && &bytes[range.end..range.end + context.suffix.len()] == context.suffix.as_bytes()
}

fn valid_range(source: &str, range: TextRange) -> bool {
    !range.is_empty()
        && range.end <= source.len()
        && source.is_char_boundary(range.start)
        && source.is_char_boundary(range.end)
}

fn validate_fields(
    range: TextRange,
    context: &ContextFingerprint,
    body: &str,
    limits: AnnotationLimits,
) -> Result<(), AnnotationError> {
    if range.is_empty() {
        return Err(AnnotationError::InvalidRange);
    }
    if context.prefix.len().saturating_add(context.suffix.len()) > limits.max_context_bytes {
        return Err(AnnotationError::ContextLimit);
    }
    validate_body(body, limits)
}

fn validate_body(body: &str, limits: AnnotationLimits) -> Result<(), AnnotationError> {
    let lines = body.lines().count().max(1);
    if body.is_empty() || body.len() > limits.max_body_bytes || lines > limits.max_body_lines {
        return Err(AnnotationError::BodyLimit);
    }
    Ok(())
}

/// A small stable digest used as an opaque equality fingerprint, not authentication.
#[must_use]
pub fn digest(bytes: &[u8]) -> [u8; 32] {
    let mut output = [0_u8; 32];
    for (index, byte) in bytes.iter().enumerate() {
        let slot = index % output.len();
        output[slot] = output[slot]
            .wrapping_mul(31)
            .wrapping_add(*byte)
            .wrapping_add((index & 0xff) as u8);
    }
    output
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnnotationError {
    InvalidIdentity,
    InvalidLimits,
    InvalidRange,
    BodyLimit,
    ContextLimit,
    RecordLimit,
    Duplicate,
    NotFound,
    StaleRevision,
    ExportLimit,
    ReanchorLimit,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: &str) -> AnnotationKey {
        AnnotationKey {
            host_id: HostId::parse("host").unwrap(),
            workspace_id: WorkspaceId::parse("workspace").unwrap(),
            document: DocumentIdentity::parse("docs/review.md").unwrap(),
            annotation_id: AnnotationId::parse(id).unwrap(),
        }
    }

    fn context(selected: &str, prefix: &str, suffix: &str) -> ContextFingerprint {
        ContextFingerprint {
            selected_digest: digest(selected.as_bytes()),
            prefix: prefix.into(),
            suffix: suffix.into(),
        }
    }

    fn registry() -> AnnotationRegistry {
        AnnotationRegistry::new(AnnotationLimits::new(4, 128, 4, 32).unwrap())
    }

    fn annotation(source: &str, selected: &str) -> Annotation {
        let start = source.find(selected).unwrap();
        let mut registry = registry();
        registry
            .create(NewAnnotation {
                key: key("a1"),
                document_revision: 1,
                range: TextRange {
                    start,
                    end: start + selected.len(),
                },
                context: context(selected, "[", "]"),
                body_markdown: "note".into(),
            })
            .unwrap()
            .clone()
    }

    #[test]
    fn crud_is_revisioned_and_stale_updates_do_not_mutate() {
        let mut records = registry();
        let created = records
            .create(NewAnnotation {
                key: key("a1"),
                document_revision: 7,
                range: TextRange { start: 1, end: 2 },
                context: context("x", "", ""),
                body_markdown: "first".into(),
            })
            .unwrap();
        assert_eq!(created.record_revision, 1);
        records.update_body(&key("a1"), 1, "second".into()).unwrap();
        assert_eq!(
            records.update_body(&key("a1"), 1, "lost".into()),
            Err(AnnotationError::StaleRevision)
        );
        assert_eq!(records.get(&key("a1")).unwrap().body_markdown, "second");
        assert!(records.delete(&key("a1"), 2).is_ok());
    }

    #[test]
    fn non_overlapping_insert_maps_range() {
        let old = "lead [target] tail";
        let record = annotation(old, "target");
        assert_eq!(
            reanchor(&record, Some(old), "new lead [target] tail", 2, ReanchorLimits { max_document_bytes: 100, max_candidates: 4 }).unwrap(),
            AnchorStatus::Attached { range: TextRange { start: 10, end: 16 }, confidence: AnchorConfidence::Mapped }
        );
    }

    #[test]
    fn repeated_context_candidates_are_ambiguous() {
        let old = "zz[target]qq";
        let record = annotation(old, "target");
        assert_eq!(
            reanchor(&record, Some(old), "[target] and [target]", 2, ReanchorLimits { max_document_bytes: 100, max_candidates: 4 }).unwrap(),
            AnchorStatus::Ambiguous { candidate_count: 2 }
        );
    }

    #[test]
    fn overlapping_rewrite_orphans() {
        let old = "[target]";
        let record = annotation(old, "target");
        assert_eq!(
            reanchor(&record, Some(old), "[changed]", 2, ReanchorLimits { max_document_bytes: 100, max_candidates: 4 }).unwrap(),
            AnchorStatus::Orphaned { reason: OrphanReason::OverlappingRewrite }
        );
    }

    #[test]
    fn export_is_sorted_bounded_and_has_no_write_capability() {
        let mut records = registry();
        for id in ["b", "a"] {
            records.create(NewAnnotation {
                key: key(id), document_revision: 1, range: TextRange { start: 1, end: 2 },
                context: context("x", "", ""), body_markdown: id.into(),
            }).unwrap();
        }
        let export = records.prepare_export(&WorkspaceId::parse("workspace").unwrap(), 2, 100, false).unwrap();
        assert_eq!(export.records.iter().map(|r| r.annotation_id.as_str()).collect::<Vec<_>>(), vec!["a", "b"]);
        assert!(export.records.iter().all(|record| record.context.is_none()));
        assert_eq!(records.prepare_export(&WorkspaceId::parse("workspace").unwrap(), 1, 100, false), Err(AnnotationError::ExportLimit));
    }
}
