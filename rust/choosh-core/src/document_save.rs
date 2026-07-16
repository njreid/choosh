//! Conflict-safe orchestration for saving a bounded encoded document snapshot.

const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteIdentity {
    pub revision: Vec<u8>,
    pub byte_len: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedSnapshot(Vec<u8>);

impl EncodedSnapshot {
    /// Creates a non-empty bounded snapshot whose bytes will be written exactly.
    ///
    /// # Errors
    /// Returns [`SaveError::InvalidSnapshot`] when empty or above the document bound.
    pub fn new(bytes: Vec<u8>) -> Result<Self, SaveError<()>> {
        if bytes.is_empty() || bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(SaveError::InvalidSnapshot);
        }
        Ok(Self(bytes))
    }
}

pub trait AtomicDocumentStore {
    type Temp;
    type Error;

    /// Reads the current identity without following a client-supplied alternate path.
    /// # Errors
    /// Returns an adapter-specific metadata failure.
    fn identity(&mut self) -> Result<RemoteIdentity, Self::Error>;
    /// Exclusively creates a sibling temporary file.
    /// # Errors
    /// Returns an adapter-specific creation failure.
    fn create_sibling_temp(&mut self) -> Result<Self::Temp, Self::Error>;
    /// Writes bytes and reports the exact persisted byte count.
    /// # Errors
    /// Returns an adapter-specific write failure.
    fn write_all(&mut self, temp: &mut Self::Temp, bytes: &[u8]) -> Result<u64, Self::Error>;
    /// Atomically replaces the destination; adapters must fail if unsupported.
    /// # Errors
    /// Returns an adapter-specific replacement failure.
    fn atomic_replace(&mut self, temp: Self::Temp) -> Result<(), Self::Error>;
    /// Best-effort cleanup of an uncommitted sibling temporary.
    /// # Errors
    /// Returns an adapter-specific cleanup failure.
    fn cleanup(&mut self, temp: Self::Temp) -> Result<(), Self::Error>;
}

