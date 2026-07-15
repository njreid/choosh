//! Platform-neutral Choosh domain boundaries.
//!
//! Concrete Android, SSH, filesystem, and process implementations belong in
//! outer adapter crates. Domain code receives these capabilities explicitly.

pub mod actor;
pub mod backoff;
pub mod connection;
pub mod event_spool;
pub mod item;
pub mod path;
pub mod ports;
pub mod runtime;
pub mod text;
pub mod workspace;
