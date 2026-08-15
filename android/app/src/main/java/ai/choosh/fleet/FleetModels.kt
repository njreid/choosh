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
 *
 * [active] is `null` when this `Project` wasn't built from a real
 * `project.list` response (e.g. still-fixture-only test construction) —
 * [isActive] falls back to the old workspace-derived heuristic in that
 * case. Once [FleetViewModel] sources Projects from
 * [ai.choosh.engine.ChooshEngine.projectList], [active] is always non-null
 * and *is* the value shown, per host-rpc.md's `project.list`: "`hostd`
 * computes it, the client does not" — this domain type must not
 * recompute a value the server already decided.
 */
data class Project(
    val projectId: String,
    val name: String,
    val primaryWorkspaceId: String,
    val workspaces: List<Workspace>,
    val active: Boolean? = null,
) {
    /**
     * `hostd`-computed [active] when present (the real `project.list`
     * path); otherwise the pre-RPC heuristic (any workspace with a live
     * item or a recent event), preserved only so fixture-only test
     * construction that predates `project.list` keeps its existing
     * behavior.
     */
    val isActive: Boolean get() = active ?: (workspaces.any { it.needsAttention } || workspaces.isNotEmpty())
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
 * Fixture Project/Workspace data (see class doc above). Deliberately
 * includes at least one attention-needing workspace and one devhost with
 * no active project, per the fork directive's UI-coverage requirement.
 *
 * As of `project.list`/`project.set_primary_workspace` landing
 * (docs/specs/host-rpc.md), a Project's own identity/`active` flag comes
 * from [ai.choosh.engine.ChooshEngine.projectList] (see [FleetViewModel]),
 * never from here directly — [FakeChooshEngine]'s own `projectList` still
 * leans on [projectsFor] to synthesize realistic `ProjectSummary` rows,
 * and [FleetViewModel] still leans on [workspacesFor] to fill in each
 * real Project's nested Workspace list, since `workspace.list` isn't
 * wired into [ai.choosh.engine.ChooshEngine] yet (a separate, tracked
 * gap — see PLAN.md's "Known follow-ups") — but no caller should reach
 * for [projectsFor] as a substitute for the real RPC anymore.
 */
object FleetFixtures {
    /** [Project.workspaces] fixture data for one already-known `projectId`, keyed the same way [projectsFor] is. */
    fun workspacesFor(projectId: String, devHosts: List<DevHostPresence>): List<Workspace> =
        projectsFor(devHosts).firstOrNull { it.projectId == projectId }?.workspaces.orEmpty()

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
