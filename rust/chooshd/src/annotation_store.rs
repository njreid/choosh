//! Durable, atomic storage for versioned annotation export bytes.

#[cfg(test)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use choosh_core::annotation_export::{ExportCodecError, ExportLimits, ExportRecord, decode};

use crate::storage::{StateStorage, StorageError};

#[derive(Debug)]
pub struct AnnotationStore {
    storage: StateStorage,
    limits: ExportLimits,
}

impl AnnotationStore {
    /// Opens the durable annotation-export store at an explicit path.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the path or byte cap is not a valid store.
    pub fn new(
        path: impl Into<std::path::PathBuf>,
        max_bytes: usize,
        limits: ExportLimits,
    ) -> Result<Self, StorageError> {
        Ok(Self {
            storage: StateStorage::new(path, max_bytes)?,
            limits,
        })
    }

    /// Returns the explicit path this store was constructed with.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.storage.path()
    }

    /// Atomically replaces the stored export with `bytes`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the payload exceeds the store's byte cap or
    /// the atomic replacement fails.
    pub fn save(&self, bytes: &[u8]) -> Result<(), StorageError> {
        self.storage.replace(bytes, "annotations")
    }

    /// Loads the stored export, returning `None` when nothing has been saved.
    ///
    /// # Errors
    ///
    /// Returns [`AnnotationStoreError`] when the stored bytes cannot be read or
    /// do not decode within the configured export limits.
    pub fn load(&self) -> Result<Option<Vec<ExportRecord>>, AnnotationStoreError> {
        self.storage
            .read()?
            .map(|bytes| decode(&bytes, self.limits).map_err(AnnotationStoreError::Codec))
            .transpose()
    }
}

#[derive(Debug)]
pub enum AnnotationStoreError {
    Storage(StorageError),
    Codec(ExportCodecError),
}

impl From<StorageError> for AnnotationStoreError {
    fn from(value: StorageError) -> Self {
        Self::Storage(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn limits() -> ExportLimits {
        ExportLimits {
            max_records: 4,
            max_payload_bytes: 4096,
            max_field_bytes: 128,
            max_body_bytes: 512,
        }
    }

    #[test]
    fn export_bytes_survive_store_reconstruction() {
        let dir = std::env::temp_dir().join(format!(
            "choosh-annotation-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("annotations.bin");
        let records = vec![ExportRecord {
            annotation_id: "a".into(),
            document: "README.md".into(),
            range_start: 0,
            range_end: 2,
            status: choosh_core::annotation_export::ExportStatus::Attached,
            context: None,
            body: "ok".into(),
        }];
        let bytes = choosh_core::annotation_export::encode(&records, limits()).unwrap();
        AnnotationStore::new(&path, 4096, limits())
            .unwrap()
            .save(&bytes)
            .unwrap();
        let restored = AnnotationStore::new(&path, 4096, limits())
            .unwrap()
            .load()
            .unwrap()
            .unwrap();
        assert_eq!(restored, records);
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
