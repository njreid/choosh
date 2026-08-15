//! The per-workspace, in-memory agent-event spool `agent-events.md`'s
//! "Delivery and replay" section requires: every workspace-scoped
//! [`WireAgentEvent`] `serve.rs` forwards gets a monotonically increasing
//! per-workspace sequence number here ([`AgentEventSpool::record`]), and a
//! bounded retention window of recent events per workspace so a phone that
//! reconnects after a gap can ask "replay everything after sequence N"
//! ([`AgentEventSpool::resume`]) instead of either missing events silently
//! or replaying an unbounded backlog.
//!
//! **In-memory only, scoped to one `serve` process's lifetime** — a
//! restart loses every spooled event and resets sequence numbering to 1
//! for every workspace. That is a real, separately documented gap
//! (`PLAN.md`'s pre-existing "no persistence across a `serve` restart"
//! note), not something this module claims to solve; a devhost or Android
//! client observing a lower sequence than it last saw after a `serve`
//! restart is expected to fall back to `snapshot_required` (this module's
//! [`ResumeOutcome::SnapshotRequired`] covers this naturally: a
//! freshly-restarted spool has no record of the workspace at all, so any
//! `after_sequence` other than the "no prior ack" case is treated as
//! stale).
//!
//! **Bound reasoning** (`agent-events.md` asks for "a real, defensible
//! bound", not just *some* bound): the events this spool retains are
//! coarse lifecycle signals — `input_required`, `turn_completed`,
//! `files_changed`, `agent_status`, `editor_attached`/`detached` — not
//! high-frequency telemetry; even a genuinely busy workspace running
//! several concurrent agent/shell items is expected to emit on the order
//! of one event every few seconds at most, not per-byte or per-line
//! output (that streams over the separate `pty:<item_id>` tunnel, not
//! through this channel at all). [`MAX_EVENTS_PER_WORKSPACE`] (500) is
//! sized to comfortably outlast the reconnect scenarios `agent-events.md`
//! actually names — network loss, app backgrounding, a relay connection
//! cycling — which resolve in seconds to low minutes, not hours.
//! [`RETENTION`] (1 hour) is a second, independent ceiling on the same
//! intuition from the other direction: it bounds how long a *quiet*
//! workspace's spool holds onto a handful of old events waiting for a
//! reconnect that may never come, so memory use for an abandoned
//! workspace doesn't grow without bound over a multi-day `serve` uptime.
//! Either bound alone would be defensible; both together mean neither a
//! very chatty workspace nor a very long-idle one can make a single
//! workspace's retained set grow unboundedly.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use choosh_protocol::relay::{SequencedAgentEvent, WireAgentEvent};

/// See this module's doc comment for the reasoning behind both bounds.
pub const MAX_EVENTS_PER_WORKSPACE: usize = 500;
pub const RETENTION: Duration = Duration::from_hours(1);

struct RetainedEvent {
    sequence: u64,
    recorded_at: Instant,
    event: WireAgentEvent,
}

struct WorkspaceSpool {
    /// The sequence to assign to the *next* recorded event. Starts at 1 —
    /// real sequence numbers are always >= 1, so `next_sequence - 1` is a
    /// safe, always-non-negative "latest sequence assigned so far" even
    /// before this workspace has ever recorded an event (where it's 0).
    next_sequence: u64,
    events: VecDeque<RetainedEvent>,
}

impl WorkspaceSpool {
    fn new() -> Self {
        Self { next_sequence: 1, events: VecDeque::new() }
    }

    fn latest_sequence(&self) -> u64 {
        self.next_sequence - 1
    }

    /// The oldest sequence still retained, or `next_sequence` (one past the
    /// latest ever assigned) if nothing is currently retained — meaning
    /// "the whole retained window is empty; the only sequence that isn't
    /// stale is one that's already fully caught up." This is what makes
    /// [`AgentEventSpool::resume`]'s "already caught up" and "gap, need a
    /// snapshot" cases fall out of one comparison rather than needing a
    /// separate empty-deque special case.
    fn oldest_retained_sequence(&self) -> u64 {
        self.events.front().map_or(self.next_sequence, |event| event.sequence)
    }

    fn evict_expired(&mut self, now: Instant, bound: &SpoolBounds) {
        while let Some(front) = self.events.front() {
            if now.duration_since(front.recorded_at) > bound.retention {
                self.events.pop_front();
            } else {
                break;
            }
        }
        while self.events.len() > bound.max_events {
            self.events.pop_front();
        }
    }
}

