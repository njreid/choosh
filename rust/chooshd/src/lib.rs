//! Authoritative, headlessly testable host-daemon state.

pub mod socket;
pub mod state;

pub use state::{CoordinatorError, CoordinatorLimits, DaemonCoordinator, WorkspaceStateSnapshot};
