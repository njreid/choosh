package ai.choosh.jj

import ai.choosh.engine.FakeChooshEngine
import ai.choosh.fleet.MainDispatcherRule
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/**
 * Exercises the M3 exit criteria that are reachable without a real device:
 * the change graph reflects a real conflicted merge (2 parents, working
 * copy is the merge, no crash) and `jj undo` from the phone produces a
 * visible refresh — against [FakeChooshEngine]'s stateful undo/restore
 * fixtures (see that class's doc comment).
 */
@OptIn(ExperimentalCoroutinesApi::class)
class JjChangeGraphViewModelTest {

    @get:Rule
    val mainDispatcherRule = MainDispatcherRule()

    @Test
    fun `refresh loads a conflicted 2-parent merge as the working copy`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = JjChangeGraphViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(!state.isLoading)
        assertNull(state.error)
        val workingCopy = state.nodes.single { it.isWorkingCopy }
        assertEquals(2, workingCopy.parentChangeIds.size)
    }

    @Test
    fun `tapping a change selects it and dismissing clears the selection`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = JjChangeGraphViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()
        val someChangeId = viewModel.state.value.nodes.first().changeId

        viewModel.selectChange(someChangeId)
        assertEquals(someChangeId, viewModel.state.value.selectedChangeId)

        viewModel.dismissSelection()
        assertNull(viewModel.state.value.selectedChangeId)
    }

    @Test
    fun `undo reverses the most recent operation and the graph reflects it within one refresh`() =
        runTest(mainDispatcherRule.dispatcher) {
            val viewModel = JjChangeGraphViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
            advanceUntilIdle()

            val descriptionBeforeUndo = viewModel.state.value.nodes.single { it.isWorkingCopy }.description
            val opCountBeforeUndo = viewModel.state.value.operations.size

            viewModel.undoMostRecentOperation()
            advanceUntilIdle()

            val stateAfterUndo = viewModel.state.value
            assertNull(stateAfterUndo.error)
            assertNotEquals(
                "undo should have reverted the working copy's description",
                descriptionBeforeUndo,
                stateAfterUndo.nodes.single { it.isWorkingCopy }.description,
            )
            assertTrue(
                "undo.undo() itself creates a new operation-log entry, so a fresh refresh should show more operations",
                stateAfterUndo.operations.size > opCountBeforeUndo,
            )
        }

    @Test
    fun `restore brings the description back after an undo`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = JjChangeGraphViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()
        val original = viewModel.state.value.nodes.single { it.isWorkingCopy }.description

        viewModel.undoMostRecentOperation()
        advanceUntilIdle()
        assertNotEquals(original, viewModel.state.value.nodes.single { it.isWorkingCopy }.description)

        val opToRestore = viewModel.state.value.operations.last().opId
        viewModel.restore(opToRestore)
        advanceUntilIdle()

        assertEquals(original, viewModel.state.value.nodes.single { it.isWorkingCopy }.description)
    }
}
