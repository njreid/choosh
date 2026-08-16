package ai.choosh.nav

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Pure-JVM tests for [ScreenBackStack] — no Compose UI, no Android runtime,
 * no `BackHandler` involved. `ChooshApp.kt` wires this class's [push]/[pop]
 * to every screen transition and to a real `BackHandler`; these tests cover
 * the sequencing/underflow behavior that wiring depends on. The
 * `BackHandler` integration itself, and the actual per-screen transitions
 * `ChooshApp.kt` performs, are code-reviewed and (where a Genymotion device
 * is reachable) verified on-device — see this task's report for what device
 * verification actually covered.
 */
class ScreenBackStackTest {

    @Test
    fun `starts at the root with nothing to pop`() {
        val stack = ScreenBackStack("root")
        assertEquals("root", stack.current)
        assertFalse(stack.canPop)
        assertEquals(listOf("root"), stack.snapshot())
    }

    @Test
    fun `push makes the pushed screen current`() {
        val stack = ScreenBackStack("root")
        stack.push("fleet")
        assertEquals("fleet", stack.current)
        assertTrue(stack.canPop)
        assertEquals(listOf("root", "fleet"), stack.snapshot())
    }

    @Test
    fun `pop returns to the previous screen`() {
        val stack = ScreenBackStack("root")
        stack.push("fleet")
        stack.push("workspace")
        assertEquals("workspace", stack.current)

        val popped = stack.pop()

        assertTrue(popped)
        assertEquals("fleet", stack.current)
        assertEquals(listOf("root", "fleet"), stack.snapshot())
    }

    @Test
    fun `sequential push then pop unwinds one level per pop, matching the dismiss to explorer to workspace-list progression`() {
        // Mirrors docs/specs/android-navigation.md's "Back behavior":
        // pinned page -> explorer -> workspace list -> fleet.
        val stack = ScreenBackStack("connection")
        stack.push("fleet")
        stack.push("workspace")
        stack.push("terminal")
        assertEquals(listOf("connection", "fleet", "workspace", "terminal"), stack.snapshot())

        assertTrue(stack.pop())
        assertEquals("workspace", stack.current)

        assertTrue(stack.pop())
        assertEquals("fleet", stack.current)

        assertTrue(stack.pop())
        assertEquals("connection", stack.current)

        // At the root: no more real screens to fall back to.
        assertFalse(stack.canPop)
    }

    @Test
    fun `popping past the root is a no-op, not a crash`() {
        val stack = ScreenBackStack("root")

        val poppedAtRoot = stack.pop()

        assertFalse(poppedAtRoot)
        assertEquals("root", stack.current)
        assertEquals(listOf("root"), stack.snapshot())

        // Repeated pops at the root stay well-behaved too.
        assertFalse(stack.pop())
        assertFalse(stack.pop())
        assertEquals("root", stack.current)
    }

    @Test
    fun `push after popping does not resurrect the popped entry`() {
        val stack = ScreenBackStack("root")
        stack.push("a")
        stack.push("b")
        stack.pop() // back to "a"
        stack.push("c")

        assertEquals(listOf("root", "a", "c"), stack.snapshot())
    }

    @Test
    fun `pushing the same value twice keeps both entries distinct on the stack`() {
        // A screen pushed twice (e.g. re-entering the same terminal) must pop
        // back through each entry individually, not collapse into one.
        val stack = ScreenBackStack("root")
        stack.push("terminal")
        stack.push("terminal")
        assertEquals(listOf("root", "terminal", "terminal"), stack.snapshot())

        assertTrue(stack.pop())
        assertEquals(listOf("root", "terminal"), stack.snapshot())
        assertTrue(stack.canPop)
    }
}
