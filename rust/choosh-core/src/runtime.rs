//! Bounded thread-backed adapter around the deterministic state-engine model.

use crate::actor::{
    CommandEnvelope, CommandOutcome, Generation, Snapshot, SnapshotEvent, StateEngine,
    SubscriptionGeneration,
};
use crate::ports::{PortError, PortResult};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};

/// Instrumentation seam for deterministic scheduling and diagnostics.
/// Production composition roots normally use [`NoopRuntimeHooks`].
pub trait RuntimeHooks: Send + Sync {
    fn before_message(&self);
}

#[derive(Debug, Default)]
pub struct NoopRuntimeHooks;

impl RuntimeHooks for NoopRuntimeHooks {
    fn before_message(&self) {}
}

/// A single typed response from the actor. Dropping a reply never stops the actor.
#[derive(Debug)]
pub struct ActorReply<T> {
    receiver: Receiver<T>,
}

impl<T> ActorReply<T> {
    /// Waits for the already accepted actor operation to finish.
    ///
    /// # Errors
    ///
    /// Returns `actor_response_closed` if the actor exits without responding.
    pub fn receive(self) -> PortResult<T> {
        self.receiver
            .recv()
            .map_err(|_| PortError::new("actor_response_closed", true))
    }
}

#[derive(Clone)]
pub struct ActorHandle {
    sender: SyncSender<Message>,
}

impl ActorHandle {
    /// Enqueues one command without waiting for its outcome.
    ///
    /// # Errors
    ///
    /// Returns `actor_queue_full` when bounded capacity is exhausted, or
    /// `actor_closed` after shutdown. Rejection never partially applies a command.
    pub fn dispatch(&self, envelope: CommandEnvelope) -> PortResult<ActorReply<CommandOutcome>> {
        let (reply, receiver) = mpsc::channel();
        self.enqueue(Message::Dispatch { envelope, reply })?;
        Ok(ActorReply { receiver })
    }

    /// Enqueues an idempotent cancellation request.
    ///
    /// # Errors
    ///
    /// Returns a bounded-queue or closed-actor error before cancellation is accepted.
    pub fn cancel(&self, command_id: String) -> PortResult<ActorReply<()>> {
        let (reply, receiver) = mpsc::channel();
        self.enqueue(Message::Cancel { command_id, reply })?;
        Ok(ActorReply { receiver })
    }

    /// Requests the latest immutable snapshot.
    ///
    /// # Errors
    ///
    /// Returns a bounded-queue or closed-actor error before the request is accepted.
    pub fn snapshot(&self) -> PortResult<ActorReply<Option<Snapshot>>> {
        let (reply, receiver) = mpsc::channel();
        self.enqueue(Message::Snapshot { reply })?;
        Ok(ActorReply { receiver })
    }

    /// Opens a subscription generation, superseding the previous generation.
    ///
    /// # Errors
    ///
    /// Returns a bounded-queue or closed-actor error before the request is accepted.
    pub fn subscribe(&self) -> PortResult<ActorReply<PortResult<SubscriptionGeneration>>> {
        let (reply, receiver) = mpsc::channel();
        self.enqueue(Message::Subscribe { reply })?;
        Ok(ActorReply { receiver })
    }

    /// Requests a snapshot event only if the subscription is still current.
    ///
    /// # Errors
    ///
    /// Returns a bounded-queue or closed-actor error before the request is accepted.
    pub fn event_for(
        &self,
        generation: SubscriptionGeneration,
    ) -> PortResult<ActorReply<Option<SnapshotEvent>>> {
        let (reply, receiver) = mpsc::channel();
        self.enqueue(Message::EventFor { generation, reply })?;
        Ok(ActorReply { receiver })
    }

    /// Advances the engine generation and closes existing subscriptions.
    ///
    /// # Errors
    ///
    /// Returns a bounded-queue or closed-actor error before the request is accepted.
    pub fn recreate(&self) -> PortResult<ActorReply<PortResult<Generation>>> {
        let (reply, receiver) = mpsc::channel();
        self.enqueue(Message::Recreate { reply })?;
        Ok(ActorReply { receiver })
    }

