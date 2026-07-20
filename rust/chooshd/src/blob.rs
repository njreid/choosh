//! Bounded, immutable Git blob stream capabilities.

use std::collections::BTreeMap;
use std::io::Read;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SnapshotId(String);

impl SnapshotId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EntryId(String);

impl EntryId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CapabilityToken(String);

impl CapabilityToken {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Comparison {
    Working,
    Staged,
    Combined,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlobAddress {
    pub snapshot_id: SnapshotId,
    pub entry_id: EntryId,
    pub comparison: Comparison,
    pub side: Side,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BlobIdentity {
    Empty,
    ObjectId(String),
    Worktree(WorktreeIdentity),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeIdentity {
    pub file_id: u64,
    pub size: u64,
    pub modified: u64,
    pub digest: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlobDescriptor {
    pub address: BlobAddress,
    pub identity: BlobIdentity,
    pub max_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedBlob {
    pub token: CapabilityToken,
    pub descriptor: BlobDescriptor,
    pub expires_at: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveBlob {
    token: CapabilityToken,
    descriptor: BlobDescriptor,
    observed_before: Option<WorktreeIdentity>,
}

impl ActiveBlob {
    #[must_use]
    pub fn descriptor(&self) -> &BlobDescriptor {
        &self.descriptor
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BlobOutcome {
    Complete(Vec<u8>),
    StaleDiscarded,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlobError {
    InvalidLimits,
    InvalidDescriptor,
    Capacity,
    DuplicateToken,
    UnknownToken,
    Expired,
    IdentityMismatch,
    TooLarge,
    WrongIdentityKind,
    ReadFailed,
}

#[derive(Debug)]
pub struct BlobCapabilities {
    capacity: usize,
    ttl: u64,
    prepared: BTreeMap<CapabilityToken, PreparedBlob>,
}

impl BlobCapabilities {
    /// Creates a bounded capability collection.
    ///
    /// # Errors
    ///
    /// Rejects zero capacity or time-to-live.
    pub fn new(capacity: usize, ttl: u64) -> Result<Self, BlobError> {
        if capacity == 0 || ttl == 0 {
            return Err(BlobError::InvalidLimits);
        }
        Ok(Self {
            capacity,
            ttl,
            prepared: BTreeMap::new(),
        })
    }

    /// Prepares a single-use capability for an already resolved entry side.
    ///
    /// # Errors
    ///
    /// Rejects invalid descriptors, duplicate tokens, capacity exhaustion, and
    /// expiry overflow. Descriptors never contain filesystem paths.
    pub fn prepare(
        &mut self,
        token: CapabilityToken,
        descriptor: BlobDescriptor,
        now: u64,
    ) -> Result<PreparedBlob, BlobError> {
        validate_descriptor(&descriptor)?;
        if self.prepared.contains_key(&token) {
            return Err(BlobError::DuplicateToken);
        }
        if self.prepared.len() == self.capacity {
            return Err(BlobError::Capacity);
        }
        let expires_at = now.checked_add(self.ttl).ok_or(BlobError::InvalidLimits)?;
        let prepared = PreparedBlob {
            token: token.clone(),
            descriptor,
            expires_at,
        };
        self.prepared.insert(token, prepared.clone());
        Ok(prepared)
    }

    /// Consumes a capability before streaming begins.
    ///
    /// `observed_before` must exactly reproduce a worktree identity; immutable
    /// object and empty sides do not accept worktree observations.
    ///
    /// # Errors
    ///
    /// Rejects unknown, expired, or identity-mismatched capabilities. A rejected
    /// token is consumed fail-closed.
    pub fn begin(
        &mut self,
        token: &CapabilityToken,
        now: u64,
        observed_before: Option<WorktreeIdentity>,
    ) -> Result<ActiveBlob, BlobError> {
        let prepared = self.prepared.remove(token).ok_or(BlobError::UnknownToken)?;
        if now >= prepared.expires_at {
            return Err(BlobError::Expired);
        }
        match (&prepared.descriptor.identity, observed_before.as_ref()) {
            (BlobIdentity::Worktree(expected), Some(observed)) if expected == observed => {}
            (BlobIdentity::Worktree(_), _) => return Err(BlobError::IdentityMismatch),
            (BlobIdentity::ObjectId(_) | BlobIdentity::Empty, Some(_)) => {
                return Err(BlobError::WrongIdentityKind);
            }
            (BlobIdentity::ObjectId(_) | BlobIdentity::Empty, None) => {}
        }
        Ok(ActiveBlob {
            token: prepared.token,
            descriptor: prepared.descriptor,
            observed_before,
        })
    }

    /// Finishes a stream atomically while retaining at most `max_bytes`.
    ///
    /// Worktree bytes are discarded unless the post-stream identity equals the
    /// pre-stream identity. Immutable object/empty sides require no observation.
    ///
    /// # Errors
    ///
    /// Rejects oversized bytes or an observation of the wrong identity kind.
    pub fn finish<R: Read>(
        active: ActiveBlob,
        reader: &mut R,
        observed_after: Option<&WorktreeIdentity>,
    ) -> Result<BlobOutcome, BlobError> {
        let ActiveBlob {
            token: _,
            descriptor,
            observed_before,
        } = active;
        let bytes = read_bounded(reader, descriptor.max_bytes)?;
        match descriptor.identity {
            BlobIdentity::Worktree(_) => {
                if observed_after == observed_before.as_ref() {
                    Ok(BlobOutcome::Complete(bytes))
                } else {
                    Ok(BlobOutcome::StaleDiscarded)
                }
            }
            BlobIdentity::ObjectId(_) | BlobIdentity::Empty if observed_after.is_some() => {
                Err(BlobError::WrongIdentityKind)
            }
            BlobIdentity::ObjectId(_) | BlobIdentity::Empty => Ok(BlobOutcome::Complete(bytes)),
        }
    }

    #[must_use]
    pub fn cancel(active: ActiveBlob) -> BlobOutcome {
        drop(active);
        BlobOutcome::Cancelled
    }

    /// Removes expired, unused capabilities using injected time.
    pub fn expire(&mut self, now: u64) {
        self.prepared
            .retain(|_, prepared| now < prepared.expires_at);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.prepared.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.prepared.is_empty()
    }
}

fn read_bounded(reader: &mut impl Read, max_bytes: usize) -> Result<Vec<u8>, BlobError> {
    let mut bytes = Vec::with_capacity(max_bytes.min(8 * 1024));
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        if bytes.len() == max_bytes {
            return match reader
                .read(&mut chunk[..1])
                .map_err(|_| BlobError::ReadFailed)?
            {
                0 => Ok(bytes),
                _ => Err(BlobError::TooLarge),
            };
        }
        let remaining = max_bytes - bytes.len();
        let chunk_len = chunk.len();
        let read = reader
            .read(&mut chunk[..remaining.min(chunk_len)])
            .map_err(|_| BlobError::ReadFailed)?;
        if read == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
}

fn validate_descriptor(descriptor: &BlobDescriptor) -> Result<(), BlobError> {
    let valid_identity = match &descriptor.identity {
        BlobIdentity::Empty => true,
        BlobIdentity::ObjectId(id) => !id.is_empty() && id.len() <= 128,
        BlobIdentity::Worktree(identity) => identity
            .digest
            .as_ref()
            .is_none_or(|digest| !digest.is_empty() && digest.len() <= 128),
    };
    if descriptor.max_bytes == 0 || !valid_identity {
        Err(BlobError::InvalidDescriptor)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn address(side: Side) -> BlobAddress {
        BlobAddress {
            snapshot_id: SnapshotId::new("snapshot-1"),
            entry_id: EntryId::new("entry-1"),
            comparison: Comparison::Working,
            side,
        }
    }

    fn worktree() -> WorktreeIdentity {
        WorktreeIdentity {
            file_id: 7,
            size: 3,
            modified: 9,
            digest: Some("abc".into()),
        }
    }

    fn descriptor(identity: BlobIdentity) -> BlobDescriptor {
        BlobDescriptor {
            address: address(Side::Right),
            identity,
            max_bytes: 4,
        }
    }

    #[test]
    fn object_capability_is_single_use_and_path_free() {
        let mut capabilities = BlobCapabilities::new(2, 10).unwrap();
        let token = CapabilityToken::new("opaque-1");
        capabilities
            .prepare(
                token.clone(),
                descriptor(BlobIdentity::ObjectId("oid".into())),
                5,
            )
            .unwrap();
        let active = capabilities.begin(&token, 6, None).unwrap();
        assert_eq!(
            BlobCapabilities::finish(active, &mut Cursor::new(b"data"), None).unwrap(),
            BlobOutcome::Complete(b"data".to_vec())
        );
        assert_eq!(
            capabilities.begin(&token, 6, None),
            Err(BlobError::UnknownToken)
        );
    }

    #[test]
    fn worktree_precheck_fails_closed_and_postcheck_discards_bytes() {
        let mut capabilities = BlobCapabilities::new(2, 10).unwrap();
        let token = CapabilityToken::new("worktree");
        capabilities
            .prepare(
                token.clone(),
                descriptor(BlobIdentity::Worktree(worktree())),
                0,
            )
            .unwrap();
        assert_eq!(
            capabilities.begin(
                &token,
                1,
                Some(WorktreeIdentity {
                    size: 4,
                    ..worktree()
                })
            ),
            Err(BlobError::IdentityMismatch)
        );

        let token = CapabilityToken::new("worktree-2");
        capabilities
            .prepare(
                token.clone(),
                descriptor(BlobIdentity::Worktree(worktree())),
                0,
            )
            .unwrap();
        let active = capabilities.begin(&token, 1, Some(worktree())).unwrap();
        let changed = WorktreeIdentity {
            modified: 10,
            ..worktree()
        };
        assert_eq!(
            BlobCapabilities::finish(active, &mut Cursor::new(b"new"), Some(&changed)).unwrap(),
            BlobOutcome::StaleDiscarded
        );
    }

    #[test]
    fn expiry_capacity_and_cancellation_are_deterministic() {
        let mut capabilities = BlobCapabilities::new(1, 2).unwrap();
        let first = CapabilityToken::new("first");
        capabilities
            .prepare(first.clone(), descriptor(BlobIdentity::Empty), 10)
            .unwrap();
        assert_eq!(
            capabilities.prepare(
                CapabilityToken::new("second"),
                descriptor(BlobIdentity::Empty),
                10
            ),
            Err(BlobError::Capacity)
        );
        assert_eq!(
            capabilities.begin(&first, 12, None),
            Err(BlobError::Expired)
        );
        capabilities
            .prepare(first.clone(), descriptor(BlobIdentity::Empty), 20)
            .unwrap();
        let active = capabilities.begin(&first, 20, None).unwrap();
        assert_eq!(BlobCapabilities::cancel(active), BlobOutcome::Cancelled);
    }

    #[test]
    fn immutable_identities_reject_worktree_evidence_and_bound_streaming_reads() {
        let mut capabilities = BlobCapabilities::new(1, 10).unwrap();
        let token = CapabilityToken::new("head");
        capabilities
            .prepare(
                token.clone(),
                descriptor(BlobIdentity::ObjectId("head-oid".into())),
                0,
            )
            .unwrap();
        assert_eq!(
            capabilities.begin(&token, 1, Some(worktree())),
            Err(BlobError::WrongIdentityKind)
        );

        let token = CapabilityToken::new("index");
        capabilities
            .prepare(
                token.clone(),
                descriptor(BlobIdentity::ObjectId("index-oid".into())),
                0,
            )
            .unwrap();
        let active = capabilities.begin(&token, 1, None).unwrap();
        let mut oversized = Cursor::new(vec![0; 1024 * 1024]);
        assert_eq!(
            BlobCapabilities::finish(active, &mut oversized, None),
            Err(BlobError::TooLarge)
        );
        assert_eq!(oversized.position(), 5);
    }

    #[test]
    fn expiry_sweep_preserves_only_live_tokens() {
        let mut capabilities = BlobCapabilities::new(2, 5).unwrap();
        capabilities
            .prepare(
                CapabilityToken::new("old"),
                descriptor(BlobIdentity::Empty),
                0,
            )
            .unwrap();
        capabilities
            .prepare(
                CapabilityToken::new("new"),
                descriptor(BlobIdentity::Empty),
                4,
            )
            .unwrap();
        capabilities.expire(5);
        assert_eq!(capabilities.len(), 1);
    }
}
