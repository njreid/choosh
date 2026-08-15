package ai.choosh.agentevents

import ai.choosh.engine.AgentEvent
import ai.choosh.engine.AgentEventPush
import ai.choosh.engine.FakeChooshEngine
import ai.choosh.engine.InputReason
import ai.choosh.engine.SequencedAgentEvent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * [AgentEventSubscription]'s orchestration against [FakeChooshEngine] —
 * the Kotlin-side half of docs/specs/agent-events.md's "Delivery and
 * replay" contract. Covers this task's three named scenarios: a replayed
 * batch applied in order (cursor advances to `latest_sequence`), a live
 * push applied and the cursor never regressing, and `snapshot_required`
 * triggering the documented refresh callback rather than doing nothing.
 */
class AgentEventSubscriptionTest {

    private fun subscription(
        engine: FakeChooshEngine,
        cursorStore: AgentEventCursorStore = AgentEventCursorStore(),
        attentionTracker: AgentAttentionTracker = AgentAttentionTracker(),
        onSnapshotRequired: suspend (String, String) -> Unit = { _, _ -> },
    ) = Triple(
        AgentEventSubscription(engine, cursorStore, attentionTracker, onSnapshotRequired),
        cursorStore,
        attentionTracker,
    )

    @Test
    fun `subscribe applies a replayed batch in order and advances the cursor to latest_sequence`() = runTest {
        val engine = FakeChooshEngine()
        engine.setAgentEventHistory(
            "ws-1",
            listOf(
                SequencedAgentEvent(1, AgentEvent.AgentStatusChanged("ws-1", "item-1", ai.choosh.engine.AgentRunStatus.BUSY)),
                SequencedAgentEvent(2, AgentEvent.InputRequired("ws-1", "item-1", InputReason.APPROVAL)),
            ),
        )
        val (subscription, cursorStore, attentionTracker) = subscription(engine)

        subscription.subscribe("dev-1", "ws-1")

        assertEquals("cursor must advance to the replay's latest_sequence", 2L, cursorStore.lastAcknowledged("ws-1"))
        // Applied in order: BUSY (sequence 1) then InputRequired (sequence
        // 2) — the net observable effect is "needs attention", proving the
        // InputRequired was applied *after* (not clobbered by) the BUSY
        // status, i.e. genuinely oldest-first.
        assertEquals(setOf("ws-1"), attentionTracker.needsAttention.value)
    }

    @Test
    fun `a second subscribe call only replays what's new since the first, never duplicating`() = runTest {
        val engine = FakeChooshEngine()
        engine.setAgentEventHistory(
            "ws-1",
            listOf(
                SequencedAgentEvent(1, AgentEvent.InputRequired("ws-1", "item-1", InputReason.APPROVAL)),
                SequencedAgentEvent(2, AgentEvent.TurnCompleted("ws-1", "item-1")),
            ),
        )
        val (subscription, cursorStore, attentionTracker) = subscription(engine)

        subscription.subscribe("dev-1", "ws-1") // replays 1 and 2 — net: attention cleared by TurnCompleted
        assertEquals(2L, cursorStore.lastAcknowledged("ws-1"))
        assertTrue(attentionTracker.needsAttention.value.isEmpty())

        // Re-subscribing (e.g. the workspace screen re-entering foreground)
        // with nothing new in history must not re-apply 1/2 a second time —
        // if InputRequired#1 were re-applied, attention would flip back on.
        subscription.subscribe("dev-1", "ws-1")
        assertEquals(2L, cursorStore.lastAcknowledged("ws-1"))
        assertTrue("re-subscribing with no new history must not re-flag attention", attentionTracker.needsAttention.value.isEmpty())
    }

    @Test
    fun `pollOnce applies a live push and advances the cursor`() = runTest {
        val engine = FakeChooshEngine()
        val (subscription, cursorStore, attentionTracker) = subscription(engine)

        engine.enqueueAgentEvent(AgentEventPush("dev-1", AgentEvent.InputRequired("ws-1", "item-1", InputReason.QUESTION), sequence = 5L))
        subscription.pollOnce()

        assertEquals(5L, cursorStore.lastAcknowledged("ws-1"))
        assertEquals(setOf("ws-1"), attentionTracker.needsAttention.value)
    }

    @Test
    fun `pollOnce never regresses the cursor even if a stale-sequenced push arrives`() = runTest {
        val engine = FakeChooshEngine()
        val (subscription, cursorStore, _) = subscription(engine)

        engine.enqueueAgentEvent(AgentEventPush("dev-1", AgentEvent.TurnCompleted("ws-1", "item-1"), sequence = 5L))
        subscription.pollOnce()
        assertEquals(5L, cursorStore.lastAcknowledged("ws-1"))

        // A stale/out-of-order push (should never happen for a
        // well-behaved relayd, but this must not regress the cursor if it
        // did).
        engine.enqueueAgentEvent(AgentEventPush("dev-1", AgentEvent.TurnCompleted("ws-1", "item-1"), sequence = 3L))
        subscription.pollOnce()
        assertEquals("a lower sequence must never regress the cursor", 5L, cursorStore.lastAcknowledged("ws-1"))
    }

