//! Loopback-only SSH `direct-tcpip` channel admission.
//!
//! A direct TCP/IP channel asks the SSH server to originate a new connection.
//! This adapter deliberately admits only its own loopback address and a caller
//! selected non-zero port.  It neither accepts a hostname nor exposes the raw
//! channel stream, so it cannot become a general-purpose proxy.  A later
//! versioned protocol capability may consume the opaque channel.

use std::num::NonZeroU16;
use std::sync::{Arc, Mutex};

use crate::VerifiedConnection;

const LOOPBACK_V4: &str = "127.0.0.1";
const MAX_OPEN_CHANNELS: usize = 8;

/// A fixed loopback destination for one SSH forwarding channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoopbackTcpTarget {
    port: NonZeroU16,
}

impl LoopbackTcpTarget {
    /// Creates a target for the SSH server's IPv4 loopback interface.
    #[must_use]
    pub const fn new(port: NonZeroU16) -> Self {
        Self { port }
    }

    #[must_use]
    pub const fn port(self) -> NonZeroU16 {
        self.port
    }
}

/// Explicit channel-count bound for one forwarding capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectTcpipLimits {
    max_open_channels: usize,
}

impl DirectTcpipLimits {
    /// Validates the number of concurrently open forwarding channels.
    ///
    /// # Errors
    ///
    /// Returns [`DirectTcpipError::InvalidLimits`] for zero or an adapter-wide
    /// ceiling violation.
    pub const fn new(max_open_channels: usize) -> Result<Self, DirectTcpipError> {
        if max_open_channels == 0 || max_open_channels > MAX_OPEN_CHANNELS {
            return Err(DirectTcpipError::InvalidLimits);
        }
        Ok(Self { max_open_channels })
    }

    /// Conservative default: a single host-local bridge at a time.
    #[must_use]
    pub const fn one_at_a_time() -> Self {
        Self {
            max_open_channels: 1,
        }
    }
}

/// Stable failures from loopback forwarding admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectTcpipError {
    InvalidLimits,
    ChannelLimitReached,
    TransportFailed,
}

impl DirectTcpipError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "invalid_limits",
            Self::ChannelLimitReached => "channel_limit_reached",
            Self::TransportFailed => "transport_error",
        }
    }
}

/// Capability to open a bounded number of channels to SSH-server loopback.
///
/// It can only be constructed from a [`VerifiedConnection`], which means
/// exact host-key verification and public-key authentication have completed.
pub struct LoopbackForwarding<'connection> {
    connection: &'connection VerifiedConnection,
    budget: Arc<ForwardingBudget>,
}

impl VerifiedConnection {
    /// Restricts future `direct-tcpip` channels to loopback destinations.
    #[must_use]
    pub fn loopback_forwarding(&self, limits: DirectTcpipLimits) -> LoopbackForwarding<'_> {
        LoopbackForwarding {
            connection: self,
            budget: Arc::new(ForwardingBudget::new(limits)),
        }
    }
}

impl LoopbackForwarding<'_> {
    /// Opens one opaque SSH channel to `127.0.0.1:target.port()`.
    ///
    /// The SSH protocol originator fields are also fixed to loopback and zero;
    /// no caller-provided network identity is sent to the host. The returned
    /// value only supports closing the channel, pending a separately reviewed
    /// bounded protocol layered over it.
    ///
    /// # Errors
    ///
    /// Returns [`DirectTcpipError::ChannelLimitReached`] before opening a
    /// channel if this capability has no available lease, or
    /// [`DirectTcpipError::TransportFailed`] when the SSH server rejects or
    /// loses the loopback channel.
    pub async fn open(
        &self,
        target: LoopbackTcpTarget,
    ) -> Result<LoopbackTcpipChannel, DirectTcpipError> {
        let lease = self.budget.acquire()?;
        let channel = self
            .connection
            .handle
            .channel_open_direct_tcpip(LOOPBACK_V4, u32::from(target.port().get()), LOOPBACK_V4, 0)
            .await
            .map_err(|_| DirectTcpipError::TransportFailed)?;
        Ok(LoopbackTcpipChannel {
            channel,
            _lease: lease,
        })
    }
}

/// An opaque, leased loopback forwarding channel.
pub struct LoopbackTcpipChannel {
    channel: russh::Channel<russh::client::Msg>,
    _lease: ForwardingLease,
}

impl LoopbackTcpipChannel {
    /// Closes the channel and releases its local concurrency lease.
    ///
    /// # Errors
    ///
    /// Returns [`DirectTcpipError::TransportFailed`] if the SSH close request
    /// cannot be sent. Dropping this value also releases the local lease.
    pub async fn close(self) -> Result<(), DirectTcpipError> {
        self.channel
            .close()
            .await
            .map_err(|_| DirectTcpipError::TransportFailed)
    }
}

struct ForwardingBudget {
    limit: usize,
    open: Mutex<usize>,
}

impl ForwardingBudget {
    const fn new(limits: DirectTcpipLimits) -> Self {
        Self {
            limit: limits.max_open_channels,
            open: Mutex::new(0),
        }
    }

    fn acquire(self: &Arc<Self>) -> Result<ForwardingLease, DirectTcpipError> {
        let mut open = self
            .open
            .lock()
            .expect("forwarding budget mutex is not poisoned");
        if *open == self.limit {
            return Err(DirectTcpipError::ChannelLimitReached);
        }
        *open += 1;
        Ok(ForwardingLease {
            budget: Arc::clone(self),
        })
    }
}

struct ForwardingLease {
    budget: Arc<ForwardingBudget>,
}

impl Drop for ForwardingLease {
    fn drop(&mut self) {
        let mut open = self
            .budget
            .open
            .lock()
            .expect("forwarding budget mutex is not poisoned");
        *open = open
            .checked_sub(1)
            .expect("forwarding lease count is positive");
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU16;
    use std::sync::Arc;

    use super::{
        DirectTcpipError, DirectTcpipLimits, ForwardingBudget, LOOPBACK_V4, LoopbackTcpTarget,
    };

    #[test]
    fn target_has_no_hostname_or_non_loopback_constructor() {
        let target = LoopbackTcpTarget::new(NonZeroU16::new(22).unwrap());
        assert_eq!(target.port().get(), 22);
        assert_eq!(LOOPBACK_V4, "127.0.0.1");
    }

    #[test]
    fn limits_are_small_positive_and_stable() {
        assert_eq!(
            DirectTcpipLimits::new(0),
            Err(DirectTcpipError::InvalidLimits)
        );
        assert_eq!(
            DirectTcpipLimits::new(9),
            Err(DirectTcpipError::InvalidLimits)
        );
        assert_eq!(DirectTcpipLimits::new(8).unwrap().max_open_channels, 8);
    }

    #[test]
    fn deterministic_budget_releases_on_drop_and_rejects_excess() {
        let budget = Arc::new(ForwardingBudget::new(DirectTcpipLimits::one_at_a_time()));
        let first = budget.acquire().unwrap();
        assert!(matches!(
            budget.acquire(),
            Err(DirectTcpipError::ChannelLimitReached)
        ));
        drop(first);
        assert!(budget.acquire().is_ok());
    }
}
