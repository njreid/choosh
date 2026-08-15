package ai.choosh.agentevents

import ai.choosh.engine.AgentEventPush
import ai.choosh.engine.AgentEventsResumeOutcome
import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.workspaceIdOrNull
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * Owns the whole client-side half of docs/specs/agent-events.md's "Delivery
 * and replay" contract — the piece that was, until this pass, entirely
 * absent from the Android app (only `ai.choosh.notifications`'s FCM
 * best-effort backstop existed; see that package's own doc comments for
 * why it's explicitly a *separate*, best-effort path, not this one).
 *
 * For every workspace the app currently cares about (see [subscribe]),
 * this class issues an `AgentEventsResumeRequest` at (re)subscribe time,
 * applies the replayed events in order, then keeps draining
 * [ChooshEngine.pollAgentEvents] on an interval for live pushes —
 * advancing [AgentEventCursorStore] and [AgentAttentionTracker] identically
 * whether an event arrived replayed or live, and invoking
 * [onSnapshotRequired] (never silently doing nothing) whenever a resume
 * comes back `SnapshotRequired`.
 *
 * ## Design choices
 *
 * **One subscription instance per process, covering every workspace the
 * app has ever [subscribe]d to — not one instance per open workspace
 * screen.** Live `agent-event` pushes arrive unprompted on the phone's
 * *ordinary* control connection the moment `relayd` forwards them
 * (`choosh_protocol::relay::ServerPush::AgentEvent`'s own doc comment:
 * forwarded "to the owning phone Identity when a devhost sends
 * `agent-event` and that phone is currently connected" — there is no
 * relay-level "subscribe to this workspace" step at all). The only thing
 * genuinely scoped per-workspace is the *resume* request, and a workspace
 * only ever needs one outstanding resume/cursor regardless of how many
 * screens currently show it — a single coordinator polling one shared
 * queue and fanning events out is simpler, and cannot double-apply or
 * double-poll the way one instance per screen could if the same workspace
 * happened to be open twice.
 *
 * **Polling at the JNI boundary, not a callback.** See
 * `rust/choosh-android-bridge/src/engine.rs`'s `Engine::poll_agent_events`
 * doc comment for the fuller rationale: this crate has no existing "Rust
 * calls back into Kotlin unprompted" mechanism for any other server-pushed
 * stream (PTY output renders entirely inside Rust via `wgpu`, never
 * reaching Kotlin as structured data at all) — a polling queue, the same
 * shape every other native method already uses (`itemList`,
 * `workspaceStatus`, ...), is the one that fits this codebase's existing
 * pattern rather than inventing a second, novel cross-JNI delivery
 * mechanism for this one feature.
 *
 * Not itself a [androidx.lifecycle.ViewModel] — the composition root owns
 * one instance for the process lifetime (parallel to how it owns a single
 * [ChooshEngine]), and hands it to whichever ViewModels want to
 * [subscribe]/observe [AgentAttentionTracker.needsAttention].
 */
class AgentEventSubscription(
    private val engine: ChooshEngine,
    private val cursorStore: AgentEventCursorStore,
    private val attentionTracker: AgentAttentionTracker,
    /**
     * Invoked (from whatever dispatcher [pollOnce]/[subscribe] happen to
     * run on — callers needing a specific dispatcher should hop inside
     * their own implementation) whenever a workspace's resume comes back
     * `SnapshotRequired`. Per agent-events.md: "the client refreshes full
     * workspace/item state via `workspace.status`/`workspace.list`" — the
     * composition root wires this to whatever ViewModel already owns that
     * refresh for the given workspace (e.g.
     * [ai.choosh.explorer.ExplorerViewModel.refresh] /
     * [ai.choosh.fleet.FleetViewModel.refresh]), rather than this class
     * inventing a second refresh path.
     */
    private val onSnapshotRequired: suspend (deviceId: String, workspaceId: String) -> Unit,
    private val pollIntervalMs: Long = DEFAULT_POLL_INTERVAL_MS,
) {
    /** `workspaceId -> deviceId` for every workspace [subscribe] has registered — re-resumed by [resubscribeAll]. */
    private val subscribedWorkspaces = mutableMapOf<String, String>()
    private var pollJob: Job? = null

    /**
     * Registers `workspaceId`/`deviceId` as one this subscription should
     * keep caught up, and immediately issues its first resume request
     * (from [AgentEventCursorStore.lastAcknowledged] — `null` on a genuine
     * first subscribe). Idempotent: calling this again for an
     * already-subscribed workspace just re-resumes it, which is exactly
     * what a workspace screen re-entering the foreground wants — a resume
     * from an already-caught-up cursor simply replays nothing, per
     * `agent-events.md`.
     */
    suspend fun subscribe(deviceId: String, workspaceId: String) {
        subscribedWorkspaces[workspaceId] = deviceId
        resume(deviceId, workspaceId)
    }

    /** Stops treating `workspaceId` as one this subscription keeps caught up — its cursor is left as-is, not reset, so a later re-[subscribe] resumes correctly rather than replaying everything again. */
    fun unsubscribe(workspaceId: String) {
        subscribedWorkspaces.remove(workspaceId)
    }

    /**
     * Re-issues a resume request for every currently-subscribed workspace —
     * call this after a fresh [ChooshEngine.connect] succeeds (a
     * reconnect), per `agent-events.md`: "A reconnect after any gap ...
     * network loss, app backgrounding, or the relay connection itself
     * cycling ... MUST resume from the last acknowledged sequence or fall
     * back to `snapshot_required`; it MUST NOT silently drop events."
     */
    suspend fun resubscribeAll() {
        subscribedWorkspaces.toMap().forEach { (workspaceId, deviceId) -> resume(deviceId, workspaceId) }
    }

    private suspend fun resume(deviceId: String, workspaceId: String) {
        // A transport failure here (not connected, relay round-trip
        // failed) is recoverable by the next resubscribeAll/poll cycle —
        // deliberately swallowed rather than thrown, so one workspace's
        // momentary offline-ness never aborts subscribing every other
        // workspace in the same batch (resubscribeAll's forEach).
        val outcome = runCatching { engine.agentEventsResume(deviceId, workspaceId, cursorStore.lastAcknowledged(workspaceId)) }
            .getOrNull() ?: return
        when (outcome) {
            is AgentEventsResumeOutcome.Replayed -> {
                // Oldest-first application, per agent-events.md — the same
                // order the wire type itself documents and this app must
                // preserve, not re-sort.
                outcome.events.forEach { sequenced -> attentionTracker.apply(sequenced.event) }
                // Advances the cursor to `latestSequence` even when
                // `events` is empty (already caught up) — per
                // `AgentEventsResumeResponse::Replayed`'s own doc comment,
                // this lets the cursor move forward without having
                // received a new event, matching what the wire response
                // itself is documented to carry `latest_sequence` for.
                cursorStore.acknowledge(workspaceId, outcome.latestSequence)
            }
            AgentEventsResumeOutcome.SnapshotRequired -> {
                cursorStore.reset(workspaceId)
                onSnapshotRequired(deviceId, workspaceId)
            }
        }
    }

    private fun applyPush(push: AgentEventPush) {
        attentionTracker.apply(push.event)
        val workspaceId = push.event.workspaceIdOrNull ?: return
        val sequence = push.sequence ?: return
        cursorStore.acknowledge(workspaceId, sequence)
    }

    /**
     * Drains [ChooshEngine.pollAgentEvents] exactly once and applies every
     * returned push, oldest first (the order [ChooshEngine.pollAgentEvents]
     * itself documents returning). Public — and deliberately separate from
     * [startPolling]'s loop — so a test can drive one poll cycle
     * deterministically without a real timer/coroutine loop.
     */
    suspend fun pollOnce() {
        runCatching { engine.pollAgentEvents() }.getOrNull()?.forEach { applyPush(it) }
    }

    /**
     * Starts the background loop calling [pollOnce] every [pollIntervalMs].
     * Idempotent — cancels whatever loop was already running first, so
     * calling this more than once (e.g. once per app-foreground) never
     * accumulates duplicate loops double-applying the same pushes.
     */
    fun startPolling(scope: CoroutineScope) {
        pollJob?.cancel()
        pollJob = scope.launch {
            while (true) {
                pollOnce()
                delay(pollIntervalMs)
            }
        }
    }

    fun stopPolling() {
        pollJob?.cancel()
        pollJob = null
    }

    private companion object {
        const val DEFAULT_POLL_INTERVAL_MS = 2_000L
    }
}
