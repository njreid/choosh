package ai.choosh.explorer

import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.FakeChooshEngine
import ai.choosh.engine.WorkspaceStatus
import ai.choosh.fleet.MainDispatcherRule
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/** [ChooshEngine] wrapper that always fails `workspaceStatus`, delegating everything else to [FakeChooshEngine] — lets a test exercise [ExplorerViewModel.refresh]'s `onFailure` path without a real transport failure. */
private class FailingWorkspaceStatusChooshEngine(private val delegate: ChooshEngine = FakeChooshEngine()) : ChooshEngine by delegate {
    override suspend fun workspaceStatus(deviceId: String, workspaceId: String): WorkspaceStatus = error("simulated workspace.status failure")
}

@OptIn(ExperimentalCoroutinesApi::class)
class ExplorerViewModelTest {

    @get:Rule
    val mainDispatcherRule = MainDispatcherRule()

    @Test
    fun `refresh loads the changed-files summary including a conflicted path`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(!state.isLoading)
        assertNull(state.error)
        assertTrue(state.changed.isNotEmpty())
        assertEquals(1, state.conflicted.size)
        assertTrue(state.changed.any { it.path == state.conflicted.first() })
    }

    @Test
    fun `a workspace status failure surfaces as an error state, not a crash`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FailingWorkspaceStatusChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(!state.isLoading)
        assertTrue(state.error?.contains("simulated workspace.status failure") == true)
        assertTrue("no stale changed-files data should be shown alongside an error", state.changed.isEmpty())
    }
}
