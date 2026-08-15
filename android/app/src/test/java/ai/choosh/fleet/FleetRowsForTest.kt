package ai.choosh.fleet

import ai.choosh.engine.ConnectionState
import ai.choosh.engine.DevHostPresence
import ai.choosh.engine.FakeChooshEngine
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/**
 * Unit tests for [FleetViewModel.rowsFor], the pure row-derivation function
 * per docs/specs/android-navigation.md's "Fleet drawer" section, plus the
 * tap-handler methods. [MainDispatcherRule] is needed even here: constructing
 * a [FleetViewModel] triggers its `init { refresh() }`, which launches on
 * `viewModelScope` (`Dispatchers.Main`) — unavailable in a plain JVM test
 * without it, regardless of whether a given test cares about `refresh()`'s
 * own outcome.
 */
class FleetRowsForTest {

    @get:Rule
    val mainDispatcherRule = MainDispatcherRule()

    private fun devHost(id: String, alias: String, state: ConnectionState = ConnectionState.ONLINE) =
        DevHostPresence(
            deviceId = id,
            alias = alias,
            platform = "linux",
            accountLabel = null,
            connectionState = state,
            lastSeen = "2026-08-14T20:00:00Z",
        )

    private fun workspace(id: String, name: String, hostId: String, needsAttention: Boolean = false, lastActiveAt: String) =
        Workspace(workspaceId = id, name = name, devHostId = hostId, needsAttention = needsAttention, lastActiveAt = lastActiveAt)

    @Test
    fun `project mode returns one row per project in order`() {
        val hostA = devHost("dev-a", "host-a")
        val projects = listOf(
            Project("proj-1", "first", "ws-1", listOf(workspace("ws-1", "w1", "dev-a", lastActiveAt = "2026-08-14T10:00:00Z"))),
            Project("proj-2", "second", "ws-2", listOf(workspace("ws-2", "w2", "dev-a", lastActiveAt = "2026-08-14T11:00:00Z"))),
        )

        val rows = FleetViewModel.rowsFor(SortMode.PROJECT, listOf(hostA), projects)

        assertEquals(2, rows.size)
        assertEquals("proj-1", (rows[0] as FleetRow.ProjectRow).project.projectId)
        assertEquals("proj-2", (rows[1] as FleetRow.ProjectRow).project.projectId)
    }

    @Test
    fun `host mode includes every devhost even with zero workspaces`() {
        val active = devHost("dev-active", "active-host")
        val lonely = devHost("dev-lonely", "lonely-host")
        val projects = listOf(
            Project("proj-1", "only", "ws-1", listOf(workspace("ws-1", "w1", "dev-active", lastActiveAt = "2026-08-14T10:00:00Z"))),
        )

        val rows = FleetViewModel.rowsFor(SortMode.HOST, listOf(active, lonely), projects)

        assertEquals(2, rows.size)
        val lonelyRow = rows.filterIsInstance<FleetRow.DevHostRow>().single { it.devHost.deviceId == "dev-lonely" }
        assertEquals(0, lonelyRow.workspaceCount)
        assertTrue(!lonelyRow.needsAttention)
    }

    @Test
    fun `host mode scopes workspace count to workspaces actually on that host`() {
        val hostA = devHost("dev-a", "host-a")
        val hostB = devHost("dev-b", "host-b")
        val projects = listOf(
            Project(
                "proj-1",
                "mixed",
                "ws-a1",
                listOf(
                    workspace("ws-a1", "a1", "dev-a", lastActiveAt = "2026-08-14T10:00:00Z"),
                    workspace("ws-a2", "a2", "dev-a", lastActiveAt = "2026-08-14T10:05:00Z"),
                    workspace("ws-b1", "b1", "dev-b", lastActiveAt = "2026-08-14T10:10:00Z"),
                ),
            ),
        )

        val rows = FleetViewModel.rowsFor(SortMode.HOST, listOf(hostA, hostB), projects)

        val rowA = rows.filterIsInstance<FleetRow.DevHostRow>().single { it.devHost.deviceId == "dev-a" }
        val rowB = rows.filterIsInstance<FleetRow.DevHostRow>().single { it.devHost.deviceId == "dev-b" }
        assertEquals(2, rowA.workspaceCount)
        assertEquals(1, rowB.workspaceCount)
    }

    @Test
    fun `host mode excludes workspaces belonging to a project with no workspaces at all`() {
        // Project.isActive is `workspaces.any { needsAttention } || workspaces.isNotEmpty()` — an
        // empty-workspace project is the only way to be "inactive" under the current definition,
        // and it trivially contributes zero workspaces either way. This test documents that
        // observable behavior rather than a stronger "non-empty project can still be inactive"
        // claim the current isActive definition doesn't actually support.
        val hostA = devHost("dev-a", "host-a")
        val projects = listOf(
            Project("proj-empty", "empty", primaryWorkspaceId = "does-not-exist", workspaces = emptyList()),
        )

        val rows = FleetViewModel.rowsFor(SortMode.HOST, listOf(hostA), projects)

        val rowA = rows.filterIsInstance<FleetRow.DevHostRow>().single()
        assertEquals(0, rowA.workspaceCount)
    }

