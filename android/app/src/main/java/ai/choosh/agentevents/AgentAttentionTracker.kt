package ai.choosh.agentevents

import ai.choosh.engine.AgentEvent
import ai.choosh.engine.AgentRunStatus
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Derives "does this workspace currently have an outstanding
 * `input_required`" from the live/replayed agent-event stream, per
 * docs/specs/agent-events.md. This is the one piece of real,
 * wired-to-live-events state behind what
 * [ai.choosh.fleet.Workspace.needsAttention] reads today only as fixture
 * data (see [ai.choosh.fleet.FleetFixtures]'s own doc comment on why —
 * `workspace.list` isn't wired into [ai.choosh.engine.ChooshEngine] yet,
 * a separate, tracked gap). [ai.choosh.fleet.FleetViewModel] is expected to
 * OR this tracker's [needsAttention] set into its row computation once
 * [AgentEventSubscription] is wired into the composition root.
 *
 * **Set/clear rule** (a deliberate, documented design call — the wire
 * protocol has no explicit "acknowledged" signal of its own; that's a
 * `notifications.md`-side concept, not this one):
 *  - [AgentEvent.InputRequired] sets a workspace's attention flag.
 *  - [AgentEvent.TurnCompleted], or an [AgentEvent.AgentStatusChanged] to
 *    [AgentRunStatus.BUSY], clears it — both are `agent-events.md`'s two
 *    normalized signals for "the agent is doing something again", which is
 *    the only implicit resolution this tracker recognizes.
 *
 * **Workspace-level, not item-level** — a deliberate simplification
 * matching [ai.choosh.fleet.Workspace.needsAttention]'s own existing
 * (fixture-only, pre-this-pass) granularity. Real per-item attention
 * surfacing in the Explorer is a later increment.
 */
class AgentAttentionTracker {
    private val _needsAttention = MutableStateFlow<Set<String>>(emptySet())
    val needsAttention: StateFlow<Set<String>> = _needsAttention.asStateFlow()

    fun apply(event: AgentEvent) {
        when (event) {
            is AgentEvent.InputRequired -> add(event.workspaceId)
            is AgentEvent.TurnCompleted -> remove(event.workspaceId)
            is AgentEvent.AgentStatusChanged -> if (event.status == AgentRunStatus.BUSY) remove(event.workspaceId)
            is AgentEvent.FilesChanged,
            is AgentEvent.EditorAttached,
            is AgentEvent.EditorDetached,
            is AgentEvent.AuthRequired,
            AgentEvent.Unknown,
            -> Unit
        }
    }

    private fun add(workspaceId: String) {
        _needsAttention.value = _needsAttention.value + workspaceId
    }

    private fun remove(workspaceId: String) {
        val current = _needsAttention.value
        if (workspaceId in current) {
            _needsAttention.value = current - workspaceId
        }
    }
}
