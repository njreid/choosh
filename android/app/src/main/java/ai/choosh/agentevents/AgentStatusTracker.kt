package ai.choosh.agentevents

import ai.choosh.engine.AgentEvent
import ai.choosh.engine.AgentRunStatus
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Derives each `AgentTerminal` item's live [AgentRunStatus] (`starting`,
 * `busy`, `waiting`, `stopped`, `failed` — docs/specs/agent-events.md's
 * `agent_status` normalized event) from the live/replayed agent-event
 * stream, keyed by `itemId`. The per-item companion to
 * [AgentAttentionTracker]'s workspace-level `needsAttention` set, and the
 * real increment that class's own doc comment flagged as future work ("Real
 * per-item attention surfacing in the Explorer is a later increment").
 *
 * `item.list`'s own `status` field is not a substitute for this: per
 * host-rpc.md's `ItemSummary` doc comment, "`AgentTerminal`/`Shell` items
 * only ever use `Running`/`Stopped` ... `starting`/`failed`/`unknown` are
 * real only for `WebService` items" — an `AgentTerminal`'s Zellij tab is
 * either registered or it isn't, so `item.list` alone can never distinguish
 * a genuinely busy agent from one quietly waiting for its next prompt. This
 * tracker is the only source of that distinction, and is exactly what
 * `docs/specs/android-navigation.md`'s "active agents" explorer section
 * needs to actually read as "active" (a stopped/idle agent still has an
 * `item.list` row, but isn't what that section means by "active").
 *
 * Process-wide, like [AgentAttentionTracker] — see that class's own doc
 * comment for why a single shared instance, fed by the one shared
 * [AgentEventSubscription], is preferred over one tracker per open
 * workspace screen.
 */
class AgentStatusTracker {
    private val _statusByItemId = MutableStateFlow<Map<String, AgentRunStatus>>(emptyMap())
    val statusByItemId: StateFlow<Map<String, AgentRunStatus>> = _statusByItemId.asStateFlow()

    fun apply(event: AgentEvent) {
        if (event is AgentEvent.AgentStatusChanged) {
            _statusByItemId.value = _statusByItemId.value + (event.itemId to event.status)
        }
    }
}
