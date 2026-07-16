//! Generation-safe bounded bridge request lifecycle.

use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RequestId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestHandle {
    pub generation: u64,
    pub request_id: RequestId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RequestResult<T, E> {
    Success(T),
    Error(E),
    Cancelled,
}

pub trait CallbackSink<T, E> {
    type Error;

    /// Delivers exactly one terminal request result to the platform boundary.
    ///
    /// # Errors
    ///
    /// Returns the sink-specific delivery failure.
    fn deliver(
        &mut self,
        handle: RequestHandle,
        result: RequestResult<T, E>,
    ) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelCommand {
    Propagate(RequestHandle),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeCheckpoint {
    pub generation: u64,
    pub next_request_id: u64,
    pub abandoned: Vec<RequestId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BridgeError<SinkError> {
    InvalidLimits,
    InvalidCheckpoint,
    Capacity,
    IdExhausted,
    UnknownRequest,
    StaleGeneration,
    AlreadyCancelled,
    Sink(SinkError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestState {
    Pending,
    Cancelling,
}

#[derive(Debug)]
pub struct BridgeLifecycle<S> {
    generation: u64,
    next_request_id: u64,
    capacity: usize,
    requests: BTreeMap<RequestId, RequestState>,
    sink: S,
}

impl<S> BridgeLifecycle<S> {
    /// Creates a bridge registry with an explicit callback capability.
    ///
    /// # Errors
    ///
    /// Rejects zero generation, capacity, or first request ID.
    pub fn new(
        generation: u64,
        first_request_id: u64,
        capacity: usize,
        sink: S,
    ) -> Result<Self, BridgeError<std::convert::Infallible>> {
        if generation == 0 || first_request_id == 0 || capacity == 0 {
            return Err(BridgeError::InvalidLimits);
        }
        Ok(Self {
            generation,
            next_request_id: first_request_id,
            capacity,
            requests: BTreeMap::new(),
            sink,
        })
    }

    /// Restores a new generation after process recreation. Old in-flight IDs are
    /// retained only as bounded abandonment evidence and are never resumed.
    ///
    /// # Errors
    ///
    /// Rejects invalid/stale generations, zero IDs, or oversized checkpoints.
    pub fn restore(
        checkpoint: &BridgeCheckpoint,
        new_generation: u64,
        capacity: usize,
        sink: S,
    ) -> Result<Self, BridgeError<std::convert::Infallible>> {
        if checkpoint.generation == 0
            || new_generation <= checkpoint.generation
            || checkpoint.next_request_id == 0
            || capacity == 0
            || checkpoint.abandoned.len() > capacity
        {
            return Err(BridgeError::InvalidCheckpoint);
        }
        Ok(Self {
            generation: new_generation,
            next_request_id: checkpoint.next_request_id,
            capacity,
            requests: BTreeMap::new(),
            sink,
        })
    }

    /// Allocates one bounded in-flight request ID.
    ///
    /// # Errors
    ///
    /// Returns capacity or unsigned-ID exhaustion instead of wrapping.
    pub fn begin(&mut self) -> Result<RequestHandle, BridgeError<std::convert::Infallible>> {
        if self.requests.len() == self.capacity {
            return Err(BridgeError::Capacity);
        }
        let request_id = RequestId(self.next_request_id);
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or(BridgeError::IdExhausted)?;
        self.requests.insert(request_id, RequestState::Pending);
        Ok(RequestHandle {
            generation: self.generation,
            request_id,
        })
    }

    /// Marks a request cancelling and returns the command an outer transport must
    /// propagate. Repeated cancel never emits a second command.
    ///
    /// # Errors
    ///
    /// Rejects stale, unknown, or already-cancelling requests.
    pub fn cancel(
        &mut self,
        handle: RequestHandle,
    ) -> Result<CancelCommand, BridgeError<std::convert::Infallible>> {
        self.validate_generation(handle)?;
        let state = self
            .requests
            .get_mut(&handle.request_id)
            .ok_or(BridgeError::UnknownRequest)?;
        if *state == RequestState::Cancelling {
            return Err(BridgeError::AlreadyCancelled);
        }
        *state = RequestState::Cancelling;
        Ok(CancelCommand::Propagate(handle))
    }

    /// Creates a bounded process-recreation checkpoint and abandons all requests.
    #[must_use]
    pub fn checkpoint(self) -> BridgeCheckpoint {
        BridgeCheckpoint {
            generation: self.generation,
            next_request_id: self.next_request_id,
            abandoned: self.requests.into_keys().collect(),
        }
    }

    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.requests.len()
    }

    fn validate_generation<E>(&self, handle: RequestHandle) -> Result<(), BridgeError<E>> {
        if handle.generation == self.generation {
            Ok(())
        } else {
            Err(BridgeError::StaleGeneration)
        }
    }
}

impl<S> BridgeLifecycle<S> {
    /// Delivers a terminal result at most once, removing the ID before invoking
    /// external callback code so reentrancy cannot redeliver it.
    ///
    /// # Errors
    ///
    /// Rejects stale/unknown handles or wraps a callback sink failure. Even when
    /// the sink fails, the request remains terminal and cannot be delivered again.
    pub fn complete<T, E>(
        &mut self,
        handle: RequestHandle,
        result: RequestResult<T, E>,
    ) -> Result<(), BridgeError<S::Error>>
    where
        S: CallbackSink<T, E>,
    {
        self.validate_generation(handle)?;
        self.requests
            .remove(&handle.request_id)
            .ok_or(BridgeError::UnknownRequest)?;
        self.sink.deliver(handle, result).map_err(BridgeError::Sink)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default)]
    struct RecordingSink {
        delivered: Vec<(RequestHandle, RequestResult<String, &'static str>)>,
        fail: bool,
    }

    impl CallbackSink<String, &'static str> for RecordingSink {
        type Error = &'static str;

        fn deliver(
            &mut self,
            handle: RequestHandle,
            result: RequestResult<String, &'static str>,
        ) -> Result<(), Self::Error> {
            self.delivered.push((handle, result));
            if self.fail {
                Err("sink_failed")
            } else {
                Ok(())
            }
        }
    }

    fn bridge(capacity: usize) -> BridgeLifecycle<RecordingSink> {
        BridgeLifecycle::new(1, 10, capacity, RecordingSink::default()).unwrap()
    }

    #[test]
    fn terminal_callbacks_deliver_at_most_once_for_every_result_kind() {
        let mut bridge = bridge(3);
        for result in [
            RequestResult::Success("ok".into()),
            RequestResult::Error("remote"),
            RequestResult::Cancelled,
        ] {
            let handle = bridge.begin().unwrap();
            bridge.complete(handle, result).unwrap();
            assert_eq!(
                bridge.complete(handle, RequestResult::Cancelled),
                Err(BridgeError::UnknownRequest)
            );
        }
        assert_eq!(bridge.sink.delivered.len(), 3);
    }

    #[test]
    fn cancellation_propagates_once_before_terminal_callback() {
        let mut bridge = bridge(1);
        let handle = bridge.begin().unwrap();
        assert_eq!(
            bridge.cancel(handle).unwrap(),
            CancelCommand::Propagate(handle)
        );
        assert_eq!(bridge.cancel(handle), Err(BridgeError::AlreadyCancelled));
        bridge.complete(handle, RequestResult::Cancelled).unwrap();
        assert_eq!(bridge.in_flight(), 0);
    }

    #[test]
    fn recreation_rejects_old_callbacks_and_does_not_resume_requests() {
        let mut old = bridge(2);
        let old_handle = old.begin().unwrap();
        let checkpoint = old.checkpoint();
        assert_eq!(checkpoint.abandoned, [old_handle.request_id]);
        let mut restored =
            BridgeLifecycle::restore(&checkpoint, 2, 2, RecordingSink::default()).unwrap();
        assert_eq!(
            restored.complete(old_handle, RequestResult::Success("stale".into())),
            Err(BridgeError::StaleGeneration)
        );
        let fresh = restored.begin().unwrap();
        assert_eq!(fresh.generation, 2);
        assert_eq!(fresh.request_id, RequestId(11));
    }

    #[test]
    fn capacity_and_id_exhaustion_are_fail_closed() {
        let mut limited = bridge(1);
        limited.begin().unwrap();
        assert_eq!(limited.begin(), Err(BridgeError::Capacity));

        let mut exhausted = BridgeLifecycle::new(1, u64::MAX, 1, RecordingSink::default()).unwrap();
        assert_eq!(exhausted.begin(), Err(BridgeError::IdExhausted));
        assert_eq!(exhausted.in_flight(), 0);
    }

    #[test]
    fn sink_failure_still_consumes_request() {
        let mut bridge = BridgeLifecycle::new(
            1,
            1,
            1,
            RecordingSink {
                delivered: vec![],
                fail: true,
            },
        )
        .unwrap();
        let handle = bridge.begin().unwrap();
        assert_eq!(
            bridge.complete(handle, RequestResult::Success("value".into())),
            Err(BridgeError::Sink("sink_failed"))
        );
        assert_eq!(
            bridge.complete(handle, RequestResult::Cancelled),
            Err(BridgeError::UnknownRequest)
        );
    }
}
