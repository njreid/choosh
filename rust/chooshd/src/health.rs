//! Deterministic daemon health and compatibility negotiation.

use std::collections::BTreeSet;
pub const HEALTH_VERSION: u16 = 1;
const MAX_FIELD: usize = 128;
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Capability {
    AgentEvents,
    GitMetadata,
    Services,
    Terminal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HealthLimits {
    pub max_frame_bytes: usize,
    pub max_items: usize,
}
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DegradedReason {
    StorageReadOnly,
    EventBacklog,
    ProcessControlUnavailable,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HealthSnapshot {
    pub version: u16,
    pub host_build: String,
    pub protocol_version: u16,
    pub schema_version: u16,
    pub capabilities: BTreeSet<Capability>,
    pub limits: HealthLimits,
    pub degraded: BTreeSet<DegradedReason>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompatibilityRequest {
    pub accepted_health_versions: BTreeSet<u16>,
    pub accepted_protocol_versions: BTreeSet<u16>,
    pub accepted_schema_versions: BTreeSet<u16>,
    pub required_capabilities: BTreeSet<Capability>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Compatibility {
    Compatible,
    IncompatibleHealthVersion,
    IncompatibleProtocol,
    IncompatibleSchema,
    MissingCapabilities(BTreeSet<Capability>),
    Degraded(BTreeSet<DegradedReason>),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallHealthDecision {
    Accept,
    RollbackIncompatible,
    RollbackDegraded,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HealthError {
    InvalidBuild,
    InvalidVersion,
    InvalidLimits,
}

impl HealthSnapshot {
    /// Constructs a bounded versioned snapshot. Uptime is intentionally not represented.
    /// # Errors
    /// Returns a typed validation error for invalid build, versions, or limits.
    pub fn new(
        host_build: String,
        protocol_version: u16,
        schema_version: u16,
        capabilities: BTreeSet<Capability>,
        limits: HealthLimits,
        degraded: BTreeSet<DegradedReason>,
    ) -> Result<Self, HealthError> {
        if host_build.is_empty()
            || host_build.len() > MAX_FIELD
            || host_build.chars().any(char::is_control)
        {
            return Err(HealthError::InvalidBuild);
        }
        if protocol_version == 0 || schema_version == 0 {
            return Err(HealthError::InvalidVersion);
        }
        if limits.max_frame_bytes == 0 || limits.max_items == 0 {
            return Err(HealthError::InvalidLimits);
        }
        Ok(Self {
            version: HEALTH_VERSION,
            host_build,
            protocol_version,
            schema_version,
            capabilities,
            limits,
            degraded,
        })
    }
}
#[must_use]
pub fn negotiate(snapshot: &HealthSnapshot, request: &CompatibilityRequest) -> Compatibility {
    if !request.accepted_health_versions.contains(&snapshot.version) {
        return Compatibility::IncompatibleHealthVersion;
    }
    if !request
        .accepted_protocol_versions
        .contains(&snapshot.protocol_version)
    {
        return Compatibility::IncompatibleProtocol;
    }
    if !request
        .accepted_schema_versions
        .contains(&snapshot.schema_version)
    {
        return Compatibility::IncompatibleSchema;
    }
    let missing = request
        .required_capabilities
        .difference(&snapshot.capabilities)
        .copied()
        .collect::<BTreeSet<_>>();
    if !missing.is_empty() {
        return Compatibility::MissingCapabilities(missing);
    }
    if !snapshot.degraded.is_empty() {
        return Compatibility::Degraded(snapshot.degraded.clone());
    }
    Compatibility::Compatible
}
#[must_use]
pub fn install_decision(compatibility: &Compatibility) -> InstallHealthDecision {
    match compatibility {
        Compatibility::Compatible => InstallHealthDecision::Accept,
        Compatibility::Degraded(_) => InstallHealthDecision::RollbackDegraded,
        _ => InstallHealthDecision::RollbackIncompatible,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> HealthSnapshot {
        HealthSnapshot::new(
            "build-1".into(),
            1,
            1,
            [Capability::Services, Capability::Terminal].into(),
            HealthLimits {
                max_frame_bytes: 1024,
                max_items: 8,
            },
            BTreeSet::new(),
        )
        .unwrap()
    }
    fn request() -> CompatibilityRequest {
        CompatibilityRequest {
            accepted_health_versions: [1].into(),
            accepted_protocol_versions: [1].into(),
            accepted_schema_versions: [1].into(),
            required_capabilities: [Capability::Services].into(),
        }
    }
    #[test]
    fn exact_compatible_snapshot_is_accepted() {
        let result = negotiate(&snapshot(), &request());
        assert_eq!(result, Compatibility::Compatible);
        assert_eq!(install_decision(&result), InstallHealthDecision::Accept);
    }
    #[test]
    fn negotiation_classifies_version_failures_stably() {
        let mut r = request();
        r.accepted_protocol_versions = [2].into();
        let result = negotiate(&snapshot(), &r);
        assert_eq!(result, Compatibility::IncompatibleProtocol);
        assert_eq!(
            install_decision(&result),
            InstallHealthDecision::RollbackIncompatible
        );
    }
    #[test]
    fn missing_capabilities_are_sorted_and_exact() {
        let mut r = request();
        r.required_capabilities = [Capability::GitMetadata, Capability::AgentEvents].into();
        assert_eq!(
            negotiate(&snapshot(), &r),
            Compatibility::MissingCapabilities(
                [Capability::AgentEvents, Capability::GitMetadata].into()
            )
        );
    }
    #[test]
    fn degraded_health_triggers_distinct_rollback() {
        let mut s = snapshot();
        s.degraded.insert(DegradedReason::StorageReadOnly);
        let result = negotiate(&s, &request());
        assert_eq!(
            install_decision(&result),
            InstallHealthDecision::RollbackDegraded
        );
    }
    #[test]
    fn invalid_and_volatile_fields_cannot_enter_snapshot() {
        assert_eq!(
            HealthSnapshot::new(
                "bad\n".into(),
                1,
                1,
                BTreeSet::new(),
                HealthLimits {
                    max_frame_bytes: 1,
                    max_items: 1
                },
                BTreeSet::new()
            ),
            Err(HealthError::InvalidBuild)
        );
        assert_eq!(HEALTH_VERSION, 1);
    }
}
