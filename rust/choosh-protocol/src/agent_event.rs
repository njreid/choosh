//! Dependency-free normalization of untrusted observational agent hook events.
//!
//! Adapter timestamps are metadata only. Ordering is assigned by the daemon in
//! [`ReceivedAgentEvent`] using its own receipt time and workspace sequence.

use core::fmt;

pub const MAX_CAPTURE_BYTES: usize = 64 * 1024;
pub const MAX_PATHS: usize = 1_000;
pub const MAX_PATH_BYTES: usize = 4_096;
pub const MAX_TIMESTAMP_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentAdapter {
    Codex,
    Claude,
    OpenCode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputReason {
    Approval,
    Permission,
    Question,
    Elicitation,
    NextPrompt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentStatus {
    Starting,
    Busy,
    Waiting,
    Stopped,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeOperation {
    Added,
    Modified,
    Deleted,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidatePath {
    pub path: String,
    pub operation: ChangeOperation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentEventKind {
    InputRequired {
        reason: InputReason,
        summary: CoarseSummary,
    },
    TurnCompleted,
    FilesChanged {
        candidates: Vec<CandidatePath>,
    },
    AgentStatus {
        status: AgentStatus,
        diagnostic: Option<DiagnosticCode>,
    },
}

/// Safe display text is selected locally and never derived from vendor text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoarseSummary {
    ApprovalRequired,
    PermissionRequired,
    QuestionWaiting,
    InputRequested,
    ReadyForNextPrompt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticCode {
    AgentReportedFailure,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedAgentEvent {
    pub workspace_id: String,
    pub item_id: String,
    pub adapter: AgentAdapter,
    /// Untrusted adapter metadata; this field never determines ordering.
    pub adapter_occurred_at: String,
    pub kind: AgentEventKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReceivedAgentEvent {
    pub event: NormalizedAgentEvent,
    /// Daemon-owned monotonically increasing sequence within the workspace.
    pub workspace_sequence: u64,
    /// Daemon clock value, authoritative for receipt ordering.
    pub received_at: String,
}

impl ReceivedAgentEvent {
    /// Adds daemon-owned ordering metadata to a validated normalized event.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidSequence`] for sequence zero or
    /// [`ValidationError::InvalidTimestamp`] for a malformed receipt time.
    pub fn new(
        event: NormalizedAgentEvent,
        workspace_sequence: u64,
        received_at: &str,
    ) -> Result<Self, ValidationError> {
        validate_timestamp(received_at)?;
        if workspace_sequence == 0 {
            return Err(ValidationError::InvalidSequence);
        }
        Ok(Self {
            event,
            workspace_sequence,
            received_at: received_at.to_owned(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterContext<'a> {
    pub workspace_id: &'a str,
    pub item_id: &'a str,
    pub root: &'a str,
    pub adapter: AgentAdapter,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedHook<'a> {
    pub surface: &'a str,
    pub occurred_at: &'a str,
    pub candidate_paths: &'a [(&'a str, ChangeOperation)],
    /// Total original capture size, checked before any mapping occurs.
    pub captured_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationError {
    MissingContext,
    AdapterMismatch,
    CaptureTooLarge,
    InvalidWorkspaceId,
    InvalidItemId,
    InvalidRoot,
    InvalidTimestamp,
    UnknownSurface,
    TooManyPaths,
    InvalidCandidatePath,
    InvalidSequence,
}

impl ValidationError {
    /// Stable, payload-free diagnostic suitable for logs and fixtures.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::MissingContext => "agent_event.missing_context",
            Self::AdapterMismatch => "agent_event.adapter_mismatch",
            Self::CaptureTooLarge => "agent_event.capture_too_large",
            Self::InvalidWorkspaceId => "agent_event.invalid_workspace_id",
            Self::InvalidItemId => "agent_event.invalid_item_id",
            Self::InvalidRoot => "agent_event.invalid_root",
            Self::InvalidTimestamp => "agent_event.invalid_timestamp",
            Self::UnknownSurface => "agent_event.unknown_surface",
            Self::TooManyPaths => "agent_event.too_many_paths",
            Self::InvalidCandidatePath => "agent_event.invalid_candidate_path",
            Self::InvalidSequence => "agent_event.invalid_sequence",
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for ValidationError {}

/// Normalizes a captured Codex hook without executing or contacting Codex.
///
/// # Errors
///
/// Returns a stable [`ValidationError`] when context or captured input fails
/// bounds and syntax checks, or the surface is not supported for Codex.
pub fn normalize_codex(
    context: &AdapterContext<'_>,
    hook: &CapturedHook<'_>,
) -> Result<NormalizedAgentEvent, ValidationError> {
    normalize(AgentAdapter::Codex, context, hook)
}

/// Normalizes a captured Claude Code hook without executing or contacting it.
///
/// # Errors
///
/// Returns a stable [`ValidationError`] when context or captured input fails
/// bounds and syntax checks, or the surface is not supported for Claude Code.
pub fn normalize_claude(
    context: &AdapterContext<'_>,
    hook: &CapturedHook<'_>,
) -> Result<NormalizedAgentEvent, ValidationError> {
    normalize(AgentAdapter::Claude, context, hook)
}

/// Normalizes a captured `OpenCode` hook without executing or contacting it.
///
/// # Errors
///
/// Returns a stable [`ValidationError`] when context or captured input fails
/// bounds and syntax checks, or the surface is not supported for `OpenCode`.
pub fn normalize_opencode(
    context: &AdapterContext<'_>,
    hook: &CapturedHook<'_>,
) -> Result<NormalizedAgentEvent, ValidationError> {
    normalize(AgentAdapter::OpenCode, context, hook)
}

fn normalize(
    expected: AgentAdapter,
    context: &AdapterContext<'_>,
    hook: &CapturedHook<'_>,
) -> Result<NormalizedAgentEvent, ValidationError> {
    validate_context(context)?;
    if context.adapter != expected {
        return Err(ValidationError::AdapterMismatch);
    }
    if hook.captured_bytes > MAX_CAPTURE_BYTES {
        return Err(ValidationError::CaptureTooLarge);
    }
    validate_timestamp(hook.occurred_at)?;
    let kind = map_surface(expected, hook)?;
    Ok(NormalizedAgentEvent {
        workspace_id: context.workspace_id.to_owned(),
        item_id: context.item_id.to_owned(),
        adapter: expected,
        adapter_occurred_at: hook.occurred_at.to_owned(),
        kind,
    })
}

fn map_surface(
    adapter: AgentAdapter,
    hook: &CapturedHook<'_>,
) -> Result<AgentEventKind, ValidationError> {
    let input = |reason, summary| AgentEventKind::InputRequired { reason, summary };
    let kind = match (adapter, hook.surface) {
        (AgentAdapter::Codex, "PermissionRequest") => {
            input(InputReason::Approval, CoarseSummary::ApprovalRequired)
        }
        (AgentAdapter::Claude, "PermissionRequest")
        | (AgentAdapter::OpenCode, "permission.asked") => {
            input(InputReason::Permission, CoarseSummary::PermissionRequired)
        }
        (AgentAdapter::Claude, "Notification") => {
            input(InputReason::Question, CoarseSummary::QuestionWaiting)
        }
        (AgentAdapter::Codex | AgentAdapter::Claude, "UserPromptSubmit")
        | (AgentAdapter::OpenCode, "permission.replied") => AgentEventKind::AgentStatus {
            status: AgentStatus::Busy,
            diagnostic: None,
        },
        (AgentAdapter::Codex, "PostToolUse")
        | (AgentAdapter::Claude, "FileChanged" | "PostToolUse")
        | (AgentAdapter::OpenCode, "file.edited" | "session.diff") => paths(hook)?,
        (AgentAdapter::Codex | AgentAdapter::Claude, "Stop")
        | (AgentAdapter::OpenCode, "session.idle") => AgentEventKind::TurnCompleted,
        (AgentAdapter::OpenCode, "session.error") => AgentEventKind::AgentStatus {
            status: AgentStatus::Failed,
            diagnostic: Some(DiagnosticCode::AgentReportedFailure),
        },
        _ => return Err(ValidationError::UnknownSurface),
    };
    Ok(kind)
}

fn paths(hook: &CapturedHook<'_>) -> Result<AgentEventKind, ValidationError> {
    if hook.candidate_paths.len() > MAX_PATHS {
        return Err(ValidationError::TooManyPaths);
    }
    let mut candidates = Vec::with_capacity(hook.candidate_paths.len());
    for &(path, operation) in hook.candidate_paths {
        validate_candidate_path(path)?;
        if !candidates
            .iter()
            .any(|candidate: &CandidatePath| candidate.path == path)
        {
            candidates.push(CandidatePath {
                path: path.to_owned(),
                operation,
            });
        }
    }
    Ok(AgentEventKind::FilesChanged { candidates })
}

fn validate_context(context: &AdapterContext<'_>) -> Result<(), ValidationError> {
    if context.workspace_id.is_empty() || context.item_id.is_empty() || context.root.is_empty() {
        return Err(ValidationError::MissingContext);
    }
    if !is_uuid(context.workspace_id) {
        return Err(ValidationError::InvalidWorkspaceId);
    }
    if !is_uuid(context.item_id) {
        return Err(ValidationError::InvalidItemId);
    }
    if !context.root.starts_with('/')
        || context.root.len() > MAX_PATH_BYTES
        || context.root.as_bytes().iter().any(u8::is_ascii_control)
    {
        return Err(ValidationError::InvalidRoot);
    }
    Ok(())
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}

fn validate_timestamp(value: &str) -> Result<(), ValidationError> {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes.len() > MAX_TIMESTAMP_BYTES
        || bytes.iter().any(u8::is_ascii_control)
    {
        return Err(ValidationError::InvalidTimestamp);
    }
    // Deliberately small RFC 3339 shape check; adapters do not own ordering.
    if !bytes.get(..19).is_some_and(|prefix| {
        prefix
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7 | 10 | 13 | 16) || byte.is_ascii_digit())
    }) || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return Err(ValidationError::InvalidTimestamp);
    }
    let suffix = &bytes[19..];
    let zone_start = suffix
        .iter()
        .position(|byte| matches!(byte, b'Z' | b'+' | b'-'))
        .ok_or(ValidationError::InvalidTimestamp)?;
    let fraction = &suffix[..zone_start];
    if !fraction.is_empty()
        && (fraction[0] != b'.'
            || fraction.len() == 1
            || !fraction[1..].iter().all(u8::is_ascii_digit))
    {
        return Err(ValidationError::InvalidTimestamp);
    }
    let zone = &suffix[zone_start..];
    if zone != b"Z"
        && !(zone.len() == 6
            && matches!(zone[0], b'+' | b'-')
            && zone[1..3].iter().all(u8::is_ascii_digit)
            && zone[3] == b':'
            && zone[4..].iter().all(u8::is_ascii_digit))
    {
        return Err(ValidationError::InvalidTimestamp);
    }
    Ok(())
}

fn validate_candidate_path(path: &str) -> Result<(), ValidationError> {
    if path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || path.starts_with('/')
        || path.starts_with('\\')
        || path
            .as_bytes()
            .iter()
            .any(|byte| *byte == 0 || byte.is_ascii_control())
        || path
            .split(['/', '\\'])
            .any(|part| part.is_empty() || part == "." || part == "..")
        || (path.len() >= 2 && path.as_bytes()[1] == b':')
    {
        return Err(ValidationError::InvalidCandidatePath);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "11111111-1111-4111-8111-111111111111";
    const ITEM: &str = "22222222-2222-4222-8222-222222222222";
    const TIME: &str = "2026-01-02T03:04:05Z";

    fn context(adapter: AgentAdapter) -> AdapterContext<'static> {
        AdapterContext {
            workspace_id: WORKSPACE,
            item_id: ITEM,
            root: "/fixture/root",
            adapter,
        }
    }
    fn hook(surface: &'static str) -> CapturedHook<'static> {
        CapturedHook {
            surface,
            occurred_at: TIME,
            candidate_paths: &[],
            captured_bytes: 32,
        }
    }

    #[test]
    fn maps_each_vendor_without_executing_it() {
        assert!(matches!(
            normalize_codex(&context(AgentAdapter::Codex), &hook("PermissionRequest"))
                .unwrap()
                .kind,
            AgentEventKind::InputRequired {
                reason: InputReason::Approval,
                ..
            }
        ));
        assert!(matches!(
            normalize_claude(&context(AgentAdapter::Claude), &hook("Stop"))
                .unwrap()
                .kind,
            AgentEventKind::TurnCompleted
        ));
        assert!(matches!(
            normalize_opencode(&context(AgentAdapter::OpenCode), &hook("session.error"))
                .unwrap()
                .kind,
            AgentEventKind::AgentStatus {
                status: AgentStatus::Failed,
                ..
            }
        ));
    }

    #[test]
    fn candidate_paths_are_lexical_untrusted_hints_and_deduplicated() {
        let paths = [
            ("src/main.rs", ChangeOperation::Modified),
            ("src/main.rs", ChangeOperation::Unknown),
        ];
        let captured = CapturedHook {
            candidate_paths: &paths,
            ..hook("file.edited")
        };
        let event = normalize_opencode(&context(AgentAdapter::OpenCode), &captured).unwrap();
        let AgentEventKind::FilesChanged { candidates } = event.kind else {
            panic!("wrong kind")
        };
        assert_eq!(
            candidates,
            vec![CandidatePath {
                path: "src/main.rs".into(),
                operation: ChangeOperation::Modified
            }]
        );
    }

    #[test]
    fn rejects_escapes_absolute_windows_and_controls_without_echo() {
        for bad in [
            "../secret",
            "/etc/passwd",
            "C:\\secret",
            "a//b",
            "a\nsecret",
        ] {
            let paths = [(bad, ChangeOperation::Unknown)];
            let captured = CapturedHook {
                candidate_paths: &paths,
                ..hook("PostToolUse")
            };
            let error = normalize_codex(&context(AgentAdapter::Codex), &captured).unwrap_err();
            assert_eq!(error.code(), "agent_event.invalid_candidate_path");
            assert!(!error.to_string().contains(bad));
        }
    }

    #[test]
    fn rejects_incomplete_mismatched_and_malformed_context() {
        let missing = AdapterContext {
            workspace_id: "",
            ..context(AgentAdapter::Codex)
        };
        assert_eq!(
            normalize_codex(&missing, &hook("Stop")),
            Err(ValidationError::MissingContext)
        );
        assert_eq!(
            normalize_codex(&context(AgentAdapter::Claude), &hook("Stop")),
            Err(ValidationError::AdapterMismatch)
        );
        let malformed = AdapterContext {
            item_id: "not-an-id",
            ..context(AgentAdapter::Codex)
        };
        assert_eq!(
            normalize_codex(&malformed, &hook("Stop")),
            Err(ValidationError::InvalidItemId)
        );

        let uppercase = AdapterContext {
            workspace_id: "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA",
            ..context(AgentAdapter::Codex)
        };
        assert_eq!(
            normalize_codex(&uppercase, &hook("Stop")),
            Err(ValidationError::InvalidWorkspaceId)
        );
    }

    #[test]
    fn rejects_oversized_and_unknown_captures_before_copying_payload() {
        let oversized = CapturedHook {
            surface: "canary-secret",
            captured_bytes: MAX_CAPTURE_BYTES + 1,
            ..hook("Stop")
        };
        assert_eq!(
            normalize_codex(&context(AgentAdapter::Codex), &oversized),
            Err(ValidationError::CaptureTooLarge)
        );
        let error =
            normalize_codex(&context(AgentAdapter::Codex), &hook("canary-secret")).unwrap_err();
        assert_eq!(error.to_string(), "agent_event.unknown_surface");
    }

    #[test]
    fn adapter_timestamp_cannot_supply_daemon_ordering() {
        let event = normalize_codex(&context(AgentAdapter::Codex), &hook("Stop")).unwrap();
        let received = ReceivedAgentEvent::new(event, 7, "2026-02-03T04:05:06Z").unwrap();
        assert_eq!(received.workspace_sequence, 7);
        assert_eq!(received.received_at, "2026-02-03T04:05:06Z");
        assert_eq!(
            ReceivedAgentEvent::new(received.event, 0, TIME),
            Err(ValidationError::InvalidSequence)
        );
    }

    #[test]
    fn normalized_model_has_only_observational_outcomes() {
        // Exhaustive construction documents the complete event surface: there
        // is no approval decision, rewritten input, or model-context field.
        let kinds = [
            AgentEventKind::InputRequired {
                reason: InputReason::Elicitation,
                summary: CoarseSummary::InputRequested,
            },
            AgentEventKind::TurnCompleted,
            AgentEventKind::FilesChanged { candidates: vec![] },
            AgentEventKind::AgentStatus {
                status: AgentStatus::Waiting,
                diagnostic: None,
            },
        ];
        assert_eq!(kinds.len(), 4);
    }
}
