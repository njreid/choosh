//! Explicit, bounded and redacted support-bundle export.

use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticRecord {
    pub code: String,
    pub count: u64,
    pub elapsed_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticManifest {
    pub app_version: String,
    pub protocol_version: u16,
    pub records: Vec<DiagnosticRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportError {
    TooManyRecords,
    TooLarge,
    InvalidCode,
}

/// Serializes only the allow-listed fields; no free-form diagnostics are accepted.
pub fn export(
    manifest: &DiagnosticManifest,
    max_records: usize,
    max_bytes: usize,
) -> Result<Vec<u8>, ExportError> {
    if manifest.records.len() > max_records {
        return Err(ExportError::TooManyRecords);
    }
    if manifest.records.iter().any(|r| !valid_code(&r.code)) {
        return Err(ExportError::InvalidCode);
    }
    let mut records = manifest.records.clone();
    records.sort_by(|a, b| a.code.cmp(&b.code));
    let value = json!({
        "schema": "choosh.diagnostics.v1",
        "app_version": manifest.app_version,
        "protocol_version": manifest.protocol_version,
        "records": records.iter().map(|r| json!({"code": r.code, "count": r.count, "elapsed_ms": r.elapsed_ms})).collect::<Vec<_>>(),
    });
    let bytes = serde_json::to_vec(&value).expect("diagnostic manifest is serializable");
    if bytes.len() > max_bytes {
        return Err(ExportError::TooLarge);
    }
    Ok(bytes)
}

fn valid_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 64
        && code.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-' || b == b'.'
        })
}

pub fn parse(bytes: &[u8]) -> Option<Value> {
    serde_json::from_slice(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> DiagnosticManifest {
        DiagnosticManifest {
            app_version: "0.0.1".into(),
            protocol_version: 1,
            records: vec![DiagnosticRecord {
                code: "reconnect.count".into(),
                count: 2,
                elapsed_ms: 40,
            }],
        }
    }

    #[test]
    fn export_is_canonical_and_bounded() {
        let out = export(&manifest(), 4, 512).unwrap();
        assert_eq!(parse(&out).unwrap()["schema"], "choosh.diagnostics.v1");
        assert!(
            String::from_utf8(out)
                .unwrap()
                .find("reconnect.count")
                .is_some()
        );
    }

    #[test]
    fn rejects_unknown_or_free_form_codes() {
        let mut m = manifest();
        m.records[0].code = "host=secret".into();
        assert_eq!(export(&m, 4, 512), Err(ExportError::InvalidCode));
    }

    #[test]
    fn rejects_record_and_byte_limits_without_partial_output() {
        assert_eq!(
            export(&manifest(), 0, 512),
            Err(ExportError::TooManyRecords)
        );
        assert_eq!(export(&manifest(), 4, 8), Err(ExportError::TooLarge));
    }
}
