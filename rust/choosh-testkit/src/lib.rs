//! Deterministic, dependency-free capability fakes shared by headless tests.

use choosh_core::ports::{
    BoxFuture, Clock, IdGenerator, PortError, PortResult, ProcessHandle, ProcessLauncher,
    ProcessSpec, StateStore,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug)]
pub struct ManualClock {
    millis: AtomicU64,
}

impl ManualClock {
    #[must_use]
    pub const fn new(millis: u64) -> Self {
        Self {
            millis: AtomicU64::new(millis),
        }
    }

    /// Advances monotonic time and returns the new value.
    ///
    /// # Errors
    ///
    /// Returns `clock_overflow` when the requested advance exceeds `u64`.
    pub fn advance(&self, millis: u64) -> PortResult<u64> {
        self.millis
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                current.checked_add(millis)
            })
            .map(|previous| previous + millis)
            .map_err(|_| PortError::new("clock_overflow", false))
    }
}

impl Clock for ManualClock {
    fn now_millis(&self) -> u64 {
        self.millis.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
pub struct SequenceIdGenerator {
    values: Mutex<VecDeque<String>>,
}

impl SequenceIdGenerator {
    #[must_use]
    pub fn new(values: impl IntoIterator<Item = String>) -> Self {
        Self {
            values: Mutex::new(values.into_iter().collect()),
        }
    }
}

impl IdGenerator for SequenceIdGenerator {
    fn next_id(&self) -> PortResult<String> {
        self.values
            .lock()
            .map_err(|_| PortError::new("test_lock_poisoned", false))?
            .pop_front()
            .ok_or_else(|| PortError::new("id_fixture_exhausted", false))
    }
}

#[derive(Debug, Default)]
pub struct MemoryStateStore {
    records: Mutex<HashMap<String, Vec<u8>>>,
}

impl StateStore for MemoryStateStore {
    fn load<'a>(&'a self, key: &'a str) -> BoxFuture<'a, PortResult<Option<Vec<u8>>>> {
        Box::pin(async move {
            Ok(self
                .records
                .lock()
                .map_err(|_| PortError::new("test_lock_poisoned", false))?
                .get(key)
                .cloned())
        })
    }

    fn replace<'a>(&'a self, key: &'a str, value: Vec<u8>) -> BoxFuture<'a, PortResult<()>> {
        Box::pin(async move {
            self.records
                .lock()
                .map_err(|_| PortError::new("test_lock_poisoned", false))?
                .insert(key.to_owned(), value);
            Ok(())
        })
    }

    fn remove<'a>(&'a self, key: &'a str) -> BoxFuture<'a, PortResult<()>> {
        Box::pin(async move {
            self.records
                .lock()
                .map_err(|_| PortError::new("test_lock_poisoned", false))?
                .remove(key);
            Ok(())
        })
    }
}

#[derive(Debug, Default)]
pub struct RecordingProcessLauncher {
    launched: Mutex<Vec<ProcessSpec>>,
    terminated: Mutex<Vec<ProcessHandle>>,
}

impl RecordingProcessLauncher {
    /// Returns a snapshot of recorded launches.
    ///
    /// # Errors
    ///
    /// Returns `test_lock_poisoned` if another test panicked while holding the lock.
    pub fn launched(&self) -> PortResult<Vec<ProcessSpec>> {
        self.launched
            .lock()
            .map(|values| values.clone())
            .map_err(|_| PortError::new("test_lock_poisoned", false))
    }

    /// Returns a snapshot of recorded terminations.
    ///
    /// # Errors
    ///
    /// Returns `test_lock_poisoned` if another test panicked while holding the lock.
    pub fn terminated(&self) -> PortResult<Vec<ProcessHandle>> {
        self.terminated
            .lock()
            .map(|values| values.clone())
            .map_err(|_| PortError::new("test_lock_poisoned", false))
    }
}

impl ProcessLauncher for RecordingProcessLauncher {
    fn launch(&self, spec: ProcessSpec) -> BoxFuture<'_, PortResult<ProcessHandle>> {
        Box::pin(async move {
            let mut launched = self
                .launched
                .lock()
                .map_err(|_| PortError::new("test_lock_poisoned", false))?;
            launched.push(spec);
            Ok(ProcessHandle {
                id: format!("process-{}", launched.len()),
            })
        })
    }

    fn terminate<'a>(&'a self, handle: &'a ProcessHandle) -> BoxFuture<'a, PortResult<()>> {
        Box::pin(async move {
            self.terminated
                .lock()
                .map_err(|_| PortError::new("test_lock_poisoned", false))?
                .push(handle.clone());
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{ManualClock, SequenceIdGenerator};
    use choosh_core::ports::{Clock, IdGenerator};

    #[test]
    fn manual_clock_is_controlled_without_sleeping() {
        let clock = ManualClock::new(100);
        assert_eq!(clock.now_millis(), 100);
        assert_eq!(clock.advance(25), Ok(125));
        assert_eq!(clock.now_millis(), 125);
    }

    #[test]
    fn sequence_ids_are_reproducible_and_fail_when_exhausted() {
        let ids = SequenceIdGenerator::new(["a".to_owned(), "b".to_owned()]);
        assert_eq!(ids.next_id().as_deref(), Ok("a"));
        assert_eq!(ids.next_id().as_deref(), Ok("b"));
        assert_eq!(ids.next_id().unwrap_err().code, "id_fixture_exhausted");
    }
}
