//! Versioned, bounded annotation export bytes with no filesystem authority.

use std::collections::BTreeSet;

const MAGIC: &[u8; 8] = b"CHANNEXP";
const HEADER: usize = 18;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExportLimits {
    pub max_records: usize,
    pub max_payload_bytes: usize,
    pub max_field_bytes: usize,
    pub max_body_bytes: usize,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ExportRecord {
    pub annotation_id: String,
    pub document: String,
    pub range_start: u64,
    pub range_end: u64,
    pub status: ExportStatus,
    pub context: Option<ExportContext>,
    pub body: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ExportContext {
    pub prefix: String,
    pub suffix: String,
    pub selected_digest: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ExportStatus {
    Attached,
    Ambiguous,
    Orphaned,
}

/// Canonically encodes v1 records sorted by document and annotation ID.
///
/// # Errors
///
/// Rejects invalid limits, duplicate IDs, invalid ranges/fields, and payload
/// overflow.
pub fn encode(records: &[ExportRecord], limits: ExportLimits) -> Result<Vec<u8>, ExportCodecError> {
    validate_limits(limits)?;
    let mut records = records.to_vec();
    records.sort();
    validate_records(&records, limits)?;
    let mut payload = Vec::new();
    push_u32(&mut payload, records.len())?;
    for record in records {
        push_text(&mut payload, &record.annotation_id)?;
        push_text(&mut payload, &record.document)?;
        payload.extend_from_slice(&record.range_start.to_be_bytes());
        payload.extend_from_slice(&record.range_end.to_be_bytes());
        payload.push(match record.status {
            ExportStatus::Attached => 0,
            ExportStatus::Ambiguous => 1,
            ExportStatus::Orphaned => 2,
        });
        if let Some(context) = record.context {
            payload.push(1);
            push_text(&mut payload, &context.prefix)?;
            push_text(&mut payload, &context.suffix)?;
            payload.extend_from_slice(&context.selected_digest);
        } else {
            payload.push(0);
        }
        push_text(&mut payload, &record.body)?;
    }
    envelope(1, &payload, limits)
}

/// Decodes v1 or migrates v0 records, which had no context field.
///
/// # Errors
///
/// Rejects bad magic, unknown versions, length/checksum mismatch, corruption,
/// trailing bytes, duplicates, invalid UTF-8/ranges, and configured limits.
pub fn decode(bytes: &[u8], limits: ExportLimits) -> Result<Vec<ExportRecord>, ExportCodecError> {
    validate_limits(limits)?;
    if bytes.len() < HEADER || &bytes[..8] != MAGIC {
        return Err(ExportCodecError::InvalidHeader);
    }
    let version = u16::from_be_bytes([bytes[8], bytes[9]]);
    let length = u32::from_be_bytes(
        bytes[10..14]
            .try_into()
            .map_err(|_| ExportCodecError::Truncated)?,
    ) as usize;
    let sum = u32::from_be_bytes(
        bytes[14..18]
            .try_into()
            .map_err(|_| ExportCodecError::Truncated)?,
    );
    if length > limits.max_payload_bytes {
        return Err(ExportCodecError::PayloadLimit);
    }
    if bytes.len() != HEADER.saturating_add(length) {
        return Err(if bytes.len() < HEADER.saturating_add(length) {
            ExportCodecError::Truncated
        } else {
            ExportCodecError::Trailing
        });
    }
    let payload = &bytes[HEADER..];
    if checksum(payload) != sum {
        return Err(ExportCodecError::Checksum);
    }
    if version > 1 {
        return Err(ExportCodecError::UnknownVersion);
    }
    let mut reader = Reader {
        bytes: payload,
        offset: 0,
    };
    let count = reader.u32()?;
    if count > limits.max_records {
        return Err(ExportCodecError::RecordLimit);
    }
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let annotation_id = reader.text(limits.max_field_bytes)?;
        let document = reader.text(limits.max_field_bytes)?;
        let range_start = reader.u64()?;
        let range_end = reader.u64()?;
        let status = match reader.byte()? {
            0 => ExportStatus::Attached,
            1 => ExportStatus::Ambiguous,
            2 => ExportStatus::Orphaned,
            _ => return Err(ExportCodecError::Corrupt),
        };
        let context = if version == 0 {
            None
        } else {
            match reader.byte()? {
                0 => None,
                1 => Some(ExportContext {
                    prefix: reader.text(limits.max_field_bytes)?,
                    suffix: reader.text(limits.max_field_bytes)?,
                    selected_digest: reader
                        .take(32)?
                        .try_into()
                        .map_err(|_| ExportCodecError::Truncated)?,
                }),
                _ => return Err(ExportCodecError::Corrupt),
            }
        };
        let body = reader.text(limits.max_body_bytes)?;
        records.push(ExportRecord {
            annotation_id,
            document,
            range_start,
            range_end,
            status,
            context,
            body,
        });
    }
    reader.finish()?;
    records.sort();
    validate_records(&records, limits)?;
    Ok(records)
}

fn validate_limits(l: ExportLimits) -> Result<(), ExportCodecError> {
    if l.max_records == 0
        || l.max_payload_bytes == 0
        || l.max_field_bytes == 0
        || l.max_body_bytes == 0
    {
        Err(ExportCodecError::InvalidLimits)
    } else {
        Ok(())
    }
}

fn validate_records(records: &[ExportRecord], l: ExportLimits) -> Result<(), ExportCodecError> {
    if records.len() > l.max_records {
        return Err(ExportCodecError::RecordLimit);
    }
    let mut keys = BTreeSet::new();
    for r in records {
        validate_text(&r.annotation_id, l.max_field_bytes)?;
        validate_text(&r.document, l.max_field_bytes)?;
        validate_text(&r.body, l.max_body_bytes)?;
        if r.range_start >= r.range_end {
            return Err(ExportCodecError::InvalidRange);
        }
        if !keys.insert((r.document.as_str(), r.annotation_id.as_str())) {
            return Err(ExportCodecError::Duplicate);
        }
        if let Some(c) = &r.context {
            validate_text_allow_empty(&c.prefix, l.max_field_bytes)?;
            validate_text_allow_empty(&c.suffix, l.max_field_bytes)?;
        }
    }
    Ok(())
}

fn validate_text(v: &str, max: usize) -> Result<(), ExportCodecError> {
    if v.is_empty() {
        return Err(ExportCodecError::InvalidField);
    }
    validate_text_allow_empty(v, max)
}
fn validate_text_allow_empty(v: &str, max: usize) -> Result<(), ExportCodecError> {
    if v.len() > max || v.chars().any(|c| c == '\0') {
        Err(ExportCodecError::InvalidField)
    } else {
        Ok(())
    }
}
fn push_text(out: &mut Vec<u8>, v: &str) -> Result<(), ExportCodecError> {
    push_u32(out, v.len())?;
    out.extend_from_slice(v.as_bytes());
    Ok(())
}
fn push_u32(out: &mut Vec<u8>, v: usize) -> Result<(), ExportCodecError> {
    out.extend_from_slice(
        &u32::try_from(v)
            .map_err(|_| ExportCodecError::PayloadLimit)?
            .to_be_bytes(),
    );
    Ok(())
}
fn envelope(version: u16, payload: &[u8], l: ExportLimits) -> Result<Vec<u8>, ExportCodecError> {
    if payload.len() > l.max_payload_bytes {
        return Err(ExportCodecError::PayloadLimit);
    }
    let mut out = Vec::with_capacity(HEADER + payload.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&version.to_be_bytes());
    push_u32(&mut out, payload.len())?;
    out.extend_from_slice(&checksum(payload).to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}
fn checksum(bytes: &[u8]) -> u32 {
    bytes.iter().fold(2_166_136_261, |sum, byte| {
        (sum ^ u32::from(*byte)).wrapping_mul(16_777_619)
    })
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ExportCodecError> {
        let end = self
            .offset
            .checked_add(n)
            .ok_or(ExportCodecError::Truncated)?;
        let v = self
            .bytes
            .get(self.offset..end)
            .ok_or(ExportCodecError::Truncated)?;
        self.offset = end;
        Ok(v)
    }
    fn byte(&mut self) -> Result<u8, ExportCodecError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<usize, ExportCodecError> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| ExportCodecError::Truncated)?,
        ) as usize)
    }
    fn u64(&mut self) -> Result<u64, ExportCodecError> {
        Ok(u64::from_be_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| ExportCodecError::Truncated)?,
        ))
    }
    fn text(&mut self, max: usize) -> Result<String, ExportCodecError> {
        let n = self.u32()?;
        if n > max {
            return Err(ExportCodecError::InvalidField);
        }
        std::str::from_utf8(self.take(n)?)
            .map(str::to_owned)
            .map_err(|_| ExportCodecError::InvalidField)
    }
    fn finish(self) -> Result<(), ExportCodecError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(ExportCodecError::Trailing)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportCodecError {
    InvalidLimits,
    InvalidHeader,
    UnknownVersion,
    PayloadLimit,
    RecordLimit,
    InvalidField,
    InvalidRange,
    Duplicate,
    Checksum,
    Corrupt,
    Truncated,
    Trailing,
}

