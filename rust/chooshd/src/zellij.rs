//! Pure, shell-free planning for the Zellij command-line adapter.
//!
//! This module never starts a process and never interprets terminal output. An
//! outer adapter executes [`CommandPlan`] directly and converts bounded Zellij
//! output into the typed observations accepted by the reconciliation helpers.

use std::ffi::OsString;

use choosh_core::item::ZellijTarget;
use choosh_core::workspace::{CanonicalRoot, WorkspaceName};

const ZELLIJ: &str = "zellij";

/// Bounds applied before any command is handed to a process launcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlanLimits {
    pub max_process_args: usize,
    pub max_arg_bytes: usize,
}

impl Default for PlanLimits {
    fn default() -> Self {
        Self {
            max_process_args: 128,
            max_arg_bytes: 4_096,
        }
    }
}

/// An executable and argument vector that can be passed directly to `exec`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandPlan {
    pub executable: OsString,
    pub args: Vec<OsString>,
}

impl CommandPlan {
    fn zellij(args: Vec<OsString>, limits: PlanLimits) -> Result<Self, PlanError> {
        if limits.max_process_args == 0 || limits.max_arg_bytes == 0 {
            return Err(PlanError::InvalidLimits);
        }
        if args.len() > limits.max_process_args {
            return Err(PlanError::TooManyArguments);
        }
        if args.iter().any(|arg| os_bytes(arg) > limits.max_arg_bytes) {
            return Err(PlanError::ArgumentTooLong);
        }
        Ok(Self {
            executable: ZELLIJ.into(),
            args,
        })
    }
}

/// Stable planning failures, suitable for protocol error mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlanError {
    InvalidLimits,
    EmptyCommand,
    TooManyArguments,
    ArgumentTooLong,
    SessionMismatch,
}

impl PlanError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "invalid_zellij_plan_limits",
            Self::EmptyCommand => "empty_zellij_command",
            Self::TooManyArguments => "zellij_argument_count_exceeded",
            Self::ArgumentTooLong => "zellij_argument_too_long",
            Self::SessionMismatch => "zellij_target_session_mismatch",
        }
    }
}

/// Lists session identities for an outer adapter to parse into a bounded set.
///
/// # Errors
///
/// Returns [`PlanError`] when the supplied limits are invalid or cannot contain
/// the fixed lookup argument vector.
pub fn lookup_sessions(limits: PlanLimits) -> Result<CommandPlan, PlanError> {
    plan(["list-sessions", "--short", "--no-formatting"], limits)
}

/// Creates the registered same-named session without attaching a terminal.
///
/// # Errors
///
/// Returns [`PlanError`] when the supplied limits are invalid or an argument
/// count or byte length exceeds them.
pub fn create_session(
    session: &WorkspaceName,
    root: &CanonicalRoot,
    limits: PlanLimits,
) -> Result<CommandPlan, PlanError> {
    plan(
        [
            "--session",
            session.as_str(),
            "--new-session-with-layout",
            "default",
            "options",
            "--default-cwd",
            root.as_str(),
            "--attach-to-session",
            "false",
        ],
        limits,
    )
}

/// Creates one explicitly named managed tab and preserves the child argv.
///
/// # Errors
///
/// Returns [`PlanError::EmptyCommand`] for an empty child command, or another
/// [`PlanError`] when the limits are invalid or the combined argument vector
/// exceeds them.
pub fn create_tab(
    session: &WorkspaceName,
    root: &CanonicalRoot,
    tab: &str,
    command: &[OsString],
    limits: PlanLimits,
) -> Result<CommandPlan, PlanError> {
    if command.is_empty() {
        return Err(PlanError::EmptyCommand);
    }
    let mut args = strings([
        "--session",
        session.as_str(),
        "action",
        "new-tab",
        "--name",
        tab,
        "--cwd",
        root.as_str(),
        "--",
    ]);
    args.extend(command.iter().cloned());
    CommandPlan::zellij(args, limits)
}

/// Focuses the exact managed tab; the pane remains part of reconciliation.
///
/// # Errors
///
/// Returns [`PlanError`] when the supplied limits are invalid or an argument
/// count or byte length exceeds them.
pub fn focus_tab(target: &ZellijTarget, limits: PlanLimits) -> Result<CommandPlan, PlanError> {
    plan(
        [
            "--session",
            target.session(),
            "action",
            "go-to-tab-name",
            target.tab(),
        ],
        limits,
    )
}

/// Focuses the exact pane ID within its owning session.
///
/// # Errors
///
/// Returns [`PlanError`] when the supplied limits are invalid or an argument
/// count or byte length exceeds them.
pub fn focus_pane(target: &ZellijTarget, limits: PlanLimits) -> Result<CommandPlan, PlanError> {
    plan(
        [
            "--session",
            target.session(),
            "action",
            "focus-pane-id",
            target.pane(),
        ],
        limits,
    )
}

