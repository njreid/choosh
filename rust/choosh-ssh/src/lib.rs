//! Outer SSH adapter boundary.
//!
//! This crate owns the admitted Russh graph and must not leak Russh types into
//! `choosh-core`. The concrete transport is added only with the generated-key
//! admission harness: exact host-key verification must complete before this
//! crate attempts authentication or opens a channel.

mod host_key;

pub use host_key::{ExactHostKeyHandler, presented_fingerprint};

/// Exact Russh release admitted by ADR 0007.
pub const RUSSH_VERSION: &str = "0.62.2";

/// Exact SFTP subsystem release admitted alongside Russh.
pub const RUSSH_SFTP_VERSION: &str = "2.3.0";

/// The dependency graph deliberately disables Russh defaults and uses `ring`.
pub const CRYPTO_BACKEND: &str = "ring";

#[cfg(test)]
mod tests {
    use super::{CRYPTO_BACKEND, RUSSH_SFTP_VERSION, RUSSH_VERSION};

    #[test]
    fn admission_metadata_is_pinned_and_does_not_enable_defaults() {
        assert_eq!(RUSSH_VERSION, "0.62.2");
        assert_eq!(RUSSH_SFTP_VERSION, "2.3.0");
        assert_eq!(CRYPTO_BACKEND, "ring");
    }
}
