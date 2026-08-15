package ai.choosh.agentevents

import ai.choosh.engine.AgentEvent
import ai.choosh.engine.AgentRunStatus
import ai.choosh.engine.InputReason
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class AgentAttentionTrackerTest {

    @Test
    fun `input_required sets the workspace's attention flag`() {
        val tracker = AgentAttentionTracker()
        tracker.apply(AgentEvent.InputRequired("ws-1", "item-1", InputReason.APPROVAL))
        assertEquals(setOf("ws-1"), tracker.needsAttention.value)
    }

    @Test
    fun `turn_completed clears attention for that workspace`() {
        val tracker = AgentAttentionTracker()
        tracker.apply(AgentEvent.InputRequired("ws-1", "item-1", InputReason.QUESTION))
        tracker.apply(AgentEvent.TurnCompleted("ws-1", "item-1"))
        assertTrue(tracker.needsAttention.value.isEmpty())
    }

    @Test
    fun `agent_status busy clears attention, other statuses do not`() {
        val tracker = AgentAttentionTracker()
        tracker.apply(AgentEvent.InputRequired("ws-1", "item-1", InputReason.PERMISSION))
        tracker.apply(AgentEvent.AgentStatusChanged("ws-1", "item-1", AgentRunStatus.WAITING))
        assertEquals("WAITING must not clear a prior input_required", setOf("ws-1"), tracker.needsAttention.value)
        tracker.apply(AgentEvent.AgentStatusChanged("ws-1", "item-1", AgentRunStatus.STARTING))
        assertEquals("STARTING must not clear it either", setOf("ws-1"), tracker.needsAttention.value)
        tracker.apply(AgentEvent.AgentStatusChanged("ws-1", "item-1", AgentRunStatus.BUSY))
        assertTrue("BUSY must clear it", tracker.needsAttention.value.isEmpty())
    }

    @Test
    fun `unrelated event kinds never affect attention`() {
        val tracker = AgentAttentionTracker()
        tracker.apply(AgentEvent.FilesChanged("ws-1", "item-1", listOf("a.txt")))
        tracker.apply(AgentEvent.EditorAttached("ws-1"))
        tracker.apply(AgentEvent.EditorDetached("ws-1"))
        tracker.apply(AgentEvent.AuthRequired("aws", "WDJB-MJHT", "https://example.com/device"))
        tracker.apply(AgentEvent.Unknown)
        assertTrue(tracker.needsAttention.value.isEmpty())
    }

    @Test
    fun `attention is tracked independently per workspace`() {
        val tracker = AgentAttentionTracker()
        tracker.apply(AgentEvent.InputRequired("ws-a", "item-1", InputReason.APPROVAL))
        tracker.apply(AgentEvent.InputRequired("ws-b", "item-1", InputReason.APPROVAL))
        assertEquals(setOf("ws-a", "ws-b"), tracker.needsAttention.value)
        tracker.apply(AgentEvent.TurnCompleted("ws-a", "item-1"))
        assertEquals(setOf("ws-b"), tracker.needsAttention.value)
    }

    @Test
    fun `clearing an already-clear workspace is a no-op, not a crash`() {
        val tracker = AgentAttentionTracker()
        tracker.apply(AgentEvent.TurnCompleted("ws-never-flagged", "item-1"))
        assertTrue(tracker.needsAttention.value.isEmpty())
    }
}