/// Stops only the exact managed pane. It does not delete item metadata or the session.
///
/// # Errors
///
/// Returns [`PlanError`] when the supplied limits are invalid or an argument
/// count or byte length exceeds them.
pub fn stop_pane(target: &ZellijTarget, limits: PlanLimits) -> Result<CommandPlan, PlanError> {
    plan(
        [
            "--session",
            target.session(),
            "action",
            "close-pane",
            "--pane-id",
            target.pane(),
        ],
        limits,
    )
}

/// Terminates a workspace session as a distinct destructive operation.
///
/// # Errors
///
/// Returns [`PlanError`] when the supplied limits are invalid or an argument
/// count or byte length exceeds them.
pub fn terminate_session(
    session: &WorkspaceName,
    limits: PlanLimits,
) -> Result<CommandPlan, PlanError> {
    plan(["delete-session", session.as_str(), "--force"], limits)
}

/// Attaches stdio to the exact session; no shell command is reconstructed.
///
/// # Errors
///
/// Returns [`PlanError`] when the supplied limits are invalid or an argument
/// count or byte length exceeds them.
pub fn attach(session: &WorkspaceName, limits: PlanLimits) -> Result<CommandPlan, PlanError> {
    plan(["attach", session.as_str()], limits)
}

/// Typed adapter observation after a bounded session lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionObservation {
    Missing,
    Present,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistrationIntent {
    Create,
    Adopt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistrationAction {
    CreateSession,
    RecordExistingSession,
}

/// Reconciles explicit registration intent without silently adopting or replacing.
///
/// # Errors
///
/// Returns [`ReconcileError::SessionAlreadyExists`] when creation would replace
/// an existing session, or [`ReconcileError::SessionNotFound`] when explicit
/// adoption names a missing session.
pub fn reconcile_registration(
    intent: RegistrationIntent,
    observed: SessionObservation,
) -> Result<RegistrationAction, ReconcileError> {
    match (intent, observed) {
        (RegistrationIntent::Create, SessionObservation::Missing) => {
            Ok(RegistrationAction::CreateSession)
        }
        (RegistrationIntent::Adopt, SessionObservation::Present) => {
            Ok(RegistrationAction::RecordExistingSession)
        }
        (RegistrationIntent::Create, SessionObservation::Present) => {
            Err(ReconcileError::SessionAlreadyExists)
        }
        (RegistrationIntent::Adopt, SessionObservation::Missing) => {
            Err(ReconcileError::SessionNotFound)
        }
    }
}

/// Typed target observation; only exact session/tab/pane identity is accepted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetObservation {
    Missing,
    Exact,
    Different {
        session: String,
        tab: String,
        pane: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetState {
    Unavailable,
    Available,
}

/// Reconnect decision for a previously persisted target. Missing targets are
/// reported to the caller; reconnect never creates a replacement session/tab.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReattachAction {
    AttachExisting,
    MarkUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconcileError {
    SessionAlreadyExists,
    SessionNotFound,
    TargetIdentityMismatch,
}

impl ReconcileError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::SessionAlreadyExists => "zellij_session_already_exists",
            Self::SessionNotFound => "zellij_session_not_found",
            Self::TargetIdentityMismatch => "zellij_target_identity_mismatch",
        }
    }
}

/// Converts a typed observation into availability without silently retargeting.
///
/// # Errors
///
/// Returns [`ReconcileError::TargetIdentityMismatch`] when the adapter observed
/// a different session, tab, or pane identity.
pub fn reconcile_target(observed: &TargetObservation) -> Result<TargetState, ReconcileError> {
    match observed {
        TargetObservation::Missing => Ok(TargetState::Unavailable),
        TargetObservation::Exact => Ok(TargetState::Available),
        TargetObservation::Different { .. } => Err(ReconcileError::TargetIdentityMismatch),
    }
}

/// Converts a post-reconnect observation into an explicit reattach decision.
///
/// A missing target remains unavailable so a reconnect cannot silently spawn a
/// new process or retarget an item by display name.
///
/// # Errors
///
/// Returns [`ReconcileError::TargetIdentityMismatch`] when the observed target
/// exists under the same name but is not the same target.
pub fn reattach_target(observed: &TargetObservation) -> Result<ReattachAction, ReconcileError> {
    match observed {
        TargetObservation::Missing => Ok(ReattachAction::MarkUnavailable),
        TargetObservation::Exact => Ok(ReattachAction::AttachExisting),
        TargetObservation::Different { .. } => Err(ReconcileError::TargetIdentityMismatch),
    }
}

fn plan<const N: usize>(args: [&str; N], limits: PlanLimits) -> Result<CommandPlan, PlanError> {
    CommandPlan::zellij(strings(args), limits)
}

fn strings<const N: usize>(args: [&str; N]) -> Vec<OsString> {
    args.into_iter().map(OsString::from).collect()
}

