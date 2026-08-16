package ai.choosh.jj

import ai.choosh.engine.ChangeGraphNode
import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.OperationLogEntry
import ai.choosh.viewmodel.loadInto
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

data class JjChangeGraphUiState(
    val nodes: List<ChangeGraphNode> = emptyList(),
    val operations: List<OperationLogEntry> = emptyList(),
    val selectedChangeId: String? = null,
    val isLoading: Boolean = true,
    val error: String? = null,
)

/**
 * Backs the `JjChangeGraph` item (docs/milestones/M3-jj-diff-and-graph.md):
 * the DAG view (`workspace.log`), tap-to-inspect (local selection state
 * only — every field shown comes from the already-fetched
 * [ChangeGraphNode]), and one-tap undo/restore (`workspace.op.*`). Every
 * mutating action here calls [refresh] afterward, which is what satisfies
 * the M3 exit criterion "the change graph updates to reflect [an undo]
 * within one refresh cycle" — there is no separate, implicit refresh path.
 */
class JjChangeGraphViewModel(
    private val engine: ChooshEngine,
    private val deviceId: String,
    private val workspaceId: String,
) : ViewModel() {
    private val _state = MutableStateFlow(JjChangeGraphUiState())
    val state: StateFlow<JjChangeGraphUiState> = _state.asStateFlow()

    init {
        refresh()
    }

    fun refresh() {
        viewModelScope.launch {
            _state.loadInto(
                setLoading = { it.copy(isLoading = true, error = null) },
                call = {
                    val nodes = engine.workspaceLog(deviceId, workspaceId)
                    val operations = engine.workspaceOpLog(deviceId, workspaceId)
                    nodes to operations
                },
                onSuccess = { state, (nodes, operations) -> state.copy(nodes = nodes, operations = operations, isLoading = false, error = null) },
                onFailure = { state, failure -> state.copy(isLoading = false, error = failure.message ?: "failed to load change graph") },
            )
        }
    }

    fun selectChange(changeId: String) {
        _state.value = _state.value.copy(selectedChangeId = changeId)
    }

    fun dismissSelection() {
        _state.value = _state.value.copy(selectedChangeId = null)
    }

    /** One-tap undo of the most recently listed operation. */
    fun undoMostRecentOperation() {
        val latest = _state.value.operations.firstOrNull() ?: return
        undo(latest.opId)
    }

    fun undo(opId: String) {
        viewModelScope.launch {
            runCatching { engine.workspaceOpUndo(deviceId, workspaceId, opId) }
                .onSuccess { refresh() }
                .onFailure { failure -> _state.value = _state.value.copy(error = failure.message ?: "undo failed") }
        }
    }

    fun restore(opId: String) {
        viewModelScope.launch {
            runCatching { engine.workspaceOpRestore(deviceId, workspaceId, opId) }
                .onSuccess { refresh() }
                .onFailure { failure -> _state.value = _state.value.copy(error = failure.message ?: "restore failed") }
        }
    }
}
