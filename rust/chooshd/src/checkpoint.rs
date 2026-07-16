//! Versioned, bounded binary checkpoint codec for non-secret daemon metadata.
//!
//! Atomic replacement is deliberately outside this module.

const MAGIC: &[u8; 8] = b"CHOOSHCP";
const VERSION_V0: u16 = 0;
const VERSION_V1: u16 = 1;
const HEADER_BYTES: usize = 18;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckpointLimits {
    pub max_payload_bytes: usize,
    pub max_workspaces: usize,
    pub max_items: usize,
    pub max_spools: usize,
    pub max_identity_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonCheckpoint {
    pub generation: u64,
    pub workspaces: Vec<WorkspaceCheckpoint>,
    pub items: Vec<ItemCheckpoint>,
    pub spools: Vec<SpoolCheckpoint>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct WorkspaceCheckpoint {
    pub workspace_id: String,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ItemKind {
    Agent,
    Service,
    Terminal,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ItemCheckpoint {
    pub workspace_id: String,
    pub item_id: String,
    pub kind: ItemKind,
    pub revision: u64,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SpoolCheckpoint {
    pub workspace_id: String,
    pub stream_id: String,
    pub high_water: u64,
}

/// Encodes a canonical v1 checkpoint envelope.
///
/// Records are sorted before encoding so caller iteration order cannot change
/// durable bytes. The input is not mutated.
///
/// # Errors
///
/// Rejects invalid limits, duplicate records, invalid identities, excessive
/// counts, and payload-size or integer-representation overflow.
pub fn encode(
    checkpoint: &DaemonCheckpoint,
    limits: CheckpointLimits,
) -> Result<Vec<u8>, CheckpointError> {
    validate_limits(limits)?;
    let mut canonical = checkpoint.clone();
    canonical.workspaces.sort();
    canonical.items.sort();
    canonical.spools.sort();
    validate_checkpoint(&canonical, limits)?;
    let mut payload = Vec::new();
    push_u64(&mut payload, canonical.generation);
    push_count(&mut payload, canonical.workspaces.len())?;
    for workspace in &canonical.workspaces {
        push_string(&mut payload, &workspace.workspace_id)?;
        push_u64(&mut payload, workspace.revision);
    }
    push_count(&mut payload, canonical.items.len())?;
    for item in &canonical.items {
        push_string(&mut payload, &item.workspace_id)?;
        push_string(&mut payload, &item.item_id)?;
        payload.push(match item.kind {
            ItemKind::Agent => 0,
            ItemKind::Service => 1,
            ItemKind::Terminal => 2,
        });
        push_u64(&mut payload, item.revision);
    }
    push_count(&mut payload, canonical.spools.len())?;
    for spool in &canonical.spools {
        push_string(&mut payload, &spool.workspace_id)?;
        push_string(&mut payload, &spool.stream_id)?;
        push_u64(&mut payload, spool.high_water);
    }
    envelope(VERSION_V1, &payload, limits)
}

/// Decodes v1 or migrates the documented workspace-only v0 layout.
///
/// # Errors
///
/// Rejects invalid headers, unknown versions, mismatched lengths/checksums,
/// truncated/trailing data, invalid identities, duplicates, and every configured
/// resource ceiling.
pub fn decode(bytes: &[u8], limits: CheckpointLimits) -> Result<DaemonCheckpoint, CheckpointError> {
    validate_limits(limits)?;
    if bytes.len() < HEADER_BYTES || &bytes[..8] != MAGIC {
        return Err(CheckpointError::InvalidHeader);
    }
    let version = u16::from_be_bytes([bytes[8], bytes[9]]);
    let payload_len = u32::from_be_bytes(
        bytes[10..14]
            .try_into()
            .map_err(|_| CheckpointError::Truncated)?,
    ) as usize;
    let expected_checksum = u32::from_be_bytes(
        bytes[14..18]
            .try_into()
            .map_err(|_| CheckpointError::Truncated)?,
    );
    if payload_len > limits.max_payload_bytes {
        return Err(CheckpointError::PayloadLimit);
    }
    if bytes.len() != HEADER_BYTES.saturating_add(payload_len) {
        return Err(if bytes.len() < HEADER_BYTES.saturating_add(payload_len) {
            CheckpointError::Truncated
        } else {
            CheckpointError::TrailingData
        });
    }
    let payload = &bytes[HEADER_BYTES..];
    if checksum(payload) != expected_checksum {
        return Err(CheckpointError::ChecksumMismatch);
    }
    let checkpoint = match version {
        VERSION_V0 => decode_v0(payload, limits)?,
        VERSION_V1 => decode_v1(payload, limits)?,
        _ => return Err(CheckpointError::UnknownVersion),
    };
    validate_checkpoint(&checkpoint, limits)?;
    Ok(checkpoint)
}

fn decode_v1(
    payload: &[u8],
    limits: CheckpointLimits,
) -> Result<DaemonCheckpoint, CheckpointError> {
    let mut reader = Reader::new(payload);
    let generation = reader.u64()?;
    let workspace_count = reader.count(limits.max_workspaces)?;
    let mut workspaces = Vec::with_capacity(workspace_count);
    for _ in 0..workspace_count {
        workspaces.push(WorkspaceCheckpoint {
            workspace_id: reader.string(limits.max_identity_bytes)?,
            revision: reader.u64()?,
        });
    }
    let item_count = reader.count(limits.max_items)?;
    let mut items = Vec::with_capacity(item_count);
    for _ in 0..item_count {
        items.push(ItemCheckpoint {
            workspace_id: reader.string(limits.max_identity_bytes)?,
            item_id: reader.string(limits.max_identity_bytes)?,
            kind: match reader.byte()? {
                0 => ItemKind::Agent,
                1 => ItemKind::Service,
                2 => ItemKind::Terminal,
                _ => return Err(CheckpointError::InvalidRecord),
            },
            revision: reader.u64()?,
        });
    }
    let spool_count = reader.count(limits.max_spools)?;
    let mut spools = Vec::with_capacity(spool_count);
    for _ in 0..spool_count {
        spools.push(SpoolCheckpoint {
            workspace_id: reader.string(limits.max_identity_bytes)?,
            stream_id: reader.string(limits.max_identity_bytes)?,
            high_water: reader.u64()?,
        });
    }
    reader.finish()?;
    Ok(DaemonCheckpoint {
        generation,
        workspaces,
        items,
        spools,
    })
}

// V0 contained generation plus workspace ID/revision records only.
fn decode_v0(
    payload: &[u8],
    limits: CheckpointLimits,
) -> Result<DaemonCheckpoint, CheckpointError> {
    let mut reader = Reader::new(payload);
    let generation = reader.u64()?;
    let count = reader.count(limits.max_workspaces)?;
    let mut workspaces = Vec::with_capacity(count);
    for _ in 0..count {
        workspaces.push(WorkspaceCheckpoint {
            workspace_id: reader.string(limits.max_identity_bytes)?,
            revision: reader.u64()?,
        });
    }
    reader.finish()?;
    Ok(DaemonCheckpoint {
        generation,
        workspaces,
        items: Vec::new(),
        spools: Vec::new(),
    })
}

fn validate_limits(limits: CheckpointLimits) -> Result<(), CheckpointError> {
    if limits.max_payload_bytes == 0
        || limits.max_workspaces == 0
        || limits.max_items == 0
        || limits.max_spools == 0
        || limits.max_identity_bytes == 0
    {
        return Err(CheckpointError::InvalidLimits);
    }
    Ok(())
}

fn validate_checkpoint(
    checkpoint: &DaemonCheckpoint,
    limits: CheckpointLimits,
) -> Result<(), CheckpointError> {
    if checkpoint.workspaces.len() > limits.max_workspaces
        || checkpoint.items.len() > limits.max_items
        || checkpoint.spools.len() > limits.max_spools
    {
        return Err(CheckpointError::RecordLimit);
    }
    for identity in checkpoint
        .workspaces
        .iter()
        .map(|record| record.workspace_id.as_str())
        .chain(
            checkpoint
                .items
                .iter()
                .flat_map(|record| [record.workspace_id.as_str(), record.item_id.as_str()]),
        )
        .chain(
            checkpoint
                .spools
                .iter()
                .flat_map(|record| [record.workspace_id.as_str(), record.stream_id.as_str()]),
        )
    {
        validate_identity(identity, limits.max_identity_bytes)?;
    }
    if has_adjacent_duplicate(&checkpoint.workspaces)
        || has_adjacent_duplicate(&checkpoint.items)
        || has_adjacent_duplicate(&checkpoint.spools)
    {
        return Err(CheckpointError::DuplicateRecord);
    }
    Ok(())
}

fn has_adjacent_duplicate<T: PartialEq>(values: &[T]) -> bool {
    values.windows(2).any(|pair| pair[0] == pair[1])
}

fn validate_identity(value: &str, max_bytes: usize) -> Result<(), CheckpointError> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.chars().any(char::is_control)
        || value.contains('/')
        || value.contains('\\')
    {
        return Err(CheckpointError::InvalidIdentity);
    }
    Ok(())
}

fn envelope(
    version: u16,
    payload: &[u8],
    limits: CheckpointLimits,
) -> Result<Vec<u8>, CheckpointError> {
    if payload.len() > limits.max_payload_bytes {
        return Err(CheckpointError::PayloadLimit);
    }
    let length = u32::try_from(payload.len()).map_err(|_| CheckpointError::PayloadLimit)?;
    let mut bytes = Vec::with_capacity(HEADER_BYTES + payload.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&version.to_be_bytes());
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(&checksum(payload).to_be_bytes());
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

fn checksum(bytes: &[u8]) -> u32 {
    let mut value = 2_166_136_261_u32;
    for byte in bytes {
        value ^= u32::from(*byte);
        value = value.wrapping_mul(16_777_619);
    }
    value
}

fn push_count(output: &mut Vec<u8>, value: usize) -> Result<(), CheckpointError> {
    let value = u32::try_from(value).map_err(|_| CheckpointError::RecordLimit)?;
    output.extend_from_slice(&value.to_be_bytes());
    Ok(())
}

fn push_string(output: &mut Vec<u8>, value: &str) -> Result<(), CheckpointError> {
    let length = u16::try_from(value.len()).map_err(|_| CheckpointError::InvalidIdentity)?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_be_bytes());
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], CheckpointError> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or(CheckpointError::Truncated)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(CheckpointError::Truncated)?;
        self.offset = end;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, CheckpointError> {
        Ok(self.take(1)?[0])
    }

    fn u64(&mut self) -> Result<u64, CheckpointError> {
        Ok(u64::from_be_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| CheckpointError::Truncated)?,
        ))
    }

    fn count(&mut self, max: usize) -> Result<usize, CheckpointError> {
        let value = u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| CheckpointError::Truncated)?,
        ) as usize;
        if value > max {
            return Err(CheckpointError::RecordLimit);
        }
        Ok(value)
    }

    fn string(&mut self, max: usize) -> Result<String, CheckpointError> {
        let length = usize::from(u16::from_be_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| CheckpointError::Truncated)?,
        ));
        if length > max {
            return Err(CheckpointError::InvalidIdentity);
        }
        let value = std::str::from_utf8(self.take(length)?)
            .map_err(|_| CheckpointError::InvalidIdentity)?;
        validate_identity(value, max)?;
        Ok(value.to_owned())
    }

    fn finish(self) -> Result<(), CheckpointError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(CheckpointError::TrailingData)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointError {
    InvalidLimits,
    InvalidHeader,
    UnknownVersion,
    PayloadLimit,
    RecordLimit,
    InvalidIdentity,
    InvalidRecord,
    DuplicateRecord,
    ChecksumMismatch,
    Truncated,
    TrailingData,
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: CheckpointLimits = CheckpointLimits {
        max_payload_bytes: 2_048,
        max_workspaces: 4,
        max_items: 8,
        max_spools: 8,
        max_identity_bytes: 32,
    };

    fn sample() -> DaemonCheckpoint {
        DaemonCheckpoint {
            generation: 9,
            workspaces: vec![
                WorkspaceCheckpoint {
                    workspace_id: "w2".into(),
                    revision: 2,
                },
                WorkspaceCheckpoint {
                    workspace_id: "w1".into(),
                    revision: 1,
                },
            ],
            items: vec![ItemCheckpoint {
                workspace_id: "w1".into(),
                item_id: "terminal".into(),
                kind: ItemKind::Terminal,
                revision: 3,
            }],
            spools: vec![SpoolCheckpoint {
                workspace_id: "w1".into(),
                stream_id: "events".into(),
                high_water: 44,
            }],
        }
    }

    #[test]
    fn v1_roundtrip_is_canonical_and_golden_header_is_stable() {
        let encoded = encode(&sample(), LIMITS).unwrap();
        assert_eq!(&encoded[..10], b"CHOOSHCP\0\x01");
        let decoded = decode(&encoded, LIMITS).unwrap();
        assert_eq!(decoded.workspaces[0].workspace_id, "w1");
        assert_eq!(encode(&decoded, LIMITS).unwrap(), encoded);
    }

    #[test]
    fn corruption_truncation_and_trailing_bytes_fail_closed() {
        let encoded = encode(&sample(), LIMITS).unwrap();
        let mut corrupt = encoded.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert_eq!(
            decode(&corrupt, LIMITS),
            Err(CheckpointError::ChecksumMismatch)
        );
        assert_eq!(
            decode(&encoded[..encoded.len() - 1], LIMITS),
            Err(CheckpointError::Truncated)
        );
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert_eq!(
            decode(&trailing, LIMITS),
            Err(CheckpointError::TrailingData)
        );
    }

    #[test]
    fn unknown_version_is_rejected_after_envelope_validation() {
        let mut encoded = encode(&sample(), LIMITS).unwrap();
        encoded[8..10].copy_from_slice(&99_u16.to_be_bytes());
        assert_eq!(
            decode(&encoded, LIMITS),
            Err(CheckpointError::UnknownVersion)
        );
    }

    #[test]
    fn workspace_only_v0_fixture_migrates_to_empty_items_and_spools() {
        let mut payload = Vec::new();
        push_u64(&mut payload, 5);
        push_count(&mut payload, 1).unwrap();
        push_string(&mut payload, "legacy").unwrap();
        push_u64(&mut payload, 7);
        let fixture = envelope(VERSION_V0, &payload, LIMITS).unwrap();
        let migrated = decode(&fixture, LIMITS).unwrap();
        assert_eq!(migrated.generation, 5);
        assert_eq!(
            migrated.workspaces,
            vec![WorkspaceCheckpoint {
                workspace_id: "legacy".into(),
                revision: 7
            }]
        );
        assert!(migrated.items.is_empty() && migrated.spools.is_empty());
    }

    #[test]
    fn paths_secrets_duplicates_and_count_limits_are_rejected() {
        let mut invalid = sample();
        invalid.workspaces[0].workspace_id = "path/to/workspace".into();
        assert_eq!(
            encode(&invalid, LIMITS),
            Err(CheckpointError::InvalidIdentity)
        );
        let mut duplicate = sample();
        duplicate.workspaces.push(duplicate.workspaces[0].clone());
        assert_eq!(
            encode(&duplicate, LIMITS),
            Err(CheckpointError::DuplicateRecord)
        );
        let small = CheckpointLimits {
            max_workspaces: 1,
            ..LIMITS
        };
        assert_eq!(encode(&sample(), small), Err(CheckpointError::RecordLimit));
    }
}
