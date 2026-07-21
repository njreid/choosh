//! Headless acceptance evidence for the reconnect contract.

use choosh_core::{
    connection::{
        ChannelFailure, ConnectionFailure, ConnectionMachine, ConnectionState, RetryPolicy,
    },
    reconnect_recovery::{Component, ComponentCheckpoint, RecoveryAction, RecoveryCoordinator},
};

fn authenticate(machine: &mut ConnectionMachine) -> u64 {
    machine.connect().unwrap();
    machine.host_key_trusted().unwrap();
    machine.authenticated().unwrap()
}

#[test]
fn reconnect_fails_closed_until_exact_admission_then_recovers_components() {
    let mut connection = ConnectionMachine::new(RetryPolicy::new(2));
    let old_generation = authenticate(&mut connection);

    connection.transport_lost(1_000).unwrap();
    assert_eq!(
        connection.validate_channel(old_generation),
        Err(ChannelFailure::NotReady)
    );
    assert!(!connection.retry_due(999));
    assert!(connection.retry_due(1_000));

    // A reconnect cannot convert a changed key into a first-trust prompt. The
    // machine remains outside Authenticating, so an outer composition root has
    // no signer-admission transition to invoke.
    assert_eq!(
        connection.host_key_unknown("SHA256:changed".into()),
        Err(ConnectionFailure::InvalidTransition)
    );
    assert_eq!(connection.state(), &ConnectionState::VerifyingHostKey);
    assert_eq!(
        connection.authenticated(),
        Err(ConnectionFailure::InvalidTransition)
    );

    // The adapter reports the changed pinned key as terminal; it must not be
    // retried as a transport failure.
    connection.host_key_mismatch().unwrap();
    assert_eq!(
        connection.state(),
        &ConnectionState::Failed(ConnectionFailure::HostKeyMismatch)
    );

    let mut restored = ConnectionMachine::new(RetryPolicy::new(2));
    let first = authenticate(&mut restored);
    restored.transport_lost(20).unwrap();
    assert!(restored.retry_due(20));
    restored.host_key_trusted().unwrap();
    let current = restored.authenticated().unwrap();
    assert_eq!(current, first + 1);
    assert_eq!(
        restored.validate_channel(first),
        Err(ChannelFailure::StaleGeneration)
    );
    assert_eq!(restored.validate_channel(current), Ok(()));

    let mut recovery = RecoveryCoordinator::new(2).unwrap();
    let (_, actions) = recovery
        .begin([
            ComponentCheckpoint {
                component: Component::Workspace,
                local_revision: 7,
                remote_revision: 9,
                oldest_replay_revision: 8,
            },
            ComponentCheckpoint {
                component: Component::Items,
                local_revision: 2,
                remote_revision: 9,
                oldest_replay_revision: 5,
            },
            ComponentCheckpoint {
                component: Component::EventSpool,
                local_revision: 4,
                remote_revision: 4,
                oldest_replay_revision: 1,
            },
            ComponentCheckpoint {
                component: Component::Pins,
                local_revision: 3,
                remote_revision: 3,
                oldest_replay_revision: 1,
            },
        ])
        .unwrap();
    assert_eq!(
        actions,
        [
            RecoveryAction::Replay {
                component: Component::Workspace,
                after_revision: 7,
            },
            RecoveryAction::Snapshot {
                component: Component::Items,
                expected_revision: 9,
            },
            RecoveryAction::Current {
                component: Component::EventSpool,
            },
            RecoveryAction::Current {
                component: Component::Pins,
            },
        ]
    );
}