#[cfg(test)]
mod tests {
    use super::*;
    const L: ExportLimits = ExportLimits {
        max_records: 4,
        max_payload_bytes: 2048,
        max_field_bytes: 64,
        max_body_bytes: 128,
    };
    fn record(id: &str) -> ExportRecord {
        ExportRecord {
            annotation_id: id.into(),
            document: "docs/a.md".into(),
            range_start: 1,
            range_end: 3,
            status: ExportStatus::Attached,
            context: Some(ExportContext {
                prefix: "p".into(),
                suffix: "s".into(),
                selected_digest: [7; 32],
            }),
            body: "note".into(),
        }
    }
    #[test]
    fn roundtrip_is_canonical() {
        let bytes = encode(&[record("b"), record("a")], L).unwrap();
        let decoded = decode(&bytes, L).unwrap();
        assert_eq!(decoded[0].annotation_id, "a");
        assert_eq!(encode(&decoded, L).unwrap(), bytes);
    }
    #[test]
    fn corruption_truncation_trailing_and_version_fail() {
        let bytes = encode(&[record("a")], L).unwrap();
        let mut corrupt = bytes.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert_eq!(decode(&corrupt, L), Err(ExportCodecError::Checksum));
        assert_eq!(
            decode(&bytes[..bytes.len() - 1], L),
            Err(ExportCodecError::Truncated)
        );
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(decode(&trailing, L), Err(ExportCodecError::Trailing));
        let mut version = bytes;
        version[8..10].copy_from_slice(&2_u16.to_be_bytes());
        assert_eq!(decode(&version, L), Err(ExportCodecError::UnknownVersion));
    }
    #[test]
    fn duplicates_and_invalid_ranges_fail() {
        assert_eq!(
            encode(&[record("a"), record("a")], L),
            Err(ExportCodecError::Duplicate)
        );
        let mut invalid = record("a");
        invalid.range_end = 1;
        assert_eq!(encode(&[invalid], L), Err(ExportCodecError::InvalidRange));
    }
    #[test]
    fn v0_fixture_migrates_without_context() {
        let r = record("a");
        let mut p = Vec::new();
        push_u32(&mut p, 1).unwrap();
        push_text(&mut p, &r.annotation_id).unwrap();
        push_text(&mut p, &r.document).unwrap();
        p.extend_from_slice(&r.range_start.to_be_bytes());
        p.extend_from_slice(&r.range_end.to_be_bytes());
        p.push(0);
        push_text(&mut p, &r.body).unwrap();
        let bytes = envelope(0, &p, L).unwrap();
        assert_eq!(decode(&bytes, L).unwrap()[0].context, None);
    }
}
