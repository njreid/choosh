//! Merge-safe installation and bounded observational agent adapter domain.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdapterKind {
    Codex,
    OpenCode,
    ClaudeCode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Compatibility {
    pub minimum: Version,
    pub maximum: Version,
}

impl Compatibility {
    #[must_use]
    pub fn supports(self, version: Version) -> bool {
        version_key(version) >= version_key(self.minimum)
            && version_key(version) <= version_key(self.maximum)
    }
}

fn version_key(version: Version) -> (u16, u16) {
    (version.major, version.minor)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterManifest {
    pub kind: AdapterKind,
    pub adapter_version: Version,
    pub agent_versions: Compatibility,
    pub owned_path: String,
    pub owned_content: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Capability {
    Observational,
    TerminalOnly,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExistingFile {
    Missing,
    Present(Vec<u8>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileMutation {
    Write {
        path: String,
        expected: ExistingFile,
        content: Vec<u8>,
    },
    Remove {
        path: String,
        expected_content: Vec<u8>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallationPlan {
    pub capability: Capability,
    pub mutation: Option<FileMutation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdapterError {
    InvalidManifest,
    Conflict,
    NotOwned,
    PayloadTooLarge,
    TooManyPaths,
    FieldTooLarge,
    UnsafePath,
    InvalidHookInput,
}

/// Ingests one bounded JSON hook payload from a fixed-argv stdin adapter.
///
/// The adapter accepts exactly one argument naming the installed adapter and
/// treats all stdin fields as untrusted observations. It never executes an
/// agent command or emits a control decision.
pub fn ingest_hook_stdin(
    adapter: AdapterKind,
    argv: &[&str],
    stdin: &[u8],
    limits: ObservationLimits,
) -> Result<NormalizedObservation, AdapterError> {
    let expected = match adapter {
        AdapterKind::Codex => "codex",
        AdapterKind::OpenCode => "opencode",
        AdapterKind::ClaudeCode => "claude",
    };
    if argv.len() != 1 || argv[0] != expected || stdin.len() > limits.max_payload_bytes {
        return Err(AdapterError::InvalidHookInput);
    }
    let value: serde_json::Value =
        serde_json::from_slice(stdin).map_err(|_| AdapterError::InvalidHookInput)?;
    let object = value.as_object().ok_or(AdapterError::InvalidHookInput)?;
    let kind = match object.get("kind").and_then(serde_json::Value::as_str) {
        Some("input_required") => ObservationKind::InputRequired,
        Some("turn_completed") => ObservationKind::TurnCompleted,
        Some("files_changed") => ObservationKind::FilesChanged,
        Some("busy") => ObservationKind::Busy,
        Some("stopped") => ObservationKind::Stopped,
        Some("failed") => ObservationKind::Failed,
        _ => return Err(AdapterError::InvalidHookInput),
    };
    let string_field = |name: &str| {
        object
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or(AdapterError::InvalidHookInput)
    };
    let paths = object
        .get("paths")
        .map(|v| {
            v.as_array()
                .ok_or(AdapterError::InvalidHookInput)
                .and_then(|items| {
                    items
                        .iter()
                        .map(|item| {
                            item.as_str()
                                .map(str::to_owned)
                                .ok_or(AdapterError::InvalidHookInput)
                        })
                        .collect()
                })
        })
        .transpose()?
        .unwrap_or_default();
    let diagnostic_code = object
        .get("diagnostic_code")
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or(AdapterError::InvalidHookInput)
        })
        .transpose()?;
    normalize_observation(
        HookObservation {
            kind,
            workspace_id: string_field("workspace_id")?,
            item_id: string_field("item_id")?,
            paths,
            diagnostic_code,
        },
        limits,
    )
}

/// Plans an explicit opt-in install without replacing unrelated bytes.
///
/// # Errors
///
/// Rejects invalid manifests and any occupied owned path whose bytes are not
/// exactly the adapter's bytes. Incompatibility returns terminal-only operation.
pub fn plan_install(
    manifest: &AdapterManifest,
    agent_version: Version,
    existing: ExistingFile,
) -> Result<InstallationPlan, AdapterError> {
    validate_manifest(manifest)?;
    if !manifest.agent_versions.supports(agent_version) {
        return Ok(InstallationPlan {
            capability: Capability::TerminalOnly,
            mutation: None,
        });
    }
    match existing {
        ExistingFile::Present(content) if content == manifest.owned_content => {
            Ok(InstallationPlan {
                capability: Capability::Observational,
                mutation: None,
            })
        }
        ExistingFile::Present(_) => Err(AdapterError::Conflict),
        ExistingFile::Missing => Ok(InstallationPlan {
            capability: Capability::Observational,
            mutation: Some(FileMutation::Write {
                path: manifest.owned_path.clone(),
                expected: ExistingFile::Missing,
                content: manifest.owned_content.clone(),
            }),
        }),
    }
}

/// Plans uninstall only when the current bytes exactly match owned content.
///
/// # Errors
///
/// Returns [`AdapterError::NotOwned`] rather than deleting missing or modified
/// content, and rejects invalid manifests.
pub fn plan_uninstall(
    manifest: &AdapterManifest,
    existing: ExistingFile,
) -> Result<FileMutation, AdapterError> {
    validate_manifest(manifest)?;
    match existing {
        ExistingFile::Present(content) if content == manifest.owned_content => {
            Ok(FileMutation::Remove {
                path: manifest.owned_path.clone(),
                expected_content: content,
            })
        }
        ExistingFile::Missing | ExistingFile::Present(_) => Err(AdapterError::NotOwned),
    }
}

fn validate_manifest(manifest: &AdapterManifest) -> Result<(), AdapterError> {
    if manifest.owned_path.is_empty()
        || manifest.owned_path.starts_with('/')
        || manifest
            .owned_path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || manifest.owned_content.is_empty()
        || !manifest
            .agent_versions
            .supports(manifest.agent_versions.minimum)
    {
        Err(AdapterError::InvalidManifest)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationKind {
    InputRequired,
    TurnCompleted,
    FilesChanged,
    Busy,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HookObservation {
    pub kind: ObservationKind,
    pub workspace_id: String,
    pub item_id: String,
    pub paths: Vec<String>,
    pub diagnostic_code: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservationLimits {
    pub max_payload_bytes: usize,
    pub max_field_bytes: usize,
    pub max_paths: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedObservation {
    pub kind: ObservationKind,
    pub workspace_id: String,
    pub item_id: String,
    pub paths: Vec<String>,
    pub diagnostic_code: Option<String>,
}

/// Bounds and validates observational hook data without granting control actions.
///
/// # Errors
///
/// Rejects oversized fields/payloads, too many paths, and paths that are not
/// lexical root-relative values. No partial event is returned.
pub fn normalize_observation(
    observation: HookObservation,
    limits: ObservationLimits,
) -> Result<NormalizedObservation, AdapterError> {
    if limits.max_payload_bytes == 0 || limits.max_field_bytes == 0 || limits.max_paths == 0 {
        return Err(AdapterError::FieldTooLarge);
    }
    if observation.paths.len() > limits.max_paths {
        return Err(AdapterError::TooManyPaths);
    }
    let fields = [&observation.workspace_id, &observation.item_id];
    if fields
        .iter()
        .any(|field| field.is_empty() || field.len() > limits.max_field_bytes)
        || observation
            .diagnostic_code
            .as_ref()
            .is_some_and(|value| value.len() > limits.max_field_bytes)
    {
        return Err(AdapterError::FieldTooLarge);
    }
    for path in &observation.paths {
        if path.is_empty() || path.len() > limits.max_field_bytes {
            return Err(AdapterError::FieldTooLarge);
        }
        if path.starts_with('/')
            || path.contains('\0')
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(AdapterError::UnsafePath);
        }
    }
    let size = observation
        .workspace_id
        .len()
        .checked_add(observation.item_id.len())
        .and_then(|size| {
            observation
                .paths
                .iter()
                .try_fold(size, |sum, path| sum.checked_add(path.len()))
        })
        .and_then(|size| {
            size.checked_add(observation.diagnostic_code.as_ref().map_or(0, String::len))
        })
        .ok_or(AdapterError::PayloadTooLarge)?;
    if size > limits.max_payload_bytes {
        return Err(AdapterError::PayloadTooLarge);
    }
    Ok(NormalizedObservation {
        kind: observation.kind,
        workspace_id: observation.workspace_id,
        item_id: observation.item_id,
        paths: observation.paths,
        diagnostic_code: observation.diagnostic_code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(kind: AdapterKind) -> AdapterManifest {
        AdapterManifest {
            kind,
            adapter_version: Version { major: 1, minor: 2 },
            agent_versions: Compatibility {
                minimum: Version { major: 1, minor: 0 },
                maximum: Version { major: 2, minor: 5 },
            },
            owned_path: format!("choosh/{kind:?}.hook"),
            owned_content: b"choosh-observer-v1".to_vec(),
        }
    }

    #[test]
    fn all_three_adapters_install_idempotently_with_preconditions() {
        for kind in [
            AdapterKind::Codex,
            AdapterKind::OpenCode,
            AdapterKind::ClaudeCode,
        ] {
            let manifest = manifest(kind);
            let plan = plan_install(
                &manifest,
                Version { major: 1, minor: 9 },
                ExistingFile::Missing,
            )
            .unwrap();
            assert!(matches!(
                plan.mutation,
                Some(FileMutation::Write {
                    expected: ExistingFile::Missing,
                    ..
                })
            ));
            assert_eq!(
                plan_install(
                    &manifest,
                    Version { major: 1, minor: 9 },
                    ExistingFile::Present(manifest.owned_content.clone())
                )
                .unwrap()
                .mutation,
                None
            );
        }
    }

    #[test]
    fn conflict_and_incompatibility_never_block_terminal_use() {
        let manifest = manifest(AdapterKind::Codex);
        assert_eq!(
            plan_install(
                &manifest,
                Version { major: 1, minor: 1 },
                ExistingFile::Present(b"user".to_vec())
            ),
            Err(AdapterError::Conflict)
        );
        assert_eq!(
            plan_install(
                &manifest,
                Version { major: 9, minor: 0 },
                ExistingFile::Missing
            )
            .unwrap()
            .capability,
            Capability::TerminalOnly
        );
    }

    #[test]
    fn uninstall_requires_exact_owned_bytes() {
        let manifest = manifest(AdapterKind::ClaudeCode);
        assert!(matches!(
            plan_uninstall(
                &manifest,
                ExistingFile::Present(manifest.owned_content.clone())
            ),
            Ok(FileMutation::Remove { .. })
        ));
        assert_eq!(
            plan_uninstall(&manifest, ExistingFile::Present(b"modified".to_vec())),
            Err(AdapterError::NotOwned)
        );
    }

    fn limits() -> ObservationLimits {
        ObservationLimits {
            max_payload_bytes: 80,
            max_field_bytes: 24,
            max_paths: 2,
        }
    }

    #[test]
    fn observations_are_bounded_and_carry_no_control_action() {
        let event = normalize_observation(
            HookObservation {
                kind: ObservationKind::FilesChanged,
                workspace_id: "workspace-1".into(),
                item_id: "item-1".into(),
                paths: vec!["src/main.rs".into()],
                diagnostic_code: None,
            },
            limits(),
        )
        .unwrap();
        assert_eq!(event.paths, ["src/main.rs"]);
    }

    #[test]
    fn stdin_hook_requires_fixed_argv_and_normalizes_json() {
        let payload = br#"{"kind":"files_changed","workspace_id":"workspace-1","item_id":"item-1","paths":["src/lib.rs"]}"#;
        let event = ingest_hook_stdin(
            AdapterKind::Codex,
            &["codex"],
            payload,
            ObservationLimits {
                max_payload_bytes: 256,
                ..limits()
            },
        )
        .unwrap();
        assert_eq!(event.kind, ObservationKind::FilesChanged);
        assert_eq!(event.paths, ["src/lib.rs"]);
        assert_eq!(
            ingest_hook_stdin(AdapterKind::Codex, &["--approve"], payload, limits()),
            Err(AdapterError::InvalidHookInput)
        );
    }

    #[test]
    fn stdin_hook_rejects_malformed_or_oversized_json() {
        assert_eq!(
            ingest_hook_stdin(AdapterKind::ClaudeCode, &["claude"], br"[]", limits()),
            Err(AdapterError::InvalidHookInput)
        );
        let oversized = vec![b'x'; 81];
        assert_eq!(
            ingest_hook_stdin(AdapterKind::OpenCode, &["opencode"], &oversized, limits()),
            Err(AdapterError::InvalidHookInput)
        );
    }

    #[test]
    fn unsafe_or_oversized_payloads_fail_without_partial_events() {
        for path in ["../secret", "/absolute", "a//b", "a\0b"] {
            let result = normalize_observation(
                HookObservation {
                    kind: ObservationKind::FilesChanged,
                    workspace_id: "w".into(),
                    item_id: "i".into(),
                    paths: vec![path.into()],
                    diagnostic_code: None,
                },
                limits(),
            );
            assert_eq!(result, Err(AdapterError::UnsafePath));
        }
        let mut small = limits();
        small.max_payload_bytes = 3;
        assert_eq!(
            normalize_observation(
                HookObservation {
                    kind: ObservationKind::Busy,
                    workspace_id: "ww".into(),
                    item_id: "ii".into(),
                    paths: vec![],
                    diagnostic_code: None,
                },
                small
            ),
            Err(AdapterError::PayloadTooLarge)
        );
    }
}
