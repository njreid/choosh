package ai.choosh.jj

import ai.choosh.engine.ChangeGraphNode
import ai.choosh.engine.ChooshEngine
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

/** [ChooshEngine] wrapper that always fails `workspaceLog`, delegating everything else to [FakeChooshEngine] — lets a test exercise [JjChangeGraphViewModel.refresh]'s `onFailure` path without a real transport failure. */
private class FailingWorkspaceLogChooshEngine(private val delegate: ChooshEngine = FakeChooshEngine()) : ChooshEngine by delegate {
    override suspend fun workspaceLog(deviceId: String, workspaceId: String, revset: String?, limit: Int): List<ChangeGraphNode> =
        error("simulated workspace.log failure")
}

/** [ChooshEngine] wrapper that always fails `workspaceOpUndo`/`workspaceOpRestore`, delegating everything else to [FakeChooshEngine] — lets a test exercise [JjChangeGraphViewModel.undo]/[JjChangeGraphViewModel.restore]'s `onFailure` paths without a real transport failure. */
private class FailingOpMutationChooshEngine(private val delegate: ChooshEngine = FakeChooshEngine()) : ChooshEngine by delegate {
    override suspend fun workspaceOpUndo(deviceId: String, workspaceId: String, opId: String): String =
        error("simulated workspace.op.undo failure")

    override suspend fun workspaceOpRestore(deviceId: String, workspaceId: String, opId: String): String =
        error("simulated workspace.op.restore failure")
}

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

    @Test
    fun `a workspace log failure surfaces as an error state, not a crash`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = JjChangeGraphViewModel(FailingWorkspaceLogChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(!state.isLoading)
        assertTrue(state.error?.contains("simulated workspace.log failure") == true)
        assertTrue("no stale node data should be shown alongside an error", state.nodes.isEmpty())
    }

    @Test
    fun `an undo failure surfaces an error without disturbing the already-loaded graph`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = JjChangeGraphViewModel(FailingOpMutationChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()
        val nodesBeforeUndo = viewModel.state.value.nodes

        viewModel.undoMostRecentOperation()
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(state.error?.contains("simulated workspace.op.undo failure") == true)
        // The failed undo's onFailure branch never calls refresh(), so the previously-loaded
        // graph must be exactly what it was before the failed attempt — not cleared, not stale.
        assertEquals(nodesBeforeUndo, state.nodes)
    }

    @Test
    fun `a restore failure surfaces an error without disturbing the already-loaded graph`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = JjChangeGraphViewModel(FailingOpMutationChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()
        val nodesBeforeRestore = viewModel.state.value.nodes
        val someOpId = viewModel.state.value.operations.first().opId

        viewModel.restore(someOpId)
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(state.error?.contains("simulated workspace.op.restore failure") == true)
        assertEquals(nodesBeforeRestore, state.nodes)
    }
}
