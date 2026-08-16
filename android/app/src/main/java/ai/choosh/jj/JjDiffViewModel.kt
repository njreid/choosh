package ai.choosh.jj

import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.DiffFileEntry
import ai.choosh.viewmodel.loadInto
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

data class JjDiffUiState(
    val from: String = "",
    val to: String = "",
    val files: List<DiffFileEntry> = emptyList(),
    val isLoading: Boolean = false,
    val error: String? = null,
)

/**
 * Backs the `JjDiff` item (docs/milestones/M3-jj-diff-and-graph.md):
 * `workspace.diff` between any two revisions, `from`/`to` blank meaning the
 * RPC's own default (`"@-"`/`"@"`). Every hunk/segment in [JjDiffUiState.files]
 * comes verbatim from [ChooshEngine.workspaceDiff] — this class computes no
 * diff itself, only holds the load state around that one call.
 */
class JjDiffViewModel(
    private val engine: ChooshEngine,
    private val deviceId: String,
    private val workspaceId: String,
) : ViewModel() {
    private val _state = MutableStateFlow(JjDiffUiState())
    val state: StateFlow<JjDiffUiState> = _state.asStateFlow()

    init {
        load()
    }

    fun setFrom(value: String) {
        _state.value = _state.value.copy(from = value)
    }

    fun setTo(value: String) {
        _state.value = _state.value.copy(to = value)
    }

    fun load() {
        viewModelScope.launch {
            _state.loadInto(
                setLoading = { it.copy(isLoading = true, error = null) },
                call = { engine.workspaceDiff(deviceId, workspaceId, _state.value.from.ifBlank { null }, _state.value.to.ifBlank { null }) },
                onSuccess = { state, files -> state.copy(files = files, isLoading = false, error = null) },
                onFailure = { state, failure -> state.copy(isLoading = false, error = failure.message ?: "failed to load diff") },
            )
        }
    }
}