#[cfg(unix)]
fn os_bytes(value: &OsString) -> usize {
    use std::os::unix::ffi::OsStrExt;
    value.as_os_str().as_bytes().len()
}

#[cfg(not(unix))]
fn os_bytes(value: &OsString) -> usize {
    value.to_string_lossy().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(value: &str) -> WorkspaceName {
        WorkspaceName::parse(value, 128).unwrap()
    }

    fn root() -> CanonicalRoot {
        CanonicalRoot::from_adapter("/srv/project root/$HOME", 256).unwrap()
    }

    fn target() -> ZellijTarget {
        ZellijTarget::new(
            "session;still-data".into(),
            "tab $(touch nope)".into(),
            "pane`uname`".into(),
        )
        .unwrap()
    }

    fn argv(plan: CommandPlan) -> Vec<String> {
        plan.args
            .into_iter()
            .map(|value| value.into_string().unwrap())
            .collect()
    }

    #[test]
    fn preserves_hostile_child_arguments_as_distinct_argv() {
        let command = [
            OsString::from("sh"),
            OsString::from("literal;not-a-separator"),
            OsString::from("$(touch /tmp/nope)"),
            OsString::from("space kept"),
        ];
        let result = create_tab(
            &name("safe-session"),
            &root(),
            "tab name;data",
            &command,
            PlanLimits::default(),
        )
        .unwrap();
        assert_eq!(result.executable, OsString::from("zellij"));
        assert_eq!(
            argv(result),
            [
                "--session",
                "safe-session",
                "action",
                "new-tab",
                "--name",
                "tab name;data",
                "--cwd",
                "/srv/project root/$HOME",
                "--",
                "sh",
                "literal;not-a-separator",
                "$(touch /tmp/nope)",
                "space kept",
            ]
        );
    }

    #[test]
    fn exact_target_plans_are_golden_and_termination_is_separate() {
        assert_eq!(
            argv(focus_tab(&target(), PlanLimits::default()).unwrap()),
            [
                "--session",
                "session;still-data",
                "action",
                "go-to-tab-name",
                "tab $(touch nope)"
            ]
        );
        assert_eq!(
            argv(stop_pane(&target(), PlanLimits::default()).unwrap()),
            [
                "--session",
                "session;still-data",
                "action",
                "close-pane",
                "--pane-id",
                "pane`uname`"
            ]
        );
        assert_eq!(
            argv(terminate_session(&name("safe-session"), PlanLimits::default()).unwrap()),
            ["delete-session", "safe-session", "--force"]
        );
    }

    #[test]
    fn lookup_create_and_attach_are_stable() {
        assert_eq!(
            argv(lookup_sessions(PlanLimits::default()).unwrap()),
            ["list-sessions", "--short", "--no-formatting"]
        );
        assert_eq!(
            argv(attach(&name("alpha"), PlanLimits::default()).unwrap()),
            ["attach", "alpha"]
        );
        assert_eq!(
            argv(create_session(&name("alpha"), &root(), PlanLimits::default()).unwrap()),
            [
                "--session",
                "alpha",
                "--new-session-with-layout",
                "default",
                "options",
                "--default-cwd",
                "/srv/project root/$HOME",
                "--attach-to-session",
                "false"
            ]
        );
    }

    #[test]
    fn rejects_empty_or_excessive_process_vectors() {
        assert_eq!(
            create_tab(&name("alpha"), &root(), "tab", &[], PlanLimits::default()),
            Err(PlanError::EmptyCommand)
        );
        let command = [OsString::from("cmd"), OsString::from("arg")];
        assert_eq!(
            create_tab(
                &name("alpha"),
                &root(),
                "tab",
                &command,
                PlanLimits {
                    max_process_args: 4,
                    max_arg_bytes: 100
                }
            ),
            Err(PlanError::TooManyArguments)
        );
        assert_eq!(
            attach(
                &name("alpha"),
                PlanLimits {
                    max_process_args: 2,
                    max_arg_bytes: 3
                }
            ),
            Err(PlanError::ArgumentTooLong)
        );
    }

    #[test]
    fn reconciliation_never_silently_adopts_or_retargets() {
        assert_eq!(
            reconcile_registration(RegistrationIntent::Create, SessionObservation::Present),
            Err(ReconcileError::SessionAlreadyExists)
        );
        assert_eq!(
            reconcile_registration(RegistrationIntent::Adopt, SessionObservation::Missing),
            Err(ReconcileError::SessionNotFound)
        );
        assert_eq!(
            reconcile_target(&TargetObservation::Different {
                session: "other".into(),
                tab: "tab".into(),
                pane: "pane".into()
            }),
            Err(ReconcileError::TargetIdentityMismatch)
        );
        assert_eq!(
            reattach_target(&TargetObservation::Exact),
            Ok(ReattachAction::AttachExisting)
        );
        assert_eq!(
            reattach_target(&TargetObservation::Missing),
            Ok(ReattachAction::MarkUnavailable)
        );
    }
}