pub trait Cancellation {
    fn cancelled(&self) -> bool;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaveOutcome {
    Saved,
    CancelledBeforeWrite,
    CancelledBeforeReplace,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SaveError<E> {
    InvalidSnapshot,
    Preflight(E),
    Conflict,
    CreateTemp(E),
    Write(E),
    PartialWrite { expected: u64, actual: u64 },
    Recheck(E),
    AtomicReplace(E),
}

/// Saves only if both remote identity observations match the caller's base identity.
///
/// Cleanup failures are deliberately best-effort and never mask the primary outcome. Once atomic
/// replacement begins, cancellation cannot report the save as cancelled.
///
/// # Errors
/// Returns a phase-specific adapter error, conflict, or partial-write failure.
pub fn save_document<S: AtomicDocumentStore, C: Cancellation>(
    store: &mut S,
    cancellation: &C,
    expected: &RemoteIdentity,
    snapshot: &EncodedSnapshot,
) -> Result<SaveOutcome, SaveError<S::Error>> {
    let initial = store.identity().map_err(SaveError::Preflight)?;
    if initial != *expected {
        return Err(SaveError::Conflict);
    }
    if cancellation.cancelled() {
        return Ok(SaveOutcome::CancelledBeforeWrite);
    }
    let mut temp = store.create_sibling_temp().map_err(SaveError::CreateTemp)?;
    let expected_len = snapshot.0.len() as u64;
    let written = match store.write_all(&mut temp, &snapshot.0) {
        Ok(written) => written,
        Err(error) => {
            let _ = store.cleanup(temp);
            return Err(SaveError::Write(error));
        }
    };
    if written != expected_len {
        let _ = store.cleanup(temp);
        return Err(SaveError::PartialWrite {
            expected: expected_len,
            actual: written,
        });
    }
    if cancellation.cancelled() {
        let _ = store.cleanup(temp);
        return Ok(SaveOutcome::CancelledBeforeReplace);
    }
    let current = match store.identity() {
        Ok(identity) => identity,
        Err(error) => {
            let _ = store.cleanup(temp);
            return Err(SaveError::Recheck(error));
        }
    };
    if current != *expected {
        let _ = store.cleanup(temp);
        return Err(SaveError::Conflict);
    }
    store
        .atomic_replace(temp)
        .map_err(SaveError::AtomicReplace)?;
    Ok(SaveOutcome::Saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[derive(Default)]
    struct Cancel(Cell<usize>);
    impl Cancellation for Cancel {
        fn cancelled(&self) -> bool {
            let call = self.0.get();
            self.0.set(call + 1);
            false
        }
    }

    struct Fake {
        identities: Vec<RemoteIdentity>,
        written: u64,
        replace_error: bool,
        cleaned: usize,
        replaced: bool,
    }
    impl AtomicDocumentStore for Fake {
        type Temp = ();
        type Error = &'static str;
        fn identity(&mut self) -> Result<RemoteIdentity, Self::Error> {
            Ok(self.identities.remove(0))
        }
        fn create_sibling_temp(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        fn write_all(&mut self, (): &mut (), _: &[u8]) -> Result<u64, Self::Error> {
            Ok(self.written)
        }
        fn atomic_replace(&mut self, (): ()) -> Result<(), Self::Error> {
            if self.replace_error {
                Err("unsupported")
            } else {
                self.replaced = true;
                Ok(())
            }
        }
        fn cleanup(&mut self, (): ()) -> Result<(), Self::Error> {
            self.cleaned += 1;
            Ok(())
        }
    }
    fn identity(value: u8) -> RemoteIdentity {
        RemoteIdentity {
            revision: vec![value],
            byte_len: 3,
        }
    }
    fn fake(ids: Vec<RemoteIdentity>) -> Fake {
        Fake {
            identities: ids,
            written: 3,
            replace_error: false,
            cleaned: 0,
            replaced: false,
        }
    }

    #[test]
    fn unchanged_remote_is_atomically_replaced() {
        let expected = identity(1);
        let mut store = fake(vec![expected.clone(), expected.clone()]);
        assert_eq!(
            save_document(
                &mut store,
                &Cancel::default(),
                &expected,
                &EncodedSnapshot::new(vec![1, 2, 3]).unwrap()
            ),
            Ok(SaveOutcome::Saved)
        );
        assert!(store.replaced);
    }
    #[test]
    fn race_before_rename_is_conflict_and_cleans_temp() {
        let expected = identity(1);
        let mut store = fake(vec![expected.clone(), identity(2)]);
        assert_eq!(
            save_document(
                &mut store,
                &Cancel::default(),
                &expected,
                &EncodedSnapshot::new(vec![1, 2, 3]).unwrap()
            ),
            Err(SaveError::Conflict)
        );
        assert_eq!(store.cleaned, 1);
        assert!(!store.replaced);
    }
    #[test]
    fn partial_write_never_replaces() {
        let expected = identity(1);
        let mut store = fake(vec![expected.clone()]);
        store.written = 2;
        assert_eq!(
            save_document(
                &mut store,
                &Cancel::default(),
                &expected,
                &EncodedSnapshot::new(vec![1, 2, 3]).unwrap()
            ),
            Err(SaveError::PartialWrite {
                expected: 3,
                actual: 2
            })
        );
        assert_eq!(store.cleaned, 1);
    }
    #[test]
    fn unsupported_atomic_replace_is_a_stable_failure() {
        let expected = identity(1);
        let mut store = fake(vec![expected.clone(), expected.clone()]);
        store.replace_error = true;
        assert_eq!(
            save_document(
                &mut store,
                &Cancel::default(),
                &expected,
                &EncodedSnapshot::new(vec![1, 2, 3]).unwrap()
            ),
            Err(SaveError::AtomicReplace("unsupported"))
        );
        assert!(!store.replaced);
    }
}
