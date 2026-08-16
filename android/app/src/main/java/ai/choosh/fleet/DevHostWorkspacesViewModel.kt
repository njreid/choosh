package ai.choosh.fleet

import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.WorkspaceSummary
import ai.choosh.viewmodel.loadInto
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

data class DevHostWorkspacesUiState(
    val workspaces: List<WorkspaceSummary> = emptyList(),
    val isLoading: Boolean = true,
    val error: String? = null,
)

/**
 * Backs the real per-devhost Workspace list screen this replaces
 * `ChooshApp.kt`'s old `Screen.DevHostPlaceholder` (a literal placeholder
 * reading "Per-devhost workspace list lands with real workspace RPCs
 * (M1)") with — `workspace.list` filtered to `deviceId`, per
 * docs/specs/android-navigation.md's "Workspace entry": "Selecting a
 * DevHost never scans its filesystem; its Workspace list comes from
 * `workspace.list`". Selecting a DevHost row in Host mode, or a Project row
 * whose primary Workspace can't be resolved, lands here.
 */
class DevHostWorkspacesViewModel(private val engine: ChooshEngine, private val deviceId: String) : ViewModel() {
    private val _state = MutableStateFlow(DevHostWorkspacesUiState())
    val state: StateFlow<DevHostWorkspacesUiState> = _state.asStateFlow()

    init {
        refresh()
    }

    fun refresh() {
        viewModelScope.launch {
            _state.loadInto(
                setLoading = { it.copy(isLoading = true, error = null) },
                call = { engine.workspaceList(deviceId) },
                onSuccess = { state, workspaces -> state.copy(workspaces = workspaces, isLoading = false, error = null) },
                onFailure = { state, failure -> state.copy(isLoading = false, error = failure.message ?: "failed to load workspaces") },
            )
        }
    }
}