/// The two independent, injectable bounds this spool evicts by — split out
/// of [`AgentEventSpool`] itself purely so tests can construct a spool with
/// a tiny `retention` (to prove time-based eviction without a real wall-clock
/// wait) or a tiny `max_events` (to prove count-based eviction) without
/// needing to wait out or spool half a thousand events for the production
/// defaults.
#[derive(Clone, Copy)]
struct SpoolBounds {
    max_events: usize,
    retention: Duration,
}

/// Outcome of [`AgentEventSpool::resume`]. Not a `Result` — `SnapshotRequired`
/// is an expected, correct answer per `agent-events.md`, not a failure this
/// module encountered.
#[derive(Debug, Eq, PartialEq)]
pub enum ResumeOutcome {
    /// Oldest first, strictly increasing, no gaps, no duplicates: every
    /// retained event with `sequence` strictly greater than the caller's
    /// `after_sequence`.
    Replay { events: Vec<SequencedAgentEvent>, latest_sequence: u64 },
    /// `after_sequence` is older than the retained window (or names a
    /// workspace this spool has no record of, other than "caller has no
    /// prior ack at all") — the caller MUST refresh via
    /// `workspace.status`/`workspace.list` rather than receive a
    /// silently-gapped replay.
    SnapshotRequired,
}

/// Per-workspace agent-event spool + sequencer, per this module's doc
/// comment. Cheaply cloneable-by-reference (`Arc`-wrap at the call site,
/// same as `RpcContext::registry`) — every method takes `&self` and locks
/// internally for the duration of one call, never across an `.await`.
pub struct AgentEventSpool {
    bounds: SpoolBounds,
    workspaces: Mutex<HashMap<String, WorkspaceSpool>>,
}

impl Default for AgentEventSpool {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentEventSpool {
    #[must_use]
    pub fn new() -> Self {
        Self::with_bounds(MAX_EVENTS_PER_WORKSPACE, RETENTION)
    }

    /// `pub(crate)` — only this module's own tests construct a spool with
    /// non-default bounds; production call sites (`serve.rs`) always want
    /// [`Self::new`]'s real, documented bounds.
    #[must_use]
    pub(crate) fn with_bounds(max_events: usize, retention: Duration) -> Self {
        Self { bounds: SpoolBounds { max_events, retention }, workspaces: Mutex::new(HashMap::new()) }
    }

    /// Records `event` into its workspace's spool and returns the sequence
    /// it was assigned, or `None` if `event.workspace_id()` is `None` (only
    /// [`WireAgentEvent::AuthRequired`] — not sequenced, not retained, per
    /// `WireAgentEvent::workspace_id`'s doc comment). Idempotent-adjacent
    /// caller contract: call this exactly once per event, at the single
    /// point `serve.rs` reads it off `agent_event_rx` — a second call for
    /// the same logical event would assign it a second, distinct sequence,
    /// since this module has no way to detect "this is the same event
    /// again."
    ///
    /// # Panics
    ///
    /// Never in practice — the lock is only ever held for a synchronous
    /// `HashMap`/`VecDeque` mutation with no `.await` point inside, so it
    /// cannot be poisoned by a panicking holder.
    pub fn record(&self, event: &WireAgentEvent) -> Option<u64> {
        let workspace_id = event.workspace_id()?;
        let now = Instant::now();
        let mut workspaces = self.workspaces.lock().unwrap();
        let spool = workspaces.entry(workspace_id.to_string()).or_insert_with(WorkspaceSpool::new);
        let sequence = spool.next_sequence;
        spool.next_sequence += 1;
        spool.events.push_back(RetainedEvent { sequence, recorded_at: now, event: event.clone() });
        spool.evict_expired(now, &self.bounds);
        Some(sequence)
    }