    fn enqueue(&self, message: Message) -> PortResult<()> {
        self.sender.try_send(message).map_err(|error| match error {
            TrySendError::Full(_) => PortError::new("actor_queue_full", true),
            TrySendError::Disconnected(_) => PortError::new("actor_closed", false),
        })
    }
}

pub struct ActorRuntime {
    handle: ActorHandle,
    worker: Option<JoinHandle<()>>,
}

impl ActorRuntime {
    /// Starts an actor with a strictly positive bounded queue.
    ///
    /// # Errors
    ///
    /// Returns `invalid_actor_capacity` for zero capacity or `actor_spawn_failed`
    /// if the operating system cannot create the worker thread.
    pub fn spawn(engine: StateEngine, capacity: usize) -> PortResult<Self> {
        Self::spawn_with_hooks(engine, capacity, Arc::new(NoopRuntimeHooks))
    }

    /// Starts an actor with injectable scheduling hooks for deterministic tests.
    ///
    /// # Errors
    ///
    /// Returns `invalid_actor_capacity` for zero capacity or `actor_spawn_failed`
    /// if the operating system cannot create the worker thread.
    pub fn spawn_with_hooks(
        engine: StateEngine,
        capacity: usize,
        hooks: Arc<dyn RuntimeHooks>,
    ) -> PortResult<Self> {
        if capacity == 0 {
            return Err(PortError::new("invalid_actor_capacity", false));
        }
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let worker = thread::Builder::new()
            .name("choosh-state-actor".to_owned())
            .spawn(move || run(engine, &receiver, hooks.as_ref()))
            .map_err(|_| PortError::new("actor_spawn_failed", true))?;
        Ok(Self {
            handle: ActorHandle { sender },
            worker: Some(worker),
        })
    }

    #[must_use]
    pub fn handle(&self) -> ActorHandle {
        self.handle.clone()
    }

    /// Drains accepted work, stops the worker, and waits for termination.
    ///
    /// # Errors
    ///
    /// Returns `actor_closed` if the worker already exited or `actor_panicked` if
    /// the worker panicked. Caller-held handle clones become closed after success.
    pub fn shutdown(mut self) -> PortResult<()> {
        self.handle
            .sender
            .send(Message::Shutdown)
            .map_err(|_| PortError::new("actor_closed", false))?;
        let Some(worker) = self.worker.take() else {
            return Err(PortError::new("actor_closed", false));
        };
        worker
            .join()
            .map_err(|_| PortError::new("actor_panicked", false))
    }
}

enum Message {
    Dispatch {
        envelope: CommandEnvelope,
        reply: mpsc::Sender<CommandOutcome>,
    },
    Cancel {
        command_id: String,
        reply: mpsc::Sender<()>,
    },
    Snapshot {
        reply: mpsc::Sender<Option<Snapshot>>,
    },
    Subscribe {
        reply: mpsc::Sender<PortResult<SubscriptionGeneration>>,
    },
    EventFor {
        generation: SubscriptionGeneration,
        reply: mpsc::Sender<Option<SnapshotEvent>>,
    },
    Recreate {
        reply: mpsc::Sender<PortResult<Generation>>,
    },
    Shutdown,
}

