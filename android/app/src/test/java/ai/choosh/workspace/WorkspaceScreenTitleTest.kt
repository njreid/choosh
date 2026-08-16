package ai.choosh.workspace

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * UX-friction audit finding #11: [WorkspaceScreen] used to show the raw,
 * meaningless-to-a-user `workspaceId` as its title even when the real
 * `ai.choosh.fleet.Workspace.name` was available earlier in the navigation
 * flow — [workspaceScreenTitle] is the fix, pinned down directly here.
 */
class WorkspaceScreenTitleTest {
    @Test
    fun `prefers the real workspace name when available`() {
        assertEquals("Workspace: app", workspaceScreenTitle("ws-choosh-app", "app"))
    }

    @Test
    fun `falls back to the raw id only when the name is genuinely unavailable`() {
        assertEquals("Workspace ws-choosh-app", workspaceScreenTitle("ws-choosh-app", null))
    }
}
