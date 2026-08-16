package ai.choosh.fleet

import ai.choosh.agentevents.AgentAttentionTracker
import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.ConnectionState
import ai.choosh.engine.DevHostPresence
import ai.choosh.engine.ProjectSummary
import ai.choosh.engine.WorkspaceSummary
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

data class FleetUiState(
    val sortMode: SortMode = SortMode.PROJECT,
    val rows: List<FleetRow> = emptyList(),
    val isLoading: Boolean = true,
    val error: String? = null,
)

/**
 * Fired once per Project/Workspace tap; the composition root turns
 * [OpenWorkspace] into a navigation to that Workspace's explorer/`JjDiff`/
 * `JjChangeGraph` surfaces. `deviceId` travels alongside `workspaceId` here
 * because every M3 RPC (`workspace.diff`/`log`/`op.*`) is addressed by
 * `(target_device_id, workspace_id)` together — the workspace list/tap
 * chain is the only place that pairing is available, so it's carried
 * through from here rather than re-derived downstream.
 */
sealed interface FleetNavigationEvent {
    data class OpenWorkspace(val workspaceId: String, val deviceId: String) : FleetNavigationEvent
    data class OpenDevHost(val deviceId: String) : FleetNavigationEvent
}

/**
 * Owns the fleet drawer's sort-mode state and derives its row list, per
 * docs/specs/android-navigation.md's "Fleet drawer" section:
 *  - PROJECT (default): Project -> DevHost -> Workspace, flattened here to
 *    one row per Project (DevHost/Workspace nesting is the row's own
 *    expand state in the UI layer, not modeled in this list).
 *  - HOST: DevHost -> Workspace, scoped to Projects with current activity,
 *    but every DevHost still appears (per the spec: "Every DevHost ... still
 *    appears even if it currently owns no active Workspace").
 *  - RECENT: flat, most-recently-active Workspace first, no grouping.
 * Attention flagging is a property of the row (`FleetRow.needsAttention`),
 * computed the same way in every mode — never a fourth mode.
 *
 * [attentionTracker], when supplied by the composition root (see
 * [ai.choosh.agentevents.AgentEventSubscription]'s own doc comment for why
 * a single process-wide subscription feeds every ViewModel that cares
 * about live agent-event state rather than each ViewModel polling on its
 * own), is `docs/specs/agent-events.md`'s live `input_required` signal —
 * the one real, wired data source behind what [Workspace.needsAttention]
 * otherwise reads only as [FleetFixtures]' static demo data. `null`
 * (the default) preserves this ViewModel's pre-existing,
 * fixture-data-only behavior exactly — every existing call site/test that
 * doesn't pass one keeps working unchanged.
 */
