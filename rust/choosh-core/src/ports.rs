use std::future::Future;
use std::pin::Pin;

/// Object-safe future returned by asynchronous capability interfaces.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Stable error classification crossing a capability boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortError {
    pub code: &'static str,
    pub retryable: bool,
}

impl PortError {
    #[must_use]
    pub const fn new(code: &'static str, retryable: bool) -> Self {
        Self { code, retryable }
    }
}

pub type PortResult<T> = Result<T, PortError>;

/// Monotonic time only. Domain logic must not read the wall clock implicitly.
pub trait Clock: Send + Sync {
    fn now_millis(&self) -> u64;
}

/// Stable opaque identifiers supplied explicitly for reproducible tests.
pub trait IdGenerator: Send + Sync {
    /// Returns the next opaque identifier.
    ///
    /// # Errors
    ///
    /// Returns a stable capability error when an identifier cannot be produced.
    fn next_id(&self) -> PortResult<String>;
}

/// Atomic durable byte records. Format ownership remains with the caller.
pub trait StateStore: Send + Sync {
    fn load<'a>(&'a self, key: &'a str) -> BoxFuture<'a, PortResult<Option<Vec<u8>>>>;
    fn replace<'a>(&'a self, key: &'a str, value: Vec<u8>) -> BoxFuture<'a, PortResult<()>>;
    fn remove<'a>(&'a self, key: &'a str) -> BoxFuture<'a, PortResult<()>>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessSpec {
    pub executable: String,
    pub args: Vec<String>,
    pub working_directory: Option<String>,
    pub environment: Vec<(String, String)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessHandle {
    pub id: String,
}

/// Launches an executable with separately encoded arguments, never a shell string.
pub trait ProcessLauncher: Send + Sync {
    fn launch(&self, spec: ProcessSpec) -> BoxFuture<'_, PortResult<ProcessHandle>>;
    fn terminate<'a>(&'a self, handle: &'a ProcessHandle) -> BoxFuture<'a, PortResult<()>>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostKeyDecision {
    Trusted,
    Unknown { sha256_fingerprint: String },
    Mismatch { sha256_fingerprint: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostEndpoint {
    pub hostname: String,
    pub port: u16,
    pub username: String,
}

/// Verified remote transport. A connection is unavailable until the host-key
/// decision is `Trusted`; adapters must not expose a permissive bypass.
pub trait HostTransport: Send + Sync {
    fn verify_host_key<'a>(
        &'a self,
        endpoint: &'a HostEndpoint,
    ) -> BoxFuture<'a, PortResult<HostKeyDecision>>;

    fn execute<'a>(
        &'a self,
        executable: &'a str,
        args: &'a [String],
        stdin: Vec<u8>,
        output_limit: usize,
    ) -> BoxFuture<'a, PortResult<Vec<u8>>>;
}

/// Root-relative SFTP operations. Implementations must enforce the registered
/// root and must not accept absolute or escaping paths.
pub trait SftpClient: Send + Sync {
    fn read<'a>(
        &'a self,
        relative_path: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, PortResult<Vec<u8>>>;
    fn write_atomic<'a>(
        &'a self,
        relative_path: &'a str,
        contents: Vec<u8>,
    ) -> BoxFuture<'a, PortResult<()>>;
}

/// Typed request boundary to `chooshd`; framing belongs to an outer adapter.
pub trait HostRpc: Send + Sync {
    fn request<'a>(&'a self, method: &'a str, body: Vec<u8>) -> BoxFuture<'a, PortResult<Vec<u8>>>;
}

/// Zellij owns PTYs and processes; this capability controls only explicit targets.
pub trait ZellijControl: Send + Sync {
    fn create_tab<'a>(
        &'a self,
        session: &'a str,
        tab: &'a str,
        process: ProcessSpec,
    ) -> BoxFuture<'a, PortResult<String>>;
    fn stop_tab<'a>(&'a self, target_id: &'a str) -> BoxFuture<'a, PortResult<()>>;
}

/// Bounded, machine-readable Git data supplied by the host.
pub trait GitProvider: Send + Sync {
    fn status<'a>(
        &'a self,
        workspace_id: &'a str,
        byte_limit: usize,
    ) -> BoxFuture<'a, PortResult<Vec<u8>>>;
    fn version<'a>(
        &'a self,
        workspace_id: &'a str,
        relative_path: &'a str,
        version: &'a str,
        byte_limit: usize,
    ) -> BoxFuture<'a, PortResult<Vec<u8>>>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NotificationIntent {
    Upsert {
        stable_id: String,
        coarse_reason: String,
    },
    Clear {
        stable_id: String,
    },
}

/// Projection sink; domain state remains authoritative when delivery is unavailable.
pub trait NotificationSink: Send + Sync {
    fn apply(&self, intent: NotificationIntent) -> BoxFuture<'_, PortResult<()>>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GatewayHandle {
    pub id: String,
    pub loopback_port: u16,
}

/// Creates authenticated loopback gateways for an already registered service.
pub trait LoopbackGateway: Send + Sync {
    fn open<'a>(
        &'a self,
        service_id: &'a str,
        registered_port: u16,
    ) -> BoxFuture<'a, PortResult<GatewayHandle>>;
    fn close<'a>(&'a self, handle: &'a GatewayHandle) -> BoxFuture<'a, PortResult<()>>;
}
