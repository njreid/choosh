//! Platform-neutral Choosh domain boundaries.
//!
//! Concrete Android, SSH, filesystem, and process implementations belong in
//! outer adapter crates. Domain code receives these capabilities explicitly.

pub mod actor;
pub mod backoff;
pub mod connection;
pub mod diff;
pub mod event_spool;
pub mod gateway;
pub mod item;
pub mod path;
pub mod pins;
pub mod ports;
pub mod runtime;
pub mod service;
pub mod terminal;
pub mod text;
pub mod workspace;
