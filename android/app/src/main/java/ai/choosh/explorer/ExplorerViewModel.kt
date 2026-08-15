package ai.choosh.explorer

import ai.choosh.engine.ChangedPath
import ai.choosh.engine.ChooshEngine
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

data class ExplorerUiState(
    val changed: List<ChangedPath> = emptyList(),
    val conflicted: List<String> = emptyList(),
    val isLoading: Boolean = true,
    val error: String? = null,
)

/**
 * Backs the explorer's changed-files section (docs/specs/android-navigation.md's
 * "Page model": item 3 of the explorer's page-zero sections), wired to
 * `workspace.status` per docs/specs/jj-integration.md. A minimal surface
 * for this pass — the "active agents"/"registered development services"/
 * "searchable project tree" sections are a different milestone's scope
 * (per host-rpc.md's Item RPCs and workspace.tree.list, neither of which
 * this class touches).
 */
class ExplorerViewModel(
    private val engine: ChooshEngine,
    private val deviceId: String,
    private val workspaceId: String,
) : ViewModel() {
    private val _state = MutableStateFlow(ExplorerUiState())
    val state: StateFlow<ExplorerUiState> = _state.asStateFlow()

    init {
        refresh()
    }

    fun refresh() {
        viewModelScope.launch {
            _state.value = _state.value.copy(isLoading = true, error = null)
            runCatching { engine.workspaceStatus(deviceId, workspaceId) }
                .onSuccess { status ->
                    _state.value = _state.value.copy(changed = status.changed, conflicted = status.conflicted, isLoading = false, error = null)
                }
                .onFailure { failure ->
                    _state.value = _state.value.copy(isLoading = false, error = failure.message ?: "failed to load workspace status")
                }
        }
    }
}
