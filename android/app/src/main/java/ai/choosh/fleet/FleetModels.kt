package ai.choosh.fleet

import ai.choosh.engine.ConnectionState
import ai.choosh.engine.DevHostPresence

/**
 * A `jj workspace` + Zellij session, per DESIGN.md §4. Real Workspace data
 * is a later milestone (M1, per docs/milestones/M1-workspace-and-jj.md) —
 * M0 only has devhost presence — so [FleetFixtures] supplies invented
 * Project/Workspace shape data here, keyed against the real
 * [DevHostPresence] list `ChooshEngine.listDevhosts()` returns, so the
 * drawer built against it is ready for real data later with no shape
 * change.
 */
data class Workspace(
    val workspaceId: String,
    val name: String,
    val devHostId: String,
    /** True if this workspace has an outstanding, unacknowledged `input_required`. */
    val needsAttention: Boolean,
    val lastActiveAt: String,
)

/**
 * Per docs/specs/android-navigation.md: a Project has a designated primary
 * Workspace (explicit, defaults to the first one registered) that tapping
 * the Project row opens directly.
 */
data class Project(
    val projectId: String,
    val name: String,
    val primaryWorkspaceId: String,
    val workspaces: List<Workspace>,
) {
    /** Per android-navigation.md's Host-mode definition: any workspace with a live item or a recent event. */
    val isActive: Boolean get() = workspaces.any { it.needsAttention } || workspaces.isNotEmpty()
    val needsAttention: Boolean get() = workspaces.any { it.needsAttention }
}

/** One resolved row in the fleet drawer, independent of which sort mode produced it. */
sealed interface FleetRow {
    val id: String
    val needsAttention: Boolean

    data class ProjectRow(val project: Project) : FleetRow {
        override val id get() = "project:${project.projectId}"
        override val needsAttention get() = project.needsAttention
    }

    data class DevHostRow(val devHost: DevHostPresence, val workspaceCount: Int, override val needsAttention: Boolean) : FleetRow {
        override val id get() = "devhost:${devHost.deviceId}"
    }

    data class WorkspaceRow(val workspace: Workspace, val devHostAlias: String) : FleetRow {
        override val id get() = "workspace:${workspace.workspaceId}"
        override val needsAttention get() = workspace.needsAttention
    }
}

enum class SortMode { PROJECT, HOST, RECENT }

/**
 * Fixture Project/Workspace data for this pass (see class doc above).
 * Deliberately includes at least one attention-needing workspace and one
 * devhost with no active project, per the fork directive's UI-coverage
 * requirement.
 */
object FleetFixtures {
    fun projectsFor(devHosts: List<DevHostPresence>): List<Project> {
        val mbp = devHosts.firstOrNull { it.alias == "mbp-home" }?.deviceId ?: return emptyList()
        val buildBox = devHosts.firstOrNull { it.alias == "build-box-large" }?.deviceId ?: mbp

        return listOf(
            Project(
                projectId = "proj-choosh",
                name = "choosh",
                primaryWorkspaceId = "ws-choosh-app",
                workspaces = listOf(
                    Workspace("ws-choosh-app", "app", mbp, needsAttention = true, lastActiveAt = "2026-08-14T20:05:00Z"),
                    Workspace("ws-choosh-agent-b", "agent-b", buildBox, needsAttention = false, lastActiveAt = "2026-08-14T19:40:00Z"),
                ),
            ),
            Project(
                projectId = "proj-sidecar",
                name = "sidecar-tools",
                primaryWorkspaceId = "ws-sidecar-main",
                workspaces = listOf(
                    Workspace("ws-sidecar-main", "main", buildBox, needsAttention = false, lastActiveAt = "2026-08-14T18:00:00Z"),
                ),
            ),
        )
    }
}
