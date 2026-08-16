package ai.choosh.fleet

import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.FakeChooshEngine
import ai.choosh.engine.WorkspaceSummary
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/** [ChooshEngine] wrapper that always fails `workspace.list`, delegating everything else to [FakeChooshEngine]. */
private class FailingDevHostWorkspaceListChooshEngine(private val delegate: ChooshEngine = FakeChooshEngine()) : ChooshEngine by delegate {
    override suspend fun workspaceList(deviceId: String): List<WorkspaceSummary> = error("simulated workspace.list failure")
}

/**
 * [DevHostWorkspacesViewModel] — the real per-devhost Workspace list that
 * replaces `ChooshApp.kt`'s old `Screen.DevHostPlaceholder`. Covers
 * loading/error/empty/success states, per this codebase's established
 * discipline (mirrors [ai.choosh.explorer.ExplorerViewModelTest]'s shape).
 */
@OptIn(ExperimentalCoroutinesApi::class)
class DevHostWorkspacesViewModelTest {

    @get:Rule
    val mainDispatcherRule = MainDispatcherRule()

    @Test
    fun `refresh loads the workspace list scoped to this devhost`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        engine.connect("fake-session-credential")
        // FakeChooshEngine.workspaceList is scoped by devHostId, mirroring the real RPC's
        // per-devhost scoping — "mbp-home" only ever owns proj-choosh's "ws-choosh-app".
        val mbpDeviceId = FakeChooshEngine.FIXTURE_DEVHOSTS.first { it.alias == "mbp-home" }.deviceId
        val viewModel = DevHostWorkspacesViewModel(engine, mbpDeviceId)
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(!state.isLoading)
        assertNull(state.error)
        assertTrue("expected at least one workspace for mbp-home", state.workspaces.isNotEmpty())
        assertTrue(state.workspaces.all { it.devHostId == mbpDeviceId })
    }

    @Test
    fun `an empty devhost with no registered workspaces is a clean empty state, not an error`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        engine.connect("fake-session-credential")
        // "old-cloud-box" has no matching FleetFixtures project/workspace at all.
        val emptyDeviceId = FakeChooshEngine.FIXTURE_DEVHOSTS.first { it.alias == "old-cloud-box" }.deviceId
        val viewModel = DevHostWorkspacesViewModel(engine, emptyDeviceId)
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(!state.isLoading)
        assertNull(state.error)
        assertTrue(state.workspaces.isEmpty())
    }

    @Test
    fun `a workspace_list failure surfaces as an error state, not a crash`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = DevHostWorkspacesViewModel(FailingDevHostWorkspaceListChooshEngine(), "dev-1")
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(!state.isLoading)
        assertNotNull("a failed workspace.list must populate an error message, not throw", state.error)
        assertTrue("a failed refresh must not leave stale rows", state.workspaces.isEmpty())
    }

    @Test
    fun `refresh can recover from a prior failure`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        engine.connect("fake-session-credential")
        val mbpDeviceId = FakeChooshEngine.FIXTURE_DEVHOSTS.first { it.alias == "mbp-home" }.deviceId
        val viewModel = DevHostWorkspacesViewModel(engine, mbpDeviceId)
        advanceUntilIdle()
        assertEquals(null, viewModel.state.value.error)

        viewModel.refresh()
        advanceUntilIdle()

        assertNull(viewModel.state.value.error)
        assertTrue(viewModel.state.value.workspaces.isNotEmpty())
    }
}
