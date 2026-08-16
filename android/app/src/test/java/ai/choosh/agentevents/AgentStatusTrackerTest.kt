package ai.choosh.agentevents

import ai.choosh.engine.AgentEvent
import ai.choosh.engine.AgentRunStatus
import ai.choosh.engine.InputReason
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class AgentStatusTrackerTest {

    @Test
    fun `apply records an AgentStatusChanged event by itemId`() {
        val tracker = AgentStatusTracker()

        tracker.apply(AgentEvent.AgentStatusChanged("ws-1", "item-1", AgentRunStatus.BUSY))

        assertEquals(mapOf("item-1" to AgentRunStatus.BUSY), tracker.statusByItemId.value)
    }

    @Test
    fun `a later status for the same item overwrites the earlier one`() {
        val tracker = AgentStatusTracker()

        tracker.apply(AgentEvent.AgentStatusChanged("ws-1", "item-1", AgentRunStatus.STARTING))
        tracker.apply(AgentEvent.AgentStatusChanged("ws-1", "item-1", AgentRunStatus.WAITING))

        assertEquals(AgentRunStatus.WAITING, tracker.statusByItemId.value["item-1"])
    }

    @Test
    fun `statuses for different items don't clobber each other`() {
        val tracker = AgentStatusTracker()

        tracker.apply(AgentEvent.AgentStatusChanged("ws-1", "item-1", AgentRunStatus.BUSY))
        tracker.apply(AgentEvent.AgentStatusChanged("ws-1", "item-2", AgentRunStatus.FAILED))

        assertEquals(AgentRunStatus.BUSY, tracker.statusByItemId.value["item-1"])
        assertEquals(AgentRunStatus.FAILED, tracker.statusByItemId.value["item-2"])
    }

    @Test
    fun `every non-AgentStatusChanged event is ignored, not just a no-op that happens to work`() {
        val tracker = AgentStatusTracker()

        tracker.apply(AgentEvent.InputRequired("ws-1", "item-1", InputReason.APPROVAL))
        tracker.apply(AgentEvent.TurnCompleted("ws-1", "item-1"))
        tracker.apply(AgentEvent.FilesChanged("ws-1", "item-1", listOf("a.txt")))
        tracker.apply(AgentEvent.EditorAttached("ws-1"))
        tracker.apply(AgentEvent.EditorDetached("ws-1"))
        tracker.apply(AgentEvent.AuthRequired("aws", "WDJB-MJHT", "https://example.com/device"))
        tracker.apply(AgentEvent.Unknown)

        assertTrue("none of these event kinds carry an item status", tracker.statusByItemId.value.isEmpty())
    }
}