    /// Answers "replay `workspace_id`'s agent-event stream after
    /// `after_sequence`" per `agent-events.md`. `after_sequence: None` means
    /// the caller has no prior acknowledged sequence for this workspace at
    /// all (e.g. first subscribe) — answered with every event currently
    /// retained, not `SnapshotRequired` (there is no gap to detect when the
    /// caller was never caught up to begin with).
    ///
    /// # Panics
    ///
    /// Never in practice — same reasoning as [`Self::record`].
    #[must_use]
    pub fn resume(&self, workspace_id: &str, after_sequence: Option<u64>) -> ResumeOutcome {
        let now = Instant::now();
        let mut workspaces = self.workspaces.lock().unwrap();
        // Sequence numbers are assigned starting at 1, so no real event
        // ever has sequence 0 — `Some(0)` is therefore exactly as
        // meaningless a claim as `None`, and both get the same "no prior
        // ack" treatment for consistency with this method's own doc
        // comment.
        let no_prior_ack = matches!(after_sequence, None | Some(0));

        let Some(spool) = workspaces.get_mut(workspace_id) else {
            // No record of this workspace at all in this spool's lifetime
            // (never emitted an event, or `serve` restarted since). Only
            // "no prior ack" is answerable with an (empty) replay —
            // anything else claims knowledge of a sequence this spool
            // never assigned, which this process cannot honor.
            return if no_prior_ack {
                ResumeOutcome::Replay { events: Vec::new(), latest_sequence: 0 }
            } else {
                ResumeOutcome::SnapshotRequired
            };
        };
        spool.evict_expired(now, &self.bounds);
        let latest = spool.latest_sequence();

        if no_prior_ack {
            // By definition, a caller with no prior acknowledged sequence
            // has never missed anything it once had — there is no gap to
            // detect, even if part of this workspace's history predates
            // this spool or has since aged out. Answer with whatever is
            // currently retained, oldest first, and let the caller treat it
            // as its starting baseline.
            let events = spool
                .events
                .iter()
                .map(|retained| SequencedAgentEvent { sequence: retained.sequence, event: retained.event.clone() })
                .collect();
            return ResumeOutcome::Replay { events, latest_sequence: latest };
        }

        let after = after_sequence.expect("no_prior_ack is false, so after_sequence must be Some");
        if after >= latest {
            // Already caught up (or ahead, which shouldn't happen for a
            // well-behaved caller, but is safe to treat the same way: there
            // is nothing newer to replay).
            return ResumeOutcome::Replay { events: Vec::new(), latest_sequence: latest };
        }
        let oldest_retained = spool.oldest_retained_sequence();
        if after + 1 < oldest_retained {
            // The first event the caller still needs (after + 1) has
            // already been evicted — replaying from here on would silently
            // skip it.
            return ResumeOutcome::SnapshotRequired;
        }
        let events = spool
            .events
            .iter()
            .filter(|retained| retained.sequence > after)
            .map(|retained| SequencedAgentEvent { sequence: retained.sequence, event: retained.event.clone() })
            .collect();
        ResumeOutcome::Replay { events, latest_sequence: latest }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use choosh_protocol::relay::{WireAgentStatus, WireAuthProvider};

    fn status_event(workspace_id: &str, status: WireAgentStatus) -> WireAgentEvent {
        WireAgentEvent::AgentStatus { workspace_id: workspace_id.to_string(), item_id: "item-1".to_string(), status }
    }

    fn auth_required_event() -> WireAgentEvent {
        WireAgentEvent::AuthRequired {
            provider: WireAuthProvider::Github,
            user_code: "WDJB-MJHT".to_string(),
            verification_uri: "https://example.com/device".to_string(),
        }
    }

    #[test]
    fn events_for_one_workspace_get_monotonically_increasing_sequence_numbers() {
        let spool = AgentEventSpool::new();
        let sequences: Vec<u64> = (0..5)
            .map(|_| spool.record(&status_event("ws-1", WireAgentStatus::Busy)).expect("workspace-scoped event"))
            .collect();
        assert_eq!(sequences, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn sequence_numbers_are_independent_per_workspace() {
        let spool = AgentEventSpool::new();
        let first_in_alpha = spool.record(&status_event("ws-a", WireAgentStatus::Busy)).unwrap();
        let first_in_bravo = spool.record(&status_event("ws-b", WireAgentStatus::Busy)).unwrap();
        let second_in_alpha = spool.record(&status_event("ws-a", WireAgentStatus::Waiting)).unwrap();
        let second_in_bravo = spool.record(&status_event("ws-b", WireAgentStatus::Waiting)).unwrap();
        assert_eq!((first_in_alpha, second_in_alpha), (1, 2));
        assert_eq!((first_in_bravo, second_in_bravo), (1, 2));
    }

    #[test]
    fn auth_required_events_are_not_sequenced_or_recorded() {
        let spool = AgentEventSpool::new();
        assert_eq!(spool.record(&auth_required_event()), None);
        // Confirm nothing was recorded under any key: a subsequent
        // workspace-scoped event still starts at sequence 1, proving the
        // `AuthRequired` call above didn't silently consume a sequence
        // number or create a phantom workspace entry.
        let sequence = spool.record(&status_event("ws-1", WireAgentStatus::Busy)).unwrap();
        assert_eq!(sequence, 1);
    }

    #[test]
    fn resume_replays_exactly_the_missed_events_in_order_no_duplicates_no_gaps() {
        let spool = AgentEventSpool::new();
        for status in [WireAgentStatus::Starting, WireAgentStatus::Busy, WireAgentStatus::Waiting, WireAgentStatus::Stopped] {
            spool.record(&status_event("ws-1", status)).unwrap();
        }
        // Caller last acknowledged sequence 1 (Starting) — expects Busy,
        // Waiting, Stopped (sequences 2, 3, 4), in that order.
        let ResumeOutcome::Replay { events, latest_sequence } = spool.resume("ws-1", Some(1)) else {
            panic!("expected a replay, not snapshot_required")
        };
        assert_eq!(latest_sequence, 4);
        let sequences: Vec<u64> = events.iter().map(|event| event.sequence).collect();
        assert_eq!(sequences, vec![2, 3, 4]);
        assert_eq!(events[0].event, status_event("ws-1", WireAgentStatus::Busy));
        assert_eq!(events[1].event, status_event("ws-1", WireAgentStatus::Waiting));
        assert_eq!(events[2].event, status_event("ws-1", WireAgentStatus::Stopped));
    }

    #[test]
    fn resume_with_no_prior_ack_replays_every_currently_retained_event() {
        let spool = AgentEventSpool::new();
        spool.record(&status_event("ws-1", WireAgentStatus::Starting)).unwrap();
        spool.record(&status_event("ws-1", WireAgentStatus::Busy)).unwrap();
        let ResumeOutcome::Replay { events, latest_sequence } = spool.resume("ws-1", None) else {
            panic!("expected a replay, not snapshot_required")
        };
        assert_eq!(latest_sequence, 2);
        assert_eq!(events.iter().map(|event| event.sequence).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn resume_with_no_prior_ack_never_requires_a_snapshot_even_after_earlier_history_was_evicted() {
        // A caller with no prior acknowledged sequence has by definition
        // never missed anything it once had — evicted history from before
        // it ever subscribed is not a gap for it, it's just history it was
        // never expecting. max_events = 2 forces sequences 1 and 2 out
        // before this workspace's very first `resume(None)` call below.
        let spool = AgentEventSpool::with_bounds(2, RETENTION);
        for status in [WireAgentStatus::Starting, WireAgentStatus::Busy, WireAgentStatus::Waiting, WireAgentStatus::Stopped] {
            spool.record(&status_event("ws-1", status)).unwrap();
        }
        let ResumeOutcome::Replay { events, latest_sequence } = spool.resume("ws-1", None) else {
            panic!("a first-ever subscribe must never require a snapshot, regardless of prior eviction")
        };
        assert_eq!(latest_sequence, 4);
        assert_eq!(events.iter().map(|event| event.sequence).collect::<Vec<_>>(), vec![3, 4]);
        // `Some(0)` is documented to behave identically to `None`.
        assert_eq!(spool.resume("ws-1", Some(0)), ResumeOutcome::Replay { events, latest_sequence });
    }

    #[test]
    fn resume_already_caught_up_replays_nothing_but_is_not_snapshot_required() {
        let spool = AgentEventSpool::new();
        spool.record(&status_event("ws-1", WireAgentStatus::Starting)).unwrap();
        let ResumeOutcome::Replay { events, latest_sequence } = spool.resume("ws-1", Some(1)) else {
            panic!("already-caught-up must be a (possibly empty) replay, not snapshot_required")
        };
        assert!(events.is_empty());
        assert_eq!(latest_sequence, 1);
    }

    #[test]
    fn resume_for_a_never_seen_workspace_with_no_prior_ack_replays_nothing() {
        let spool = AgentEventSpool::new();
        let ResumeOutcome::Replay { events, latest_sequence } = spool.resume("ws-never-seen", None) else {
            panic!("a first subscribe to an unknown-but-legitimate workspace must not be treated as stale")
        };
        assert!(events.is_empty());
        assert_eq!(latest_sequence, 0);
    }

    #[test]
    fn resume_for_a_never_seen_workspace_with_a_prior_ack_requires_a_snapshot() {
        let spool = AgentEventSpool::new();
        assert_eq!(spool.resume("ws-never-seen", Some(5)), ResumeOutcome::SnapshotRequired);
    }

    #[test]
    fn resume_from_a_sequence_older_than_the_retained_window_requires_a_snapshot() {
        // max_events = 3: after recording 5 events, only sequences 3..=5
        // are retained.
        let spool = AgentEventSpool::with_bounds(3, RETENTION);
        for status in [
            WireAgentStatus::Starting,
            WireAgentStatus::Busy,
            WireAgentStatus::Waiting,
            WireAgentStatus::Stopped,
            WireAgentStatus::Failed,
        ] {
            spool.record(&status_event("ws-1", status)).unwrap();
        }
        // The caller's next-needed event after `Some(1)` is sequence 2,
        // which has been evicted — a real gap, not answerable.
        assert_eq!(spool.resume("ws-1", Some(1)), ResumeOutcome::SnapshotRequired);
        // The exact boundary: after `Some(2)`, the next-needed event is
        // sequence 3, which is still retained — no gap, so this MUST be a
        // real replay, not a false-positive snapshot_required.
        let ResumeOutcome::Replay { events, latest_sequence } = spool.resume("ws-1", Some(2)) else {
            panic!("sequence 3 (the next-needed event) is still retained — this must replay, not require a snapshot")
        };
        assert_eq!(latest_sequence, 5);
        assert_eq!(events.iter().map(|event| event.sequence).collect::<Vec<_>>(), vec![3, 4, 5]);
    }

    #[test]
    fn spool_eviction_by_count_drops_the_oldest_events_and_a_request_for_them_gets_snapshot_required() {
        let spool = AgentEventSpool::with_bounds(3, RETENTION);
        for i in 0..5u32 {
            spool.record(&status_event("ws-1", if i % 2 == 0 { WireAgentStatus::Busy } else { WireAgentStatus::Waiting })).unwrap();
        }
        // Sequences 1 and 2 must be gone; 3, 4, 5 retained. The
        // next-needed event after `Some(1)` is sequence 2 — evicted.
        assert_eq!(spool.resume("ws-1", Some(1)), ResumeOutcome::SnapshotRequired);
        let ResumeOutcome::Replay { events, latest_sequence } = spool.resume("ws-1", Some(3)) else {
            panic!("sequence 3 is exactly the boundary — the next needed event (4) is still retained, so this must replay")
        };
        assert_eq!(latest_sequence, 5);
        assert_eq!(events.iter().map(|event| event.sequence).collect::<Vec<_>>(), vec![4, 5]);
    }

    #[test]
    fn spool_eviction_by_age_drops_expired_events_and_a_request_for_them_gets_snapshot_required() {
        let retention = Duration::from_millis(30);
        let spool = AgentEventSpool::with_bounds(MAX_EVENTS_PER_WORKSPACE, retention);
        // Two events recorded before the retention window elapses — both
        // will be expired together by the time the third is recorded.
        spool.record(&status_event("ws-1", WireAgentStatus::Starting)).unwrap(); // sequence 1
        spool.record(&status_event("ws-1", WireAgentStatus::Busy)).unwrap(); // sequence 2
        std::thread::sleep(retention + Duration::from_millis(40));
        // A fresh event for the same workspace triggers eviction of the
        // now-expired first two (eviction runs on every `record`, not just
        // on a background timer).
        spool.record(&status_event("ws-1", WireAgentStatus::Waiting)).unwrap(); // sequence 3
        // The next-needed event after `Some(1)` is sequence 2 — expired.
        assert_eq!(spool.resume("ws-1", Some(1)), ResumeOutcome::SnapshotRequired);
        // The next-needed event after `Some(2)` is sequence 3 — still
        // fresh (recorded after the sleep), so this must replay.
        let ResumeOutcome::Replay { events, .. } = spool.resume("ws-1", Some(2)) else {
            panic!("sequence 3 is still fresh and must still be retained")
        };
        assert_eq!(events.iter().map(|event| event.sequence).collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn record_and_resume_from_many_threads_never_produces_a_duplicate_or_out_of_order_sequence() {
        // Not a strict requirement of the spec, but `serve.rs` threads this
        // spool through both `serve_dispatch`'s single event-forwarding
        // loop and (independently) `handle_tunnel_frame`'s resume-request
        // handling, which can run concurrently with a live event being
        // recorded — this proves the shared `Mutex` actually serializes
        // `record` calls rather than racing.
        use std::sync::Arc;
        let spool = Arc::new(AgentEventSpool::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let spool = spool.clone();
            handles.push(std::thread::spawn(move || {
                (0..50).map(|_| spool.record(&status_event("ws-1", WireAgentStatus::Busy)).unwrap()).collect::<Vec<_>>()
            }));
        }
        let mut all_sequences: Vec<u64> = handles.into_iter().flat_map(|handle| handle.join().unwrap()).collect();
        all_sequences.sort_unstable();
        let expected: Vec<u64> = (1..=400).collect();
        assert_eq!(all_sequences, expected, "every sequence 1..=400 must appear exactly once, with no gaps or duplicates");
    }
}