class FleetViewModel(
    private val engine: ChooshEngine,
    private val attentionTracker: AgentAttentionTracker? = null,
) : ViewModel() {
    private val _state = MutableStateFlow(FleetUiState())
    val state: StateFlow<FleetUiState> = _state.asStateFlow()

    private var devHosts: List<DevHostPresence> = emptyList()
    private var projects: List<Project> = emptyList()

    init {
        refresh()
        // Recompute rows on every live attention change, independent of
        // `refresh()`'s own RPC-driven cadence — an `input_required` push
        // (or its resolution) must reach the drawer's badge without
        // waiting for the next full `listDevhosts`/`projectList` round
        // trip.
        attentionTracker?.let { tracker ->
            viewModelScope.launch { tracker.needsAttention.collect { recompute() } }
        }
    }

    fun refresh() {
        viewModelScope.launch {
            _state.value = _state.value.copy(isLoading = true, error = null)
            runCatching {
                devHosts = engine.listDevhosts()
                projects = loadProjects(devHosts)
            }.onSuccess {
                recompute()
            }.onFailure { failure ->
                _state.value = _state.value.copy(isLoading = false, error = failure.message ?: "failed to load fleet")
            }
        }
    }

    fun setSortMode(mode: SortMode) {
        _state.value = _state.value.copy(sortMode = mode)
        recompute()
    }

    /**
     * `project.set_primary_workspace` (docs/specs/host-rpc.md), then a
     * full [refresh] so the drawer reflects the change immediately.
     * Failure (a mismatched `workspaceId`/`projectId`, or a transport
     * error) surfaces through [FleetUiState.error], the same observable
     * path [refresh] itself uses — callers watch [state], not a thrown
     * exception out of this function.
     */
    fun setPrimaryWorkspace(project: Project, workspace: Workspace) {
        viewModelScope.launch {
            runCatching {
                engine.setPrimaryWorkspace(workspace.devHostId, project.projectId, workspace.workspaceId)
            }.onSuccess {
                refresh()
            }.onFailure { failure ->
                _state.value = _state.value.copy(error = failure.message ?: "failed to set primary workspace")
            }
        }
    }

    /**
     * `project.list` and `workspace.list`, each called once per *online*
     * devhost — per host-rpc.md's `workspace.list` precedent ("called once
     * per devhost after list-devhosts"), which `project.list`'s own doc
     * comment says it mirrors despite host-rpc.md's prose describing
     * `project.list`'s result as "every Project the requesting Identity can
     * reach, across every devhost" (both are, in fact, single-devhost-tunnel
     * RPCs). An offline devhost is skipped outright for both calls (no live
     * tunnel to call over, per `list-devhosts`' `connectionState`); a thrown
     * failure from either call on an *online* devhost propagates to
     * [refresh]'s `runCatching`, surfacing as this ViewModel's normal error
     * state rather than silently dropping that devhost's Projects/Workspaces.
     *
     * Merge rule when the same `projectId` is reported by more than one
     * devhost (a Project with Workspaces spread across hosts): `active`
     * is OR'd together (active anywhere counts as active for the
     * Project as a whole) and the first-seen devhost's `name`/
     * `primaryWorkspaceId` wins — a deliberate, documented simplification
     * host-rpc.md doesn't itself pin down. Each [WorkspaceSummary] is
     * grouped into its own `project_id`'s [Project.workspaces] list,
     * regardless of which devhost's call returned it — `workspace.list`'s
     * `WorkspaceSummary.projectId` is the join key, the same one
     * `project.list`'s `ProjectSummary.projectId` uses.
     *
     * [Workspace.needsAttention] starts `false` for every real Workspace —
     * `workspace.list` carries no such field (per host-rpc.md, that's a
     * live `agent-events.md` signal, not a registry fact); [recompute]
     * unions in [attentionTracker]'s live set afterward, same as before this
     * pass. [Workspace.lastActiveAt] is `WorkspaceSummary.createdAt` — the
     * real RPC has no dedicated "last active" timestamp, and `created_at` is
     * the closest available proxy; Recent sort mode is therefore ordered by
     * registration time until a real activity timestamp exists on the wire,
     * a known, deliberate approximation.
     */
    private suspend fun loadProjects(devHosts: List<DevHostPresence>): List<Project> {
        val summaries = linkedMapOf<String, ProjectSummary>()
        val workspacesByProject = mutableMapOf<String, MutableList<Workspace>>()
        for (host in devHosts) {
            if (host.connectionState != ConnectionState.ONLINE) continue
            for (summary in engine.projectList(host.deviceId)) {
                val existing = summaries[summary.projectId]
                summaries[summary.projectId] = if (existing == null) summary else existing.copy(active = existing.active || summary.active)
            }
            for (workspace in engine.workspaceList(host.deviceId)) {
                workspacesByProject.getOrPut(workspace.projectId) { mutableListOf() }.add(workspace.toFleetWorkspace())
            }
        }
        return summaries.values.map { summary ->
            Project(
                projectId = summary.projectId,
                name = summary.name,
                primaryWorkspaceId = summary.primaryWorkspaceId.orEmpty(),
                workspaces = workspacesByProject[summary.projectId].orEmpty(),
                active = summary.active,
            )
        }
    }

    /** Per android-navigation.md: tapping a Project opens its primary Workspace directly. */
    fun onProjectTapped(project: Project): FleetNavigationEvent {
        val primary = project.workspaces.firstOrNull { it.workspaceId == project.primaryWorkspaceId }
        return FleetNavigationEvent.OpenWorkspace(project.primaryWorkspaceId, primary?.devHostId.orEmpty())
    }

    fun onDevHostTapped(devHost: DevHostPresence): FleetNavigationEvent =
        FleetNavigationEvent.OpenDevHost(devHost.deviceId)

    fun onWorkspaceTapped(workspace: Workspace): FleetNavigationEvent =
        FleetNavigationEvent.OpenWorkspace(workspace.workspaceId, workspace.devHostId)

    /**
     * Merges [attentionTracker]'s live `needsAttention` set into
     * [projects] before deriving rows via [rowsFor] — deliberately a
     * union with each [Workspace]'s own (fixture-sourced) flag, never a
     * downgrade: this ViewModel has no way to *clear* a fixture's
     * hardcoded `needsAttention = true`, only to add a live one on top.
     * [rowsFor] itself stays a pure function of plain data (unaffected by
     * this merge, still directly unit-tested by `FleetRowsForTest`) — the
     * live signal is folded in here, not threaded into that function's
     * own signature.
     */
    private fun recompute() {
        val liveAttention = attentionTracker?.needsAttention?.value.orEmpty()
        val effectiveProjects = if (liveAttention.isEmpty()) {
            projects
        } else {
            projects.map { project ->
                project.copy(
                    workspaces = project.workspaces.map { workspace ->
                        if (workspace.needsAttention || workspace.workspaceId !in liveAttention) workspace
                        else workspace.copy(needsAttention = true)
                    },
                )
            }
        }
        val rows = rowsFor(_state.value.sortMode, devHosts, effectiveProjects)
        _state.value = _state.value.copy(rows = rows, isLoading = false, error = null)
    }

    companion object {
        /** Pure function, unit-tested directly rather than only through the ViewModel's Flow. */
        fun rowsFor(mode: SortMode, devHosts: List<DevHostPresence>, projects: List<Project>): List<FleetRow> =
            when (mode) {
                SortMode.PROJECT -> projects.map { FleetRow.ProjectRow(it) }

                SortMode.HOST -> devHosts.map { host ->
                    val hostWorkspaces = projects.filter { it.isActive }
                        .flatMap { it.workspaces }
                        .filter { it.devHostId == host.deviceId }
                    FleetRow.DevHostRow(
                        devHost = host,
                        workspaceCount = hostWorkspaces.size,
                        needsAttention = hostWorkspaces.any { it.needsAttention },
                    )
                }

                SortMode.RECENT -> {
                    val hostAliasById = devHosts.associate { it.deviceId to it.alias }
                    projects.flatMap { it.workspaces }
                        .sortedByDescending { it.lastActiveAt }
                        .map { workspace -> FleetRow.WorkspaceRow(workspace, hostAliasById[workspace.devHostId] ?: workspace.devHostId) }
                }
            }
    }
}

/** `workspace.list`'s per-entry shape, adapted into this package's own [Workspace] — see [FleetViewModel.loadProjects]'s doc comment for [Workspace.needsAttention]/[Workspace.lastActiveAt]'s approximations. */
private fun WorkspaceSummary.toFleetWorkspace(): Workspace =
    Workspace(workspaceId = workspaceId, name = workspaceName, devHostId = devHostId, needsAttention = false, lastActiveAt = createdAt)
