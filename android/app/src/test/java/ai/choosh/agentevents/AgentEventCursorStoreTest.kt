package ai.choosh.agentevents

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class AgentEventCursorStoreTest {

    @Test
    fun `lastAcknowledged is null before anything is ever acknowledged`() {
        val store = AgentEventCursorStore()
        assertNull(store.lastAcknowledged("ws-1"))
    }

    @Test
    fun `acknowledge advances the cursor`() {
        val store = AgentEventCursorStore()
        store.acknowledge("ws-1", 5L)
        assertEquals(5L, store.lastAcknowledged("ws-1"))
    }

    @Test
    fun `acknowledge never regresses the cursor`() {
        val store = AgentEventCursorStore()
        store.acknowledge("ws-1", 5L)
        store.acknowledge("ws-1", 3L) // an out-of-order/older sequence — must not regress
        assertEquals("a lower sequence must never regress the cursor", 5L, store.lastAcknowledged("ws-1"))
        store.acknowledge("ws-1", 5L) // equal — also must not "change" anything observable
        assertEquals(5L, store.lastAcknowledged("ws-1"))
        store.acknowledge("ws-1", 6L)
        assertEquals("a genuinely newer sequence must still advance it", 6L, store.lastAcknowledged("ws-1"))
    }

    @Test
    fun `cursors are tracked independently per workspace`() {
        val store = AgentEventCursorStore()
        store.acknowledge("ws-a", 10L)
        store.acknowledge("ws-b", 2L)
        assertEquals(10L, store.lastAcknowledged("ws-a"))
        assertEquals(2L, store.lastAcknowledged("ws-b"))
    }

    @Test
    fun `reset clears the cursor back to no prior ack`() {
        val store = AgentEventCursorStore()
        store.acknowledge("ws-1", 5L)
        store.reset("ws-1")
        assertNull("reset must fully clear the cursor, not merely lower it", store.lastAcknowledged("ws-1"))
    }

    @Test
    fun `reset only affects the named workspace`() {
        val store = AgentEventCursorStore()
        store.acknowledge("ws-a", 5L)
        store.acknowledge("ws-b", 7L)
        store.reset("ws-a")
        assertNull(store.lastAcknowledged("ws-a"))
        assertEquals(7L, store.lastAcknowledged("ws-b"))
    }
}
