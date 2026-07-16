//! Platform-neutral Choosh domain boundaries.
//!
//! Concrete Android, SSH, filesystem, and process implementations belong in
//! outer adapter crates. Domain code receives these capabilities explicitly.

pub mod actor;
pub mod annotation;
pub mod annotation_export;
pub mod asset;
pub mod backoff;
pub mod bridge;
pub mod connection;
pub mod diff;
pub mod diff_navigation;
pub mod document;
pub mod document_format;
pub mod document_save;
pub mod event_spool;
pub mod explorer;
pub mod gateway;
pub mod gesture;
pub mod http_gateway;
pub mod item;
pub mod markdown;
pub mod notification_activation;
pub mod path;
pub mod pins;
pub mod ports;
pub mod readiness;
pub mod release_evidence;
pub mod release_update;
pub mod renderer_binding;
pub mod runtime;
pub mod service;
pub mod terminal;
pub mod text;
pub mod vt;
pub mod waiting_notification;
pub mod workspace;
