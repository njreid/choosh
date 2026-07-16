//! Explicit workspace and Zellij lifecycle command authorization.

const MAX_ID: usize = 256;
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LifecycleCommand {
    Detach { workspace: String },
    Unregister { workspace: String },
    StopAgent { workspace: String, item: String },
    StopService { workspace: String, item: String },
    TerminateSession { workspace: String, session: String },
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfirmationEvidence {
    pub challenge_id: String,
    pub command: LifecycleCommand,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleOutcome {
    Detached,
    Unregistered,
    AgentStopped,
    ServiceStopped,
    SessionTerminated,
    AlreadyAbsent,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LifecycleError<E> {
    InvalidIdentity,
    ConfirmationRequired,
    ConfirmationScopeMismatch,
    Capability(E),
}

pub trait LifecycleCapabilities {
    type Error;
    /// Detaches presentation only.
    /// # Errors
    /// Returns adapter failure.
    fn detach(&mut self, workspace: &str) -> Result<bool, Self::Error>;
    /// Removes daemon metadata only.
    /// # Errors
    /// Returns adapter failure.
    fn unregister(&mut self, workspace: &str) -> Result<bool, Self::Error>;
    /// Stops one exact agent item.
    /// # Errors
    /// Returns adapter failure.
    fn stop_agent(&mut self, workspace: &str, item: &str) -> Result<bool, Self::Error>;
    /// Stops one exact service item.
    /// # Errors
    /// Returns adapter failure.
    fn stop_service(&mut self, workspace: &str, item: &str) -> Result<bool, Self::Error>;
    /// Terminates one exact session.
    /// # Errors
    /// Returns adapter failure.
    fn terminate_session(&mut self, workspace: &str, session: &str) -> Result<bool, Self::Error>;
}

/// Executes one explicit lifecycle command; no navigation state is accepted as input.
/// # Errors
/// Returns validation, confirmation, scope, or adapter errors. `false` adapter outcomes are
/// idempotent `AlreadyAbsent` results.
pub fn execute<C: LifecycleCapabilities>(
    capabilities: &mut C,
    command: &LifecycleCommand,
    evidence: Option<&ConfirmationEvidence>,
) -> Result<LifecycleOutcome, LifecycleError<C::Error>> {
    validate(command).map_err(|()| LifecycleError::InvalidIdentity)?;
    if !matches!(command, LifecycleCommand::Detach { .. }) {
        let proof = evidence.ok_or(LifecycleError::ConfirmationRequired)?;
        if proof.challenge_id.is_empty() || proof.command != *command {
            return Err(LifecycleError::ConfirmationScopeMismatch);
        }
    }
    let (changed, outcome) = match command {
        LifecycleCommand::Detach { workspace } => {
            (capabilities.detach(workspace), LifecycleOutcome::Detached)
        }
        LifecycleCommand::Unregister { workspace } => (
            capabilities.unregister(workspace),
            LifecycleOutcome::Unregistered,
        ),
        LifecycleCommand::StopAgent { workspace, item } => (
            capabilities.stop_agent(workspace, item),
            LifecycleOutcome::AgentStopped,
        ),
        LifecycleCommand::StopService { workspace, item } => (
            capabilities.stop_service(workspace, item),
            LifecycleOutcome::ServiceStopped,
        ),
        LifecycleCommand::TerminateSession { workspace, session } => (
            capabilities.terminate_session(workspace, session),
            LifecycleOutcome::SessionTerminated,
        ),
    };
    if changed.map_err(LifecycleError::Capability)? {
        Ok(outcome)
    } else {
        Ok(LifecycleOutcome::AlreadyAbsent)
    }
}
fn validate(command: &LifecycleCommand) -> Result<(), ()> {
    let values: Vec<&str> = match command {
        LifecycleCommand::Detach { workspace } | LifecycleCommand::Unregister { workspace } => {
            vec![workspace]
        }
        LifecycleCommand::StopAgent { workspace, item }
        | LifecycleCommand::StopService { workspace, item } => vec![workspace, item],
        LifecycleCommand::TerminateSession { workspace, session } => vec![workspace, session],
    };
    if values
        .into_iter()
        .any(|v| v.is_empty() || v.len() > MAX_ID || v.chars().any(char::is_control))
    {
        Err(())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Fake {
        calls: Vec<&'static str>,
        changed: bool,
    }
    impl LifecycleCapabilities for Fake {
        type Error = &'static str;
        fn detach(&mut self, _: &str) -> Result<bool, Self::Error> {
            self.calls.push("detach");
            Ok(self.changed)
        }
        fn unregister(&mut self, _: &str) -> Result<bool, Self::Error> {
            self.calls.push("unregister");
            Ok(self.changed)
        }
        fn stop_agent(&mut self, _: &str, _: &str) -> Result<bool, Self::Error> {
            self.calls.push("agent");
            Ok(self.changed)
        }
        fn stop_service(&mut self, _: &str, _: &str) -> Result<bool, Self::Error> {
            self.calls.push("service");
            Ok(self.changed)
        }
        fn terminate_session(&mut self, _: &str, _: &str) -> Result<bool, Self::Error> {
            self.calls.push("session");
            Ok(self.changed)
        }
    }
    fn proof(command: &LifecycleCommand) -> ConfirmationEvidence {
        ConfirmationEvidence {
            challenge_id: "challenge".into(),
            command: command.clone(),
        }
    }
    #[test]
    fn detach_is_non_destructive_and_needs_no_confirmation() {
        let mut f = Fake {
            changed: true,
            ..Fake::default()
        };
        assert_eq!(
            execute(
                &mut f,
                &LifecycleCommand::Detach {
                    workspace: "w".into()
                },
                None
            ),
            Ok(LifecycleOutcome::Detached)
        );
        assert_eq!(f.calls, ["detach"]);
    }
    #[test]
    fn destructive_commands_require_exact_scope() {
        let command = LifecycleCommand::StopService {
            workspace: "w".into(),
            item: "web".into(),
        };
        let wrong = proof(&LifecycleCommand::StopAgent {
            workspace: "w".into(),
            item: "web".into(),
        });
        let mut f = Fake::default();
        assert_eq!(
            execute(&mut f, &command, None),
            Err(LifecycleError::ConfirmationRequired)
        );
        assert_eq!(
            execute(&mut f, &command, Some(&wrong)),
            Err(LifecycleError::ConfirmationScopeMismatch)
        );
        assert!(f.calls.is_empty());
    }
    #[test]
    fn each_destructive_action_calls_only_its_capability() {
        let command = LifecycleCommand::TerminateSession {
            workspace: "w".into(),
            session: "s".into(),
        };
        let mut f = Fake {
            changed: true,
            ..Fake::default()
        };
        assert_eq!(
            execute(&mut f, &command, Some(&proof(&command))),
            Ok(LifecycleOutcome::SessionTerminated)
        );
        assert_eq!(f.calls, ["session"]);
    }
    #[test]
    fn missing_target_is_idempotent() {
        let command = LifecycleCommand::StopAgent {
            workspace: "w".into(),
            item: "a".into(),
        };
        let mut f = Fake::default();
        assert_eq!(
            execute(&mut f, &command, Some(&proof(&command))),
            Ok(LifecycleOutcome::AlreadyAbsent)
        );
    }
    #[test]
    fn invalid_identity_fails_before_confirmation_or_capability() {
        let command = LifecycleCommand::Unregister {
            workspace: "../\n".into(),
        };
        let mut f = Fake::default();
        assert_eq!(
            execute(&mut f, &command, Some(&proof(&command))),
            Err(LifecycleError::InvalidIdentity)
        );
        assert!(f.calls.is_empty());
    }
}
