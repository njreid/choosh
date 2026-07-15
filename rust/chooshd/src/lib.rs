//! Authoritative, headlessly testable host-daemon state.

pub mod git;
pub mod socket;
pub mod state;
pub mod storage;
pub mod zellij;

pub use state::{CoordinatorError, CoordinatorLimits, DaemonCoordinator, WorkspaceStateSnapshot};
