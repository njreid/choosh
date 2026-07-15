//! Deterministic, dependency-free capability fakes shared by headless tests.

use choosh_core::ports::{
    BoxFuture, Clock, GatewayHandle, HostEndpoint, HostKeyDecision, HostRpc, HostTransport,
    IdGenerator, LoopbackGateway, NotificationIntent, NotificationSink, PortError, PortResult,
    ProcessHandle, ProcessLauncher, ProcessSpec, SftpClient, StateStore,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

const FIXTURE_EXHAUSTED: PortError = PortError::new("fixture_exhausted", false);
const RECORDING_LIMIT: PortError = PortError::new("recording_limit", false);
const INPUT_LIMIT: PortError = PortError::new("input_limit", false);
const INVALID_RELATIVE_PATH: PortError = PortError::new("invalid_relative_path", false);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FakeBounds {
    pub max_calls: usize,
    pub max_input_bytes: usize,
}

impl FakeBounds {
    #[must_use]
    pub const fn new(max_calls: usize, max_input_bytes: usize) -> Self {
        Self {
            max_calls,
            max_input_bytes,
        }
    }
}

fn record<T>(calls: &Mutex<Vec<T>>, call: T, max_calls: usize) -> PortResult<()> {
    let mut calls = calls
        .lock()
        .map_err(|_| PortError::new("test_lock_poisoned", false))?;
    if calls.len() >= max_calls {
        return Err(RECORDING_LIMIT);
    }
    calls.push(call);
    Ok(())
}

fn take<T>(outcomes: &Mutex<VecDeque<PortResult<T>>>) -> PortResult<T> {
    outcomes
        .lock()
        .map_err(|_| PortError::new("test_lock_poisoned", false))?
        .pop_front()
        .unwrap_or(Err(FIXTURE_EXHAUSTED))
}

fn snapshot<T: Clone>(calls: &Mutex<Vec<T>>) -> PortResult<Vec<T>> {
    calls
        .lock()
        .map(|calls| calls.clone())
        .map_err(|_| PortError::new("test_lock_poisoned", false))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostTransportCall {
    Verify(HostEndpoint),
    Execute {
        executable: String,
        args: Vec<String>,
        stdin: Vec<u8>,
        output_limit: usize,
    },
}

#[derive(Debug)]
pub struct ScriptedHostTransport {
    bounds: FakeBounds,
    calls: Mutex<Vec<HostTransportCall>>,
    verifies: Mutex<VecDeque<PortResult<HostKeyDecision>>>,
    executions: Mutex<VecDeque<PortResult<Vec<u8>>>>,
}

impl ScriptedHostTransport {
    #[must_use]
    pub fn new(
        bounds: FakeBounds,
        verifies: impl IntoIterator<Item = PortResult<HostKeyDecision>>,
        executions: impl IntoIterator<Item = PortResult<Vec<u8>>>,
    ) -> Self {
        Self {
            bounds,
            calls: Mutex::default(),
            verifies: Mutex::new(verifies.into_iter().collect()),
            executions: Mutex::new(executions.into_iter().collect()),
        }
    }

    /// Returns a snapshot of recorded calls.
    ///
    /// # Errors
    ///
    /// Returns `test_lock_poisoned` if another test panicked while recording.
    pub fn calls(&self) -> PortResult<Vec<HostTransportCall>> {
        snapshot(&self.calls)
    }
}

impl HostTransport for ScriptedHostTransport {
    fn verify_host_key<'a>(
        &'a self,
        endpoint: &'a HostEndpoint,
    ) -> BoxFuture<'a, PortResult<HostKeyDecision>> {
        Box::pin(async move {
            if endpoint
                .hostname
                .len()
                .saturating_add(endpoint.username.len())
                > self.bounds.max_input_bytes
            {
                return Err(INPUT_LIMIT);
            }
            record(
                &self.calls,
                HostTransportCall::Verify(endpoint.clone()),
                self.bounds.max_calls,
            )?;
            take(&self.verifies)
        })
    }

    fn execute<'a>(
        &'a self,
        executable: &'a str,
        args: &'a [String],
        stdin: Vec<u8>,
        output_limit: usize,
    ) -> BoxFuture<'a, PortResult<Vec<u8>>> {
        Box::pin(async move {
            let input_size = executable
                .len()
                .saturating_add(stdin.len())
                .saturating_add(args.iter().map(String::len).sum::<usize>());
            if input_size > self.bounds.max_input_bytes {
                return Err(INPUT_LIMIT);
            }
            record(
                &self.calls,
                HostTransportCall::Execute {
                    executable: executable.to_owned(),
                    args: args.to_vec(),
                    stdin,
                    output_limit,
                },
                self.bounds.max_calls,
            )?;
            let output = take(&self.executions)?;
            if output.len() > output_limit {
                return Err(PortError::new("output_limit", false));
            }
            Ok(output)
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SftpCall {
    Read { path: String, limit: usize },
    Write { path: String, contents: Vec<u8> },
}

#[derive(Debug)]
pub struct ScriptedSftpClient {
    bounds: FakeBounds,
    calls: Mutex<Vec<SftpCall>>,
    reads: Mutex<VecDeque<PortResult<Vec<u8>>>>,
    writes: Mutex<VecDeque<PortResult<()>>>,
}

impl ScriptedSftpClient {
    #[must_use]
    pub fn new(
        bounds: FakeBounds,
        reads: impl IntoIterator<Item = PortResult<Vec<u8>>>,
        writes: impl IntoIterator<Item = PortResult<()>>,
    ) -> Self {
        Self {
            bounds,
            calls: Mutex::default(),
            reads: Mutex::new(reads.into_iter().collect()),
            writes: Mutex::new(writes.into_iter().collect()),
        }
    }
    /// Returns a snapshot of recorded calls.
    ///
    /// # Errors
    ///
    /// Returns `test_lock_poisoned` if another test panicked while recording.
    pub fn calls(&self) -> PortResult<Vec<SftpCall>> {
        snapshot(&self.calls)
    }
}

fn valid_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}

impl SftpClient for ScriptedSftpClient {
    fn read<'a>(&'a self, path: &'a str, limit: usize) -> BoxFuture<'a, PortResult<Vec<u8>>> {
        Box::pin(async move {
            if !valid_relative_path(path) {
                return Err(INVALID_RELATIVE_PATH);
            }
            if path.len() > self.bounds.max_input_bytes {
                return Err(INPUT_LIMIT);
            }
            record(
                &self.calls,
                SftpCall::Read {
                    path: path.to_owned(),
                    limit,
                },
                self.bounds.max_calls,
            )?;
            let value = take(&self.reads)?;
            if value.len() > limit {
                return Err(PortError::new("read_limit", false));
            }
            Ok(value)
        })
    }
    fn write_atomic<'a>(
        &'a self,
        path: &'a str,
        contents: Vec<u8>,
    ) -> BoxFuture<'a, PortResult<()>> {
        Box::pin(async move {
            if !valid_relative_path(path) {
                return Err(INVALID_RELATIVE_PATH);
            }
            if path.len().saturating_add(contents.len()) > self.bounds.max_input_bytes {
                return Err(INPUT_LIMIT);
            }
            record(
                &self.calls,
                SftpCall::Write {
                    path: path.to_owned(),
                    contents,
                },
                self.bounds.max_calls,
            )?;
            take(&self.writes)
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RpcCall {
    pub method: String,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub struct ScriptedHostRpc {
    bounds: FakeBounds,
    calls: Mutex<Vec<RpcCall>>,
    outcomes: Mutex<VecDeque<PortResult<Vec<u8>>>>,
}
impl ScriptedHostRpc {
    #[must_use]
    pub fn new(
        bounds: FakeBounds,
        outcomes: impl IntoIterator<Item = PortResult<Vec<u8>>>,
    ) -> Self {
        Self {
            bounds,
            calls: Mutex::default(),
            outcomes: Mutex::new(outcomes.into_iter().collect()),
        }
    }
    /// Returns a snapshot of recorded calls.
    ///
    /// # Errors
    ///
    /// Returns `test_lock_poisoned` if another test panicked while recording.
    pub fn calls(&self) -> PortResult<Vec<RpcCall>> {
        snapshot(&self.calls)
    }
}
impl HostRpc for ScriptedHostRpc {
    fn request<'a>(&'a self, method: &'a str, body: Vec<u8>) -> BoxFuture<'a, PortResult<Vec<u8>>> {
        Box::pin(async move {
            if method.len().saturating_add(body.len()) > self.bounds.max_input_bytes {
                return Err(INPUT_LIMIT);
            }
            record(
                &self.calls,
                RpcCall {
                    method: method.to_owned(),
                    body,
                },
                self.bounds.max_calls,
            )?;
            take(&self.outcomes)
        })
    }
}

#[derive(Debug)]
pub struct ScriptedNotificationSink {
    max_calls: usize,
    calls: Mutex<Vec<NotificationIntent>>,
    outcomes: Mutex<VecDeque<PortResult<()>>>,
}
impl ScriptedNotificationSink {
    #[must_use]
    pub fn new(max_calls: usize, outcomes: impl IntoIterator<Item = PortResult<()>>) -> Self {
        Self {
            max_calls,
            calls: Mutex::default(),
            outcomes: Mutex::new(outcomes.into_iter().collect()),
        }
    }
    /// Returns a snapshot of recorded calls.
    ///
    /// # Errors
    ///
    /// Returns `test_lock_poisoned` if another test panicked while recording.
    pub fn calls(&self) -> PortResult<Vec<NotificationIntent>> {
        snapshot(&self.calls)
    }
}
impl NotificationSink for ScriptedNotificationSink {
    fn apply(&self, intent: NotificationIntent) -> BoxFuture<'_, PortResult<()>> {
        Box::pin(async move {
            record(&self.calls, intent, self.max_calls)?;
            take(&self.outcomes)
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GatewayCall {
    Open {
        service_id: String,
        registered_port: u16,
    },
    Close(GatewayHandle),
}

#[derive(Debug)]
pub struct ScriptedLoopbackGateway {
    bounds: FakeBounds,
    calls: Mutex<Vec<GatewayCall>>,
    opens: Mutex<VecDeque<PortResult<GatewayHandle>>>,
    closes: Mutex<VecDeque<PortResult<()>>>,
}
impl ScriptedLoopbackGateway {
    #[must_use]
    pub fn new(
        bounds: FakeBounds,
        opens: impl IntoIterator<Item = PortResult<GatewayHandle>>,
        closes: impl IntoIterator<Item = PortResult<()>>,
    ) -> Self {
        Self {
            bounds,
            calls: Mutex::default(),
            opens: Mutex::new(opens.into_iter().collect()),
            closes: Mutex::new(closes.into_iter().collect()),
        }
    }
    /// Returns a snapshot of recorded calls.
    ///
    /// # Errors
    ///
    /// Returns `test_lock_poisoned` if another test panicked while recording.
    pub fn calls(&self) -> PortResult<Vec<GatewayCall>> {
        snapshot(&self.calls)
    }
}
impl LoopbackGateway for ScriptedLoopbackGateway {
    fn open<'a>(
        &'a self,
        service_id: &'a str,
        registered_port: u16,
    ) -> BoxFuture<'a, PortResult<GatewayHandle>> {
        Box::pin(async move {
            if service_id.len() > self.bounds.max_input_bytes {
                return Err(INPUT_LIMIT);
            }
            record(
                &self.calls,
                GatewayCall::Open {
                    service_id: service_id.to_owned(),
                    registered_port,
                },
                self.bounds.max_calls,
            )?;
            take(&self.opens)
        })
    }
    fn close<'a>(&'a self, handle: &'a GatewayHandle) -> BoxFuture<'a, PortResult<()>> {
        Box::pin(async move {
            if handle.id.len() > self.bounds.max_input_bytes {
                return Err(INPUT_LIMIT);
            }
            record(
                &self.calls,
                GatewayCall::Close(handle.clone()),
                self.bounds.max_calls,
            )?;
            take(&self.closes)
        })
    }
}

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
    use super::{
        FakeBounds, GatewayCall, HostTransportCall, ManualClock, ScriptedHostRpc,
        ScriptedHostTransport, ScriptedLoopbackGateway, ScriptedNotificationSink,
        ScriptedSftpClient, SequenceIdGenerator, SftpCall,
    };
    use choosh_core::ports::{
        Clock, GatewayHandle, HostEndpoint, HostKeyDecision, HostRpc, HostTransport, IdGenerator,
        LoopbackGateway, NotificationIntent, NotificationSink, PortError, SftpClient,
    };
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    fn block_on<T>(future: impl Future<Output = T>) -> T {
        let mut future = Box::pin(future);
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("testkit fake unexpectedly returned a pending future"),
        }
    }

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

    #[test]
    fn host_transport_records_argv_and_enforces_caller_output_limit() {
        let transport = ScriptedHostTransport::new(
            FakeBounds::new(3, 64),
            [Ok(HostKeyDecision::Trusted)],
            [Ok(vec![1, 2, 3])],
        );
        let endpoint = HostEndpoint {
            hostname: "host.test".into(),
            port: 22,
            username: "tester".into(),
        };
        assert_eq!(
            block_on(transport.verify_host_key(&endpoint)),
            Ok(HostKeyDecision::Trusted)
        );
        let args = vec!["arg with spaces".into()];
        assert_eq!(
            block_on(transport.execute("tool", &args, vec![9], 2))
                .unwrap_err()
                .code,
            "output_limit"
        );
        assert_eq!(transport.calls().unwrap().len(), 2);
        assert!(
            matches!(&transport.calls().unwrap()[1], HostTransportCall::Execute { executable, .. } if executable == "tool")
        );
    }

    #[test]
    fn sftp_rejects_escape_before_consuming_script_or_recording() {
        let sftp = ScriptedSftpClient::new(FakeBounds::new(2, 32), [Ok(vec![7])], [Ok(())]);
        assert_eq!(
            block_on(sftp.read("../secret", 8)).unwrap_err().code,
            "invalid_relative_path"
        );
        assert!(sftp.calls().unwrap().is_empty());
        assert_eq!(block_on(sftp.read("dir/file", 8)), Ok(vec![7]));
        assert_eq!(
            sftp.calls().unwrap(),
            vec![SftpCall::Read {
                path: "dir/file".into(),
                limit: 8
            }]
        );
    }

    #[test]
    fn rpc_faults_are_scripted_and_recording_is_bounded() {
        let rpc = ScriptedHostRpc::new(
            FakeBounds::new(1, 16),
            [Err(PortError::new("disconnected", true)), Ok(vec![1])],
        );
        assert_eq!(
            block_on(rpc.request("hello", vec![])).unwrap_err(),
            PortError::new("disconnected", true)
        );
        assert_eq!(
            block_on(rpc.request("again", vec![])).unwrap_err().code,
            "recording_limit"
        );
        assert_eq!(rpc.calls().unwrap()[0].method, "hello");
    }

    #[test]
    fn notification_failures_do_not_hide_recorded_intent() {
        let sink = ScriptedNotificationSink::new(1, [Err(PortError::new("unavailable", true))]);
        let intent = NotificationIntent::Clear {
            stable_id: "item-1".into(),
        };
        assert_eq!(
            block_on(sink.apply(intent.clone())).unwrap_err().code,
            "unavailable"
        );
        assert_eq!(sink.calls().unwrap(), vec![intent]);
    }

    #[test]
    fn loopback_gateway_records_only_explicit_registered_service() {
        let handle = GatewayHandle {
            id: "gateway-1".into(),
            loopback_port: 40123,
        };
        let gateway =
            ScriptedLoopbackGateway::new(FakeBounds::new(2, 32), [Ok(handle.clone())], [Ok(())]);
        assert_eq!(
            block_on(gateway.open("service-1", 8080)),
            Ok(handle.clone())
        );
        assert_eq!(block_on(gateway.close(&handle)), Ok(()));
        assert_eq!(
            gateway.calls().unwrap(),
            vec![
                GatewayCall::Open {
                    service_id: "service-1".into(),
                    registered_port: 8080
                },
                GatewayCall::Close(handle),
            ]
        );
    }

    #[test]
    fn exhausted_scripts_fail_closed() {
        let gateway = ScriptedLoopbackGateway::new(
            FakeBounds::new(1, 8),
            std::iter::empty(),
            std::iter::empty(),
        );
        assert_eq!(
            block_on(gateway.open("service", 80)).unwrap_err().code,
            "fixture_exhausted"
        );
    }
}
