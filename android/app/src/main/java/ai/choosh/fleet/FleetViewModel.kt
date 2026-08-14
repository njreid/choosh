package ai.choosh.fleet

import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.DevHostPresence
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

/** Fired once per Project tap; the composition root turns this into "open the primary workspace." */
sealed interface FleetNavigationEvent {
    data class OpenWorkspace(val workspaceId: String) : FleetNavigationEvent
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
 */
class FleetViewModel(private val engine: ChooshEngine) : ViewModel() {
    private val _state = MutableStateFlow(FleetUiState())
    val state: StateFlow<FleetUiState> = _state.asStateFlow()

    private var devHosts: List<DevHostPresence> = emptyList()
    private var projects: List<Project> = emptyList()

    init {
        refresh()
    }

    fun refresh() {
        viewModelScope.launch {
            _state.value = _state.value.copy(isLoading = true, error = null)
            runCatching {
                devHosts = engine.listDevhosts()
                projects = FleetFixtures.projectsFor(devHosts)
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

    /** Per android-navigation.md: tapping a Project opens its primary Workspace directly. */
    fun onProjectTapped(project: Project): FleetNavigationEvent =
        FleetNavigationEvent.OpenWorkspace(project.primaryWorkspaceId)

    fun onDevHostTapped(devHost: DevHostPresence): FleetNavigationEvent =
        FleetNavigationEvent.OpenDevHost(devHost.deviceId)

    fun onWorkspaceTapped(workspace: Workspace): FleetNavigationEvent =
        FleetNavigationEvent.OpenWorkspace(workspace.workspaceId)

    private fun recompute() {
        val rows = rowsFor(_state.value.sortMode, devHosts, projects)
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