    @Test
    fun `pollOnce applies multiple queued pushes across workspaces without cross-contamination`() = runTest {
        val engine = FakeChooshEngine()
        val (subscription, cursorStore, attentionTracker) = subscription(engine)

        engine.enqueueAgentEvent(AgentEventPush("dev-1", AgentEvent.InputRequired("ws-a", "item-1", InputReason.APPROVAL), sequence = 1L))
        engine.enqueueAgentEvent(AgentEventPush("dev-1", AgentEvent.InputRequired("ws-b", "item-1", InputReason.APPROVAL), sequence = 1L))
        subscription.pollOnce()

        assertEquals(1L, cursorStore.lastAcknowledged("ws-a"))
        assertEquals(1L, cursorStore.lastAcknowledged("ws-b"))
        assertEquals(setOf("ws-a", "ws-b"), attentionTracker.needsAttention.value)
    }

    @Test
    fun `an auth_required push (no workspace_id, no sequence) is applied but never touches the cursor`() = runTest {
        val engine = FakeChooshEngine()
        val (subscription, cursorStore, _) = subscription(engine)

        engine.enqueueAgentEvent(AgentEventPush("dev-1", AgentEvent.AuthRequired("aws", "WDJB-MJHT", "https://example.com/device"), sequence = null))
        // Must not throw — AuthRequired has no workspaceId/sequence to key a cursor update on.
        subscription.pollOnce()
        assertNull(cursorStore.lastAcknowledged("ws-1"))
    }

    @Test
    fun `snapshot_required triggers the documented refresh callback and resets the cursor rather than doing nothing`() = runTest {
        val engine = FakeChooshEngine()
        engine.simulateSnapshotRequired = true
        var refreshedFor: Pair<String, String>? = null
        val (subscription, cursorStore, _) = subscription(
            engine,
            onSnapshotRequired = { deviceId, workspaceId -> refreshedFor = deviceId to workspaceId },
        )
        // A prior cursor exists (as if this workspace was subscribed
        // before, then the devhost's spool evicted past it).
        cursorStore.acknowledge("ws-1", 500L)

        subscription.subscribe("dev-1", "ws-1")

        assertEquals("dev-1" to "ws-1", refreshedFor)
        assertNull("a snapshot_required outcome must reset the now-stale cursor", cursorStore.lastAcknowledged("ws-1"))
    }

    @Test
    fun `resubscribeAll re-resumes every currently-subscribed workspace`() = runTest {
        val engine = FakeChooshEngine()
        engine.setAgentEventHistory("ws-a", listOf(SequencedAgentEvent(1, AgentEvent.TurnCompleted("ws-a", "item-1"))))
        engine.setAgentEventHistory("ws-b", listOf(SequencedAgentEvent(1, AgentEvent.TurnCompleted("ws-b", "item-1"))))
        val (subscription, cursorStore, _) = subscription(engine)

        subscription.subscribe("dev-1", "ws-a")
        subscription.subscribe("dev-1", "ws-b")
        assertEquals(1L, cursorStore.lastAcknowledged("ws-a"))
        assertEquals(1L, cursorStore.lastAcknowledged("ws-b"))

        // A reconnect: new history appended for both, per agent-events.md's
        // "A reconnect after any gap ... MUST resume from the last
        // acknowledged sequence".
        engine.setAgentEventHistory(
            "ws-a",
            listOf(SequencedAgentEvent(1, AgentEvent.TurnCompleted("ws-a", "item-1")), SequencedAgentEvent(2, AgentEvent.TurnCompleted("ws-a", "item-1"))),
        )
        engine.setAgentEventHistory(
            "ws-b",
            listOf(SequencedAgentEvent(1, AgentEvent.TurnCompleted("ws-b", "item-1")), SequencedAgentEvent(2, AgentEvent.TurnCompleted("ws-b", "item-1"))),
        )
        subscription.resubscribeAll()

        assertEquals(2L, cursorStore.lastAcknowledged("ws-a"))
        assertEquals(2L, cursorStore.lastAcknowledged("ws-b"))
    }

    @Test
    fun `unsubscribe removes a workspace from resubscribeAll without touching its cursor`() = runTest {
        val engine = FakeChooshEngine()
        engine.setAgentEventHistory("ws-1", listOf(SequencedAgentEvent(1, AgentEvent.TurnCompleted("ws-1", "item-1"))))
        val (subscription, cursorStore, _) = subscription(engine)

        subscription.subscribe("dev-1", "ws-1")
        assertEquals(1L, cursorStore.lastAcknowledged("ws-1"))

        subscription.unsubscribe("ws-1")
        engine.setAgentEventHistory(
            "ws-1",
            listOf(SequencedAgentEvent(1, AgentEvent.TurnCompleted("ws-1", "item-1")), SequencedAgentEvent(2, AgentEvent.TurnCompleted("ws-1", "item-1"))),
        )
        subscription.resubscribeAll() // must not touch ws-1 anymore

        assertEquals("unsubscribe must stop resubscribeAll from touching this workspace's cursor", 1L, cursorStore.lastAcknowledged("ws-1"))
    }
}
