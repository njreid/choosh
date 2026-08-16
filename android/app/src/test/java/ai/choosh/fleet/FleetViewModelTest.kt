package ai.choosh.fleet

import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.FakeChooshEngine
import ai.choosh.engine.WorkspaceSummary
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/** [ChooshEngine] wrapper that always fails `workspace.list`, delegating everything else to [FakeChooshEngine] — exercises [FleetViewModel]'s `workspace.list`-sourced path's failure state distinctly from a `project.list` failure. */
private class FailingWorkspaceListChooshEngine(private val delegate: ChooshEngine = FakeChooshEngine()) : ChooshEngine by delegate {
    override suspend fun workspaceList(deviceId: String): List<WorkspaceSummary> = error("simulated workspace.list failure")
}

/**
 * [FleetViewModel] state-flow behavior against [FakeChooshEngine] — the
 * derivation logic itself is covered separately in [FleetRowsForTest].
 */
@OptIn(ExperimentalCoroutinesApi::class)
class FleetViewModelTest {

    @get:Rule
    val mainDispatcherRule = MainDispatcherRule()

    @Test
    fun `refresh loads devhosts and populates rows`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        engine.connect("fake-session-credential") // FakeChooshEngine.listDevhosts() requires this first.
        val viewModel = FleetViewModel(engine)
        advanceUntilIdle() // let init { refresh() }'s viewModelScope.launch actually run.

