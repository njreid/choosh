//! Bounded policy primitives for confined Markdown asset delivery.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

const MAX_ID_BYTES: usize = 128;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AssetId(String);

impl AssetId {
    /// Parses a bounded opaque identifier.
    ///
    /// # Errors
    ///
    /// Rejects empty, oversized, or control-containing identifiers.
    pub fn parse(value: impl Into<String>) -> Result<Self, AssetError> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_ID_BYTES || value.chars().any(char::is_control) {
            return Err(AssetError::InvalidIdentity);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfinedAssetPath(PathBuf);

impl ConfinedAssetPath {
    /// Parses a bounded root-relative lexical identity.
    ///
    /// # Errors
    ///
    /// Rejects absolute paths, traversal, schemes, encoded separators, NULs,
    /// empty paths, and paths beyond the configured byte/component ceilings.
    pub fn parse(value: &str, max_bytes: usize, max_components: usize) -> Result<Self, AssetError> {
        if max_bytes == 0
            || max_components == 0
            || value.is_empty()
            || value.len() > max_bytes
            || value.contains('\0')
            || value.contains("%2f")
            || value.contains("%2F")
            || value.contains("%5c")
            || value.contains("%5C")
            || value.contains('%')
            || value.contains("://")
            || value.starts_with("//")
            || value.contains('\\')
        {
            return Err(AssetError::UnsafePath);
        }
        let path = Path::new(value);
        if path.is_absolute() {
            return Err(AssetError::UnsafePath);
        }
        let mut count = 0;
        for component in path.components() {
            match component {
                Component::Normal(_) => count += 1,
                _ => return Err(AssetError::UnsafePath),
            }
            if count > max_components {
                return Err(AssetError::PathLimit);
            }
        }
        Ok(Self(path.to_owned()))
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeMediaType {
    Png,
    Jpeg,
    Gif,
    WebP,
}

impl SafeMediaType {
    #[must_use]
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::WebP => "image/webp",
        }
    }
}

/// Classifies only allowlisted passive image bytes by magic signature.
///
/// # Errors
///
/// Rejects truncated, active, archive, textual, or unknown media.
pub fn classify_media(prefix: &[u8]) -> Result<SafeMediaType, AssetError> {
    if prefix.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok(SafeMediaType::Png)
    } else if prefix.starts_with(b"\xff\xd8\xff") {
        Ok(SafeMediaType::Jpeg)
    } else if prefix.starts_with(b"GIF87a") || prefix.starts_with(b"GIF89a") {
        Ok(SafeMediaType::Gif)
    } else if prefix.len() >= 12 && &prefix[..4] == b"RIFF" && &prefix[8..12] == b"WEBP" {
        Ok(SafeMediaType::WebP)
    } else {
        Err(AssetError::UnsupportedMedia)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ByteRange {
    pub start: u64,
    pub end_inclusive: u64,
}

impl ByteRange {
    #[must_use]
    pub fn len(self) -> u64 {
        self.end_inclusive - self.start + 1
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.start > self.end_inclusive
    }
}

/// Parses one RFC-style `bytes` range against a known nonzero size.
///
/// # Errors
///
/// Rejects multiple, malformed, empty, unsatisfiable, and oversized ranges.
pub fn parse_byte_range(
    header: Option<&str>,
    size: u64,
    max_range_bytes: u64,
) -> Result<ByteRange, AssetError> {
    if size == 0 || max_range_bytes == 0 {
        return Err(AssetError::RangeUnsatisfiable);
    }
    let range = if let Some(header) = header {
        let value = header
            .strip_prefix("bytes=")
            .ok_or(AssetError::RangeInvalid)?;
        if value.contains(',') {
            return Err(AssetError::RangeInvalid);
        }
        let (start, end) = value.split_once('-').ok_or(AssetError::RangeInvalid)?;
        match (start.is_empty(), end.is_empty()) {
            (true, false) => {
                let suffix = parse_u64(end)?;
                if suffix == 0 {
                    return Err(AssetError::RangeUnsatisfiable);
                }
                ByteRange {
                    start: size.saturating_sub(suffix),
                    end_inclusive: size - 1,
                }
            }
            (false, true) => {
                let start = parse_u64(start)?;
                if start >= size {
                    return Err(AssetError::RangeUnsatisfiable);
                }
                ByteRange {
                    start,
                    end_inclusive: size - 1,
                }
            }
            (false, false) => {
                let start = parse_u64(start)?;
                let end = parse_u64(end)?;
                if start > end || start >= size {
                    return Err(AssetError::RangeUnsatisfiable);
                }
                ByteRange {
                    start,
                    end_inclusive: end.min(size - 1),
                }
            }
            (true, true) => return Err(AssetError::RangeInvalid),
        }
    } else {
        ByteRange {
            start: 0,
            end_inclusive: size - 1,
        }
    };
    if range.len() > max_range_bytes {
        return Err(AssetError::RangeLimit);
    }
    Ok(range)
}

fn parse_u64(value: &str) -> Result<u64, AssetError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AssetError::RangeInvalid);
    }
    value.parse().map_err(|_| AssetError::RangeInvalid)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamLimits {
    pub max_chunk_bytes: usize,
    pub max_asset_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetStreamBudget {
    limits: StreamLimits,
    accepted: u64,
}

impl AssetStreamBudget {
    /// Constructs nonzero stream limits.
    ///
    /// # Errors
    ///
    /// Rejects a zero chunk or asset ceiling.
    pub fn new(limits: StreamLimits) -> Result<Self, AssetError> {
        if limits.max_chunk_bytes == 0 || limits.max_asset_bytes == 0 {
            return Err(AssetError::InvalidLimits);
        }
        Ok(Self {
            limits,
            accepted: 0,
        })
    }

    /// Accounts for a received chunk before it is forwarded or cached.
    ///
    /// # Errors
    ///
    /// Rejects empty/oversized chunks and aggregate asset overflow.
    pub fn accept_chunk(&mut self, chunk: &[u8]) -> Result<(), AssetError> {
        if chunk.is_empty() || chunk.len() > self.limits.max_chunk_bytes {
            return Err(AssetError::ChunkLimit);
        }
        self.accepted = self
            .accepted
            .checked_add(chunk.len() as u64)
            .ok_or(AssetError::AssetLimit)?;
        if self.accepted > self.limits.max_asset_bytes {
            return Err(AssetError::AssetLimit);
        }
        Ok(())
    }

    #[must_use]
    pub fn accepted_bytes(&self) -> u64 {
        self.accepted
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestGeneration {
    pub expected: u64,
    pub current: u64,
    pub cancelled: bool,
}

impl RequestGeneration {
    /// Ensures a request remains current and uncancelled.
    ///
    /// # Errors
    ///
    /// Returns stable cancellation or stale-generation errors.
    pub fn validate(self) -> Result<(), AssetError> {
        if self.cancelled {
            Err(AssetError::Cancelled)
        } else if self.expected != self.current {
            Err(AssetError::StaleGeneration)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CacheKey {
    pub asset_id: AssetId,
    pub immutable_generation: u64,
    pub range: ByteRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CacheEntry {
    bytes: Vec<u8>,
    media_type: SafeMediaType,
    used_at: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetCache {
    max_entries: usize,
    max_bytes: usize,
    used_bytes: usize,
    clock: u64,
    entries: BTreeMap<CacheKey, CacheEntry>,
}

impl AssetCache {
    /// Constructs a cache with explicit nonzero ceilings.
    ///
    /// # Errors
    ///
    /// Rejects zero entry or byte limits.
    pub fn new(max_entries: usize, max_bytes: usize) -> Result<Self, AssetError> {
        if max_entries == 0 || max_bytes == 0 {
            return Err(AssetError::InvalidLimits);
        }
        Ok(Self {
            max_entries,
            max_bytes,
            used_bytes: 0,
            clock: 0,
            entries: BTreeMap::new(),
        })
    }

    pub fn get(&mut self, key: &CacheKey) -> Option<(&[u8], SafeMediaType)> {
        self.clock = self.clock.saturating_add(1);
        let entry = self.entries.get_mut(key)?;
        entry.used_at = self.clock;
        Some((&entry.bytes, entry.media_type))
    }

    /// Inserts immutable safe bytes, evicting least-recently-used entries.
    ///
    /// # Errors
    ///
    /// Rejects empty values and values larger than total cache capacity.
    pub fn insert(
        &mut self,
        key: CacheKey,
        bytes: Vec<u8>,
        media_type: SafeMediaType,
    ) -> Result<(), AssetError> {
        if bytes.is_empty() || bytes.len() > self.max_bytes {
            return Err(AssetError::CacheLimit);
        }
        if let Some(old) = self.entries.remove(&key) {
            self.used_bytes -= old.bytes.len();
        }
        while self.entries.len() >= self.max_entries
            || self.used_bytes.saturating_add(bytes.len()) > self.max_bytes
        {
            let victim = self
                .entries
                .iter()
                .min_by_key(|(key, entry)| (entry.used_at, *key))
                .map(|(key, _)| key.clone())
                .ok_or(AssetError::CacheLimit)?;
            let removed = self.entries.remove(&victim).ok_or(AssetError::CacheLimit)?;
            self.used_bytes -= removed.bytes.len();
        }
        self.clock = self.clock.saturating_add(1);
        self.used_bytes += bytes.len();
        self.entries.insert(
            key,
            CacheEntry {
                bytes,
                media_type,
                used_at: self.clock,
            },
        );
        Ok(())
    }

    #[must_use]
    pub fn usage(&self) -> (usize, usize) {
        (self.entries.len(), self.used_bytes)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NavigationPolicy {
    ConfinedAsset,
    InertExternal,
    Rejected,
}

/// Classifies links without granting redirects or external network access.
#[must_use]
pub fn navigation_policy(target: &str) -> NavigationPolicy {
    let normalized = target.trim().to_ascii_lowercase();
    if normalized.starts_with("//")
        || normalized
            .split('/')
            .next()
            .is_some_and(|part| part.contains(':'))
    {
        NavigationPolicy::InertExternal
    } else if ConfinedAssetPath::parse(target, 2_048, 64).is_err() {
        NavigationPolicy::Rejected
    } else {
        NavigationPolicy::ConfinedAsset
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetPlaceholder {
    Unavailable,
    Unsupported,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetError {
    InvalidIdentity,
    InvalidLimits,
    UnsafePath,
    PathLimit,
    UnsupportedMedia,
    RangeInvalid,
    RangeUnsatisfiable,
    RangeLimit,
    ChunkLimit,
    AssetLimit,
    CacheLimit,
    Cancelled,
    StaleGeneration,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: &str) -> CacheKey {
        CacheKey {
            asset_id: AssetId::parse(id).unwrap(),
            immutable_generation: 1,
            range: ByteRange {
                start: 0,
                end_inclusive: 0,
            },
        }
    }

    #[test]
    fn paths_reject_traversal_schemes_and_encoded_separators() {
        assert!(ConfinedAssetPath::parse("images/a.png", 100, 4).is_ok());
        for unsafe_path in [
            "../secret",
            "/etc/passwd",
            "https://example.invalid/a",
            "a%2fb",
        ] {
            assert_eq!(
                ConfinedAssetPath::parse(unsafe_path, 100, 4),
                Err(AssetError::UnsafePath)
            );
        }
    }

    #[test]
    fn media_is_allowlisted_by_bytes_not_extension() {
        assert_eq!(
            classify_media(b"\x89PNG\r\n\x1a\nrest"),
            Ok(SafeMediaType::Png)
        );
        assert_eq!(
            classify_media(b"<svg><script>"),
            Err(AssetError::UnsupportedMedia)
        );
        assert_eq!(
            classify_media(b"PK\x03\x04archive"),
            Err(AssetError::UnsupportedMedia)
        );
    }

    #[test]
    fn ranges_cover_exact_open_and_suffix_forms() {
        assert_eq!(
            parse_byte_range(Some("bytes=2-4"), 10, 10).unwrap(),
            ByteRange {
                start: 2,
                end_inclusive: 4
            }
        );
        assert_eq!(
            parse_byte_range(Some("bytes=8-"), 10, 10).unwrap(),
            ByteRange {
                start: 8,
                end_inclusive: 9
            }
        );
        assert_eq!(
            parse_byte_range(Some("bytes=-3"), 10, 10).unwrap(),
            ByteRange {
                start: 7,
                end_inclusive: 9
            }
        );
        assert_eq!(
            parse_byte_range(Some("bytes=1-2,4-5"), 10, 10),
            Err(AssetError::RangeInvalid)
        );
        assert_eq!(
            parse_byte_range(Some("bytes=10-"), 10, 10),
            Err(AssetError::RangeUnsatisfiable)
        );
    }

    #[test]
    fn stream_budget_rejects_chunk_and_aggregate_overflow() {
        let mut budget = AssetStreamBudget::new(StreamLimits {
            max_chunk_bytes: 3,
            max_asset_bytes: 5,
        })
        .unwrap();
        budget.accept_chunk(b"abc").unwrap();
        assert_eq!(budget.accept_chunk(b"def"), Err(AssetError::AssetLimit));
        assert_eq!(
            AssetStreamBudget::new(StreamLimits {
                max_chunk_bytes: 2,
                max_asset_bytes: 5
            })
            .unwrap()
            .accept_chunk(b"abc"),
            Err(AssetError::ChunkLimit)
        );
    }

    #[test]
    fn cache_evicts_deterministic_least_recently_used_entry() {
        let mut cache = AssetCache::new(2, 4).unwrap();
        cache
            .insert(key("a"), vec![1, 1], SafeMediaType::Png)
            .unwrap();
        cache
            .insert(key("b"), vec![2, 2], SafeMediaType::Png)
            .unwrap();
        assert!(cache.get(&key("a")).is_some());
        cache
            .insert(key("c"), vec![3, 3], SafeMediaType::Png)
            .unwrap();
        assert!(cache.get(&key("b")).is_none());
        assert!(cache.get(&key("a")).is_some());
        assert_eq!(cache.usage(), (2, 4));
    }

    #[test]
    fn stale_cancelled_and_external_navigation_fail_closed() {
        assert_eq!(
            RequestGeneration {
                expected: 1,
                current: 2,
                cancelled: false
            }
            .validate(),
            Err(AssetError::StaleGeneration)
        );
        assert_eq!(
            RequestGeneration {
                expected: 2,
                current: 2,
                cancelled: true
            }
            .validate(),
            Err(AssetError::Cancelled)
        );
        assert_eq!(
            navigation_policy("https://example.invalid/image.png"),
            NavigationPolicy::InertExternal
        );
        assert_eq!(
            navigation_policy("images/local.png"),
            NavigationPolicy::ConfinedAsset
        );
        assert_eq!(navigation_policy("../secret"), NavigationPolicy::Rejected);
    }
}
