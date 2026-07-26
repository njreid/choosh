//! Authoritative, headlessly testable host-daemon state.

pub mod adapters;
pub mod annotation_store;
pub mod blob;
pub mod checkpoint;
pub mod daemon;
pub mod diagnostics;
pub mod git;
pub mod git_status;
pub mod health;
pub mod lifecycle;
pub mod project_fs;
pub mod socket;
pub mod state;
pub mod storage;
pub mod upgrade;
pub mod zellij;

pub use state::{CoordinatorError, CoordinatorLimits, DaemonCoordinator, WorkspaceStateSnapshot};