        val state = viewModel.state.value
        assertTrue("expected a populated fleet after refresh", !state.isLoading)
        assertEquals(null, state.error)
        assertTrue("expected at least one row from FIXTURE_DEVHOSTS", state.rows.isNotEmpty())
        // FakeChooshEngine.FIXTURE_DEVHOSTS includes "old-cloud-box" with no matching
        // FleetFixtures project — confirms PROJECT mode (the default) doesn't silently
        // drop devhosts with no project, it just doesn't emit a row *for* them (HOST mode
        // is what guarantees every devhost a row; see FleetRowsForTest).
        assertEquals(SortMode.PROJECT, state.sortMode)
    }

    @Test
    fun `refresh failure surfaces as an error state, not a crash`() = runTest(mainDispatcherRule.dispatcher) {
        // Deliberately not calling connect() first: FakeChooshEngine.listDevhosts()
        // throws IllegalStateException in that case, exercising refresh()'s onFailure path.
        val viewModel = FleetViewModel(FakeChooshEngine())
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(!state.isLoading)
        assertNotNull("a failed refresh must populate an error message, not throw", state.error)
        assertTrue("a failed refresh must not leave stale rows", state.rows.isEmpty())
    }

    @Test
    fun `setSortMode recomputes rows from already-loaded data without another refresh call`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        engine.connect("fake-session-credential")
        val viewModel = FleetViewModel(engine)
        advanceUntilIdle()
        // FakeChooshEngine.projectList, merged across FIXTURE_DEVHOSTS's two online
        // devhosts, always returns exactly 2 projects (see loadProjects's doc comment).
        assertEquals(2, viewModel.state.value.rows.size)

        viewModel.setSortMode(SortMode.HOST)

        val hostState = viewModel.state.value
        assertEquals(SortMode.HOST, hostState.sortMode)
        // HOST mode includes every devhost (3 in FIXTURE_DEVHOSTS) — a different count
        // than PROJECT mode's 2, proving setSortMode actually re-derived rows rather
        // than leaving PROJECT mode's stale list in place.
        assertEquals(FakeChooshEngine.FIXTURE_DEVHOSTS.size, hostState.rows.size)
    }

    // --- project.list / project.set_primary_workspace RPC-backed path ------
    //
    // `refresh()` used to build `projects` straight from
    // `FleetFixtures.projectsFor(devHosts)` — now it goes through
    // `ChooshEngine.projectList`, per devhost, merged (`loadProjects`'s own
    // doc comment). These tests exercise that path's loading/error states,
    // not just the happy path the tests above already cover incidentally.

    @Test
    fun `a project-list failure on an online devhost surfaces as an error state, not a crash`() =
        runTest(mainDispatcherRule.dispatcher) {
            val engine = FakeChooshEngine()
            engine.connect("fake-session-credential")
            engine.simulateProjectListFailure = true
            val viewModel = FleetViewModel(engine)
            advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue(!state.isLoading)
            assertNotNull("a failed project.list must populate an error message, not throw", state.error)
            assertTrue("a failed refresh must not leave stale rows", state.rows.isEmpty())
        }

    @Test
    fun `a project-list failure does not corrupt an already-loaded fleet until the retry actually succeeds`() =
        runTest(mainDispatcherRule.dispatcher) {
            val engine = FakeChooshEngine()
            engine.connect("fake-session-credential")
            val viewModel = FleetViewModel(engine)
            advanceUntilIdle()
            assertEquals(2, viewModel.state.value.rows.size)

            engine.simulateProjectListFailure = true
            viewModel.refresh()
            advanceUntilIdle()

            val state = viewModel.state.value
            assertNotNull("a second, failing refresh must surface an error", state.error)
            assertEquals("previous rows are left in place, not silently cleared", 2, state.rows.size)
        }

    @Test
    fun `setPrimaryWorkspace succeeds and is reflected after the automatic refresh`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        engine.connect("fake-session-credential")
        val viewModel = FleetViewModel(engine)
        advanceUntilIdle()

        val projectRow = viewModel.state.value.rows.filterIsInstance<FleetRow.ProjectRow>().single { it.project.projectId == "proj-choosh" }
        val project = projectRow.project
        assertEquals("ws-choosh-app", project.primaryWorkspaceId)
        val newPrimary = project.workspaces.single { it.workspaceId == "ws-choosh-agent-b" }

        viewModel.setPrimaryWorkspace(project, newPrimary)
        advanceUntilIdle()

        val updated = viewModel.state.value.rows.filterIsInstance<FleetRow.ProjectRow>().single { it.project.projectId == "proj-choosh" }
        assertEquals("ws-choosh-agent-b", updated.project.primaryWorkspaceId)
        assertEquals(null, viewModel.state.value.error)
    }

    // --- workspace.list-sourced Project.workspaces (real RPC, not FleetFixtures) ---

    @Test
    fun `a Project's nested workspaces come from the real workspace_list RPC, not fixture data directly`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        engine.connect("fake-session-credential")
        val viewModel = FleetViewModel(engine)
        advanceUntilIdle()

        val project = viewModel.state.value.rows.filterIsInstance<FleetRow.ProjectRow>().single { it.project.projectId == "proj-choosh" }.project
        assertEquals(setOf("ws-choosh-app", "ws-choosh-agent-b"), project.workspaces.map { it.workspaceId }.toSet())
    }

    @Test
    fun `a workspace_list failure on an online devhost surfaces as an error state, not a crash`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FailingWorkspaceListChooshEngine()
        engine.connect("fake-session-credential")
        val viewModel = FleetViewModel(engine)
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(!state.isLoading)
        assertNotNull("a failed workspace.list must populate an error message, not throw", state.error)
        assertTrue("a failed refresh must not leave stale rows", state.rows.isEmpty())
    }

    @Test
    fun `setPrimaryWorkspace with a workspace outside the project is rejected, not silently applied`() =
        runTest(mainDispatcherRule.dispatcher) {
            val engine = FakeChooshEngine()
            engine.connect("fake-session-credential")
            val viewModel = FleetViewModel(engine)
            advanceUntilIdle()

            val projectRow = viewModel.state.value.rows.filterIsInstance<FleetRow.ProjectRow>().single { it.project.projectId == "proj-choosh" }
            val project = projectRow.project
            val foreignWorkspace = Workspace(
                workspaceId = "ws-sidecar-main",
                name = "main",
                devHostId = "dev-build-box-large",
                needsAttention = false,
                lastActiveAt = "2026-08-14T18:00:00Z",
            )

            viewModel.setPrimaryWorkspace(project, foreignWorkspace)
            advanceUntilIdle()

            val state = viewModel.state.value
            assertNotNull("a mismatched project/workspace pair must surface an error, not silently apply", state.error)
            val unchanged = state.rows.filterIsInstance<FleetRow.ProjectRow>().single { it.project.projectId == "proj-choosh" }
            assertEquals("ws-choosh-app", unchanged.project.primaryWorkspaceId)
        }
}
