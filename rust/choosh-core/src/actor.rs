//! Deterministic model for the canonical command, snapshot, and subscription contract.

use crate::ports::PortError;
use std::collections::{HashMap, HashSet};

pub type CommandId = String;
pub type Generation = u64;
pub type Revision = u64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandEnvelope {
    pub command_id: CommandId,
    pub target_generation: Generation,
    pub expected_revision: Option<Revision>,
    pub command: Command,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    OpenDocument {
        document_id: String,
        content: String,
    },
    ApplyEdit {
        document_id: String,
        start_byte: usize,
        end_byte: usize,
        replacement: String,
    },
    CloseDocument {
        document_id: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandResult {
    Snapshot(Snapshot),
    Closed { document_id: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandOutcome {
    Completed(CommandResult),
    Failed(PortError),
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentStatus {
    Clean,
    Dirty,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub generation: Generation,
    pub document_id: String,
    pub revision: Revision,
    pub status: DocumentStatus,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Document {
    id: String,
    revision: Revision,
    status: DocumentStatus,
    content: String,
}

impl Document {
    fn snapshot(&self, generation: Generation) -> Snapshot {
        Snapshot {
            generation,
            document_id: self.id.clone(),
            revision: self.revision,
            status: self.status.clone(),
            content: self.content.clone(),
        }
    }
}

/// Pure model used as the oracle for future asynchronous actor and bridge adapters.
#[derive(Debug)]
pub struct StateEngine {
    generation: Generation,
    document: Option<Document>,
    cancelled: HashSet<CommandId>,
    outcomes: HashMap<CommandId, CommandOutcome>,
    subscriptions: SubscriptionRegistry,
}

impl StateEngine {
    #[must_use]
    pub fn new(generation: Generation) -> Self {
        Self {
            generation,
            document: None,
            cancelled: HashSet::new(),
            outcomes: HashMap::new(),
            subscriptions: SubscriptionRegistry::default(),
        }
    }

    #[must_use]
    pub const fn generation(&self) -> Generation {
        self.generation
    }

    /// Advances the engine generation after process or bridge recreation.
    /// Existing commands and subscriptions cannot target the new generation.
    ///
    /// # Errors
    ///
    /// Returns `generation_overflow` instead of wrapping the generation.
    pub fn recreate(&mut self) -> Result<Generation, PortError> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| PortError::new("generation_overflow", false))?;
        self.subscriptions.close_all();
        Ok(self.generation)
    }

    /// Marks a command cancelled if it has not already reached a terminal outcome.
    /// Repeated cancellation is idempotent.
    pub fn cancel(&mut self, command_id: &str) {
        if !self.outcomes.contains_key(command_id) {
            self.cancelled.insert(command_id.to_owned());
        }
    }

    /// Applies a command exactly once and returns its cached terminal outcome on replay.
    pub fn dispatch(&mut self, envelope: CommandEnvelope) -> CommandOutcome {
        if let Some(outcome) = self.outcomes.get(&envelope.command_id) {
            return outcome.clone();
        }

        let outcome = if self.cancelled.remove(&envelope.command_id) {
            CommandOutcome::Cancelled
        } else if envelope.target_generation != self.generation {
            CommandOutcome::Failed(PortError::new("stale_generation", false))
        } else {
            self.apply(envelope.expected_revision, envelope.command)
        };

        self.outcomes.insert(envelope.command_id, outcome.clone());
        outcome
    }

    #[must_use]
    pub fn snapshot(&self) -> Option<Snapshot> {
        self.document
            .as_ref()
            .map(|document| document.snapshot(self.generation))
    }

    /// Opens a new subscription generation and supersedes the previous one.
    ///
    /// # Errors
    ///
    /// Returns `subscription_generation_overflow` instead of panicking or wrapping.
    pub fn subscribe(&mut self) -> Result<SubscriptionGeneration, PortError> {
        self.subscriptions.open()
    }

    pub fn close_subscription(&mut self, generation: SubscriptionGeneration) {
        self.subscriptions.close(generation);
    }

    #[must_use]
    pub fn event_for(&self, generation: SubscriptionGeneration) -> Option<SnapshotEvent> {
        if !self.subscriptions.is_current(generation) {
            return None;
        }
        self.snapshot().map(|snapshot| SnapshotEvent {
            subscription_generation: generation,
            snapshot,
        })
    }

    fn apply(&mut self, expected_revision: Option<Revision>, command: Command) -> CommandOutcome {
        match command {
            Command::OpenDocument {
                document_id,
                content,
            } => {
                if self.document.is_some() {
                    return CommandOutcome::Failed(PortError::new("document_already_open", false));
                }
                if expected_revision.is_some() {
                    return CommandOutcome::Failed(PortError::new("unexpected_revision", false));
                }
                let document = Document {
                    id: document_id,
                    revision: 1,
                    status: DocumentStatus::Clean,
                    content,
                };
                let snapshot = document.snapshot(self.generation);
                self.document = Some(document);
                CommandOutcome::Completed(CommandResult::Snapshot(snapshot))
            }
            Command::ApplyEdit {
                document_id,
                start_byte,
                end_byte,
                replacement,
            } => {
                let Some(document) = self.document.as_mut() else {
                    return CommandOutcome::Failed(PortError::new("document_not_open", false));
                };
                if document.id != document_id {
                    return CommandOutcome::Failed(PortError::new("document_not_found", false));
                }
                if expected_revision != Some(document.revision) {
                    return CommandOutcome::Failed(PortError::new("stale_revision", false));
                }
                if start_byte > end_byte
                    || end_byte > document.content.len()
                    || !document.content.is_char_boundary(start_byte)
                    || !document.content.is_char_boundary(end_byte)
                {
                    return CommandOutcome::Failed(PortError::new("invalid_edit_range", false));
                }
                let Some(next_revision) = document.revision.checked_add(1) else {
                    return CommandOutcome::Failed(PortError::new("revision_overflow", false));
                };
                document
                    .content
                    .replace_range(start_byte..end_byte, &replacement);
                document.revision = next_revision;
                document.status = DocumentStatus::Dirty;
                CommandOutcome::Completed(CommandResult::Snapshot(
                    document.snapshot(self.generation),
                ))
            }
            Command::CloseDocument { document_id } => {
                let Some(document) = self.document.as_ref() else {
                    return CommandOutcome::Failed(PortError::new("document_not_open", false));
                };
                if document.id != document_id {
                    return CommandOutcome::Failed(PortError::new("document_not_found", false));
                }
                if expected_revision != Some(document.revision) {
                    return CommandOutcome::Failed(PortError::new("stale_revision", false));
                }
                self.document = None;
                CommandOutcome::Completed(CommandResult::Closed { document_id })
            }
        }
    }
}

pub type SubscriptionGeneration = u64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotEvent {
    pub subscription_generation: SubscriptionGeneration,
    pub snapshot: Snapshot,
}

#[derive(Debug, Default)]
struct SubscriptionRegistry {
    last_generation: SubscriptionGeneration,
    current: Option<SubscriptionGeneration>,
}

impl SubscriptionRegistry {
    fn open(&mut self) -> Result<SubscriptionGeneration, PortError> {
        self.last_generation = self
            .last_generation
            .checked_add(1)
            .ok_or_else(|| PortError::new("subscription_generation_overflow", false))?;
        self.current = Some(self.last_generation);
        Ok(self.last_generation)
    }

    fn close(&mut self, generation: SubscriptionGeneration) {
        if self.current == Some(generation) {
            self.current = None;
        }
    }

    fn close_all(&mut self) {
        self.current = None;
    }

    fn is_current(&self, generation: SubscriptionGeneration) -> bool {
        self.current == Some(generation)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Command, CommandEnvelope, CommandOutcome, CommandResult, DocumentStatus, StateEngine,
    };

    fn command(
        id: &str,
        generation: u64,
        revision: Option<u64>,
        command: Command,
    ) -> CommandEnvelope {
        CommandEnvelope {
            command_id: id.to_owned(),
            target_generation: generation,
            expected_revision: revision,
            command,
        }
    }

    fn open(engine: &mut StateEngine) {
        let outcome = engine.dispatch(command(
            "open",
            engine.generation(),
            None,
            Command::OpenDocument {
                document_id: "doc".to_owned(),
                content: "a😀b".to_owned(),
            },
        ));
        assert!(matches!(outcome, CommandOutcome::Completed(_)));
    }

    #[test]
    fn command_replay_returns_one_outcome_and_mutates_once() {
        let mut engine = StateEngine::new(7);
        open(&mut engine);
        let edit = command(
            "edit",
            7,
            Some(1),
            Command::ApplyEdit {
                document_id: "doc".to_owned(),
                start_byte: 0,
                end_byte: 1,
                replacement: "z".to_owned(),
            },
        );
        let first = engine.dispatch(edit.clone());
        let replay = engine.dispatch(edit);
        assert_eq!(first, replay);
        let snapshot = engine.snapshot().unwrap();
        assert_eq!(snapshot.revision, 2);
        assert_eq!(snapshot.content, "z😀b");
        assert_eq!(snapshot.status, DocumentStatus::Dirty);
    }

    #[test]
    fn cancellation_is_idempotent_and_cannot_undo_commit() {
        let mut engine = StateEngine::new(1);
        engine.cancel("open");
        engine.cancel("open");
        let cancelled = engine.dispatch(command(
            "open",
            1,
            None,
            Command::OpenDocument {
                document_id: "doc".to_owned(),
                content: String::new(),
            },
        ));
        assert_eq!(cancelled, CommandOutcome::Cancelled);
        assert!(engine.snapshot().is_none());

        let mut engine = StateEngine::new(1);
        open(&mut engine);
        engine.cancel("open");
        assert!(matches!(
            engine.dispatch(command(
                "open",
                1,
                None,
                Command::OpenDocument {
                    document_id: "different".to_owned(),
                    content: String::new(),
                },
            )),
            CommandOutcome::Completed(CommandResult::Snapshot(_))
        ));
        assert_eq!(engine.snapshot().unwrap().document_id, "doc");
    }

    #[test]
    fn stale_revision_and_invalid_unicode_boundary_leave_state_unchanged() {
        let mut engine = StateEngine::new(1);
        open(&mut engine);
        let before = engine.snapshot();

        let stale = engine.dispatch(command(
            "stale",
            1,
            Some(9),
            Command::ApplyEdit {
                document_id: "doc".to_owned(),
                start_byte: 0,
                end_byte: 1,
                replacement: String::new(),
            },
        ));
        assert_eq!(
            stale,
            CommandOutcome::Failed(crate::ports::PortError::new("stale_revision", false))
        );

        let invalid = engine.dispatch(command(
            "invalid",
            1,
            Some(1),
            Command::ApplyEdit {
                document_id: "doc".to_owned(),
                start_byte: 2,
                end_byte: 3,
                replacement: String::new(),
            },
        ));
        assert_eq!(
            invalid,
            CommandOutcome::Failed(crate::ports::PortError::new("invalid_edit_range", false))
        );
        assert_eq!(engine.snapshot(), before);
    }

    #[test]
    fn recreation_rejects_stale_commands_and_subscriptions() {
        let mut engine = StateEngine::new(4);
        open(&mut engine);
        let old_subscription = engine.subscribe().unwrap();
        assert!(engine.event_for(old_subscription).is_some());

        assert_eq!(engine.recreate(), Ok(5));
        assert!(engine.event_for(old_subscription).is_none());
        let stale = engine.dispatch(command(
            "old-generation",
            4,
            Some(1),
            Command::CloseDocument {
                document_id: "doc".to_owned(),
            },
        ));
        assert_eq!(
            stale,
            CommandOutcome::Failed(crate::ports::PortError::new("stale_generation", false))
        );
        assert!(engine.snapshot().is_some());

        let current = engine.subscribe().unwrap();
        assert!(current > old_subscription);
        assert_eq!(engine.event_for(current).unwrap().snapshot.generation, 5);
    }

    #[test]
    fn superseded_subscription_cannot_receive_callbacks() {
        let mut engine = StateEngine::new(1);
        open(&mut engine);
        let first = engine.subscribe().unwrap();
        let second = engine.subscribe().unwrap();
        assert!(engine.event_for(first).is_none());
        assert!(engine.event_for(second).is_some());
        engine.close_subscription(second);
        assert!(engine.event_for(second).is_none());
    }
}