    @Test
    fun `recent mode sorts by lastActiveAt descending regardless of insertion order`() {
        val hostA = devHost("dev-a", "host-a")
        // Deliberately out of chronological order relative to how they're inserted, so a passing
        // test proves a real timestamp sort rather than an insertion-order or string-sort artifact.
        val projects = listOf(
            Project(
                "proj-1",
                "p",
                "ws-mid",
                listOf(
                    workspace("ws-mid", "mid", "dev-a", lastActiveAt = "2026-08-14T12:00:00Z"),
                    workspace("ws-oldest", "oldest", "dev-a", lastActiveAt = "2026-08-10T08:00:00Z"),
                    workspace("ws-newest", "newest", "dev-a", lastActiveAt = "2026-08-14T23:59:00Z"),
                ),
            ),
        )

        val rows = FleetViewModel.rowsFor(SortMode.RECENT, listOf(hostA), projects)
        val order = rows.filterIsInstance<FleetRow.WorkspaceRow>().map { it.workspace.workspaceId }

        assertEquals(listOf("ws-newest", "ws-mid", "ws-oldest"), order)
    }

    @Test
    fun `recent mode resolves devhost alias for each workspace`() {
        val hostA = devHost("dev-a", "alias-a")
        val projects = listOf(
            Project("proj-1", "p", "ws-1", listOf(workspace("ws-1", "w1", "dev-a", lastActiveAt = "2026-08-14T10:00:00Z"))),
        )

        val rows = FleetViewModel.rowsFor(SortMode.RECENT, listOf(hostA), projects)

        assertEquals("alias-a", (rows.single() as FleetRow.WorkspaceRow).devHostAlias)
    }

    @Test
    fun `attention flag on a nested workspace survives every sort mode`() {
        val hostA = devHost("dev-a", "host-a")
        val projects = listOf(
            Project(
                "proj-1",
                "flagged",
                "ws-calm",
                listOf(
                    workspace("ws-calm", "calm", "dev-a", needsAttention = false, lastActiveAt = "2026-08-14T10:00:00Z"),
                    workspace("ws-hot", "hot", "dev-a", needsAttention = true, lastActiveAt = "2026-08-14T11:00:00Z"),
                ),
            ),
        )

        val projectRows = FleetViewModel.rowsFor(SortMode.PROJECT, listOf(hostA), projects)
        assertTrue("PROJECT mode must flag the project", (projectRows.single() as FleetRow.ProjectRow).needsAttention)

        val hostRows = FleetViewModel.rowsFor(SortMode.HOST, listOf(hostA), projects)
        assertTrue("HOST mode must flag the devhost", (hostRows.single() as FleetRow.DevHostRow).needsAttention)

        val recentRows = FleetViewModel.rowsFor(SortMode.RECENT, listOf(hostA), projects)
        val hotRow = recentRows.filterIsInstance<FleetRow.WorkspaceRow>().single { it.workspace.workspaceId == "ws-hot" }
        val calmRow = recentRows.filterIsInstance<FleetRow.WorkspaceRow>().single { it.workspace.workspaceId == "ws-calm" }
        assertTrue("RECENT mode must flag only the actual attention-needing workspace", hotRow.needsAttention)
        assertTrue(!calmRow.needsAttention)
    }

    @Test
    fun `tapping a project opens its designated primary workspace even when not first in the list`() {
        val project = Project(
            projectId = "proj-1",
            name = "p",
            primaryWorkspaceId = "ws-second",
            workspaces = listOf(
                Workspace("ws-first", "first", "dev-a", needsAttention = false, lastActiveAt = "2026-08-14T10:00:00Z"),
                Workspace("ws-second", "second", "dev-a", needsAttention = false, lastActiveAt = "2026-08-14T11:00:00Z"),
            ),
        )

        val event = FleetViewModel(FakeChooshEngine()).onProjectTapped(project)

        assertEquals(FleetNavigationEvent.OpenWorkspace("ws-second", "dev-a"), event)
    }

    @Test
    fun `tapping a devhost opens that devhost`() {
        val host = devHost("dev-x", "host-x")
        val event = FleetViewModel(FakeChooshEngine()).onDevHostTapped(host)
        assertEquals(FleetNavigationEvent.OpenDevHost("dev-x"), event)
    }

    @Test
    fun `tapping a workspace opens that workspace`() {
        val ws = workspace("ws-x", "x", "dev-a", lastActiveAt = "2026-08-14T10:00:00Z")
        val event = FleetViewModel(FakeChooshEngine()).onWorkspaceTapped(ws)
        assertEquals(FleetNavigationEvent.OpenWorkspace("ws-x", "dev-a"), event)
    }
}