fn run(mut engine: StateEngine, receiver: &Receiver<Message>, hooks: &dyn RuntimeHooks) {
    while let Ok(message) = receiver.recv() {
        hooks.before_message();
        match message {
            Message::Dispatch { envelope, reply } => {
                let _ignored = reply.send(engine.dispatch(envelope));
            }
            Message::Cancel { command_id, reply } => {
                engine.cancel(&command_id);
                let _ignored = reply.send(());
            }
            Message::Snapshot { reply } => {
                let _ignored = reply.send(engine.snapshot());
            }
            Message::Subscribe { reply } => {
                let _ignored = reply.send(engine.subscribe());
            }
            Message::EventFor { generation, reply } => {
                let _ignored = reply.send(engine.event_for(generation));
            }
            Message::Recreate { reply } => {
                let _ignored = reply.send(engine.recreate());
            }
            Message::Shutdown => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ActorRuntime, RuntimeHooks};
    use crate::actor::{Command, CommandEnvelope, CommandOutcome, StateEngine};
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;

    fn open(command_id: &str) -> CommandEnvelope {
        CommandEnvelope {
            command_id: command_id.to_owned(),
            target_generation: 1,
            expected_revision: None,
            command: Command::OpenDocument {
                document_id: "doc".to_owned(),
                content: "fixture".to_owned(),
            },
        }
    }

    #[test]
    fn concurrent_replay_mutates_once_and_returns_one_cached_outcome() {
        let runtime = ActorRuntime::spawn(StateEngine::new(1), 32).unwrap();
        let handle = runtime.handle();
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let handle = handle.clone();
                thread::spawn(move || {
                    handle
                        .dispatch(open("same-command"))
                        .unwrap()
                        .receive()
                        .unwrap()
                })
            })
            .collect();
        let outcomes: Vec<_> = threads
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert!(outcomes.windows(2).all(|pair| pair[0] == pair[1]));
        assert!(matches!(outcomes[0], CommandOutcome::Completed(_)));
        assert_eq!(
            handle
                .snapshot()
                .unwrap()
                .receive()
                .unwrap()
                .unwrap()
                .revision,
            1
        );
        runtime.shutdown().unwrap();
        assert_eq!(handle.snapshot().unwrap_err().code, "actor_closed");
    }

    #[derive(Debug, Default)]
    struct Gate {
        state: Mutex<(bool, bool)>,
        changed: Condvar,
    }

    impl Gate {
        fn wait_until_entered(&self) {
            let state = self.state.lock().unwrap();
            drop(
                self.changed
                    .wait_while(state, |(entered, _released)| !*entered)
                    .unwrap(),
            );
        }

        fn release(&self) {
            let mut state = self.state.lock().unwrap();
            state.1 = true;
            self.changed.notify_all();
        }
    }

    impl RuntimeHooks for Gate {
        fn before_message(&self) {
            let mut state = self.state.lock().unwrap();
            state.0 = true;
            self.changed.notify_all();
            drop(
                self.changed
                    .wait_while(state, |(_entered, released)| !*released)
                    .unwrap(),
            );
        }
    }

    #[test]
    fn full_queue_rejects_without_partial_application() {
        let gate = Arc::new(Gate::default());
        let runtime = ActorRuntime::spawn_with_hooks(StateEngine::new(1), 1, gate.clone()).unwrap();
        let handle = runtime.handle();
        let first = handle.dispatch(open("first")).unwrap();
        gate.wait_until_entered();
        let second = handle.dispatch(open("second")).unwrap();
        assert_eq!(
            handle.dispatch(open("rejected")).unwrap_err().code,
            "actor_queue_full"
        );
        gate.release();
        assert!(matches!(
            first.receive().unwrap(),
            CommandOutcome::Completed(_)
        ));
        assert!(matches!(
            second.receive().unwrap(),
            CommandOutcome::Failed(_)
        ));
        assert_eq!(
            handle
                .snapshot()
                .unwrap()
                .receive()
                .unwrap()
                .unwrap()
                .document_id,
            "doc"
        );
        runtime.shutdown().unwrap();
    }

    #[test]
    fn recreation_invalidates_subscription_through_runtime() {
        let runtime = ActorRuntime::spawn(StateEngine::new(1), 4).unwrap();
        let handle = runtime.handle();
        handle.dispatch(open("open")).unwrap().receive().unwrap();
        let subscription = handle.subscribe().unwrap().receive().unwrap().unwrap();
        assert!(
            handle
                .event_for(subscription)
                .unwrap()
                .receive()
                .unwrap()
                .is_some()
        );
        assert_eq!(handle.recreate().unwrap().receive().unwrap(), Ok(2));
        assert!(
            handle
                .event_for(subscription)
                .unwrap()
                .receive()
                .unwrap()
                .is_none()
        );
        runtime.shutdown().unwrap();
    }
}
