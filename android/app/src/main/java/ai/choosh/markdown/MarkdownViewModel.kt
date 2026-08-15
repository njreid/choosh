package ai.choosh.markdown

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/**
 * Owns one open Markdown document's local gateway lifecycle — per this
 * task's architecture decision (`rust/choosh-android-bridge/src/markdown_gateway.rs`'s
 * module doc), this gateway fetches content over the *existing*
 * `"rpc"`-purpose tunnel, not a new tunnel purpose, so there is no
 * `item.list`/status polling here the way [ai.choosh.webservice.WebServiceViewModel]
 * needs — a document either opens or it doesn't.
 */
class MarkdownViewModel(
    private val deviceId: String,
    private val workspaceId: String,
    private val docPath: String,
    private val connectionHandle: Long?,
    private val gatewayController: MarkdownGatewayController = RealMarkdownGatewayController(),
) : ViewModel() {
    private val _state = MutableStateFlow<MarkdownUiState>(MarkdownUiState.Loading)
    val state: StateFlow<MarkdownUiState> = _state.asStateFlow()

    private var gateway: MarkdownGatewayHandle? = null

    init {
        viewModelScope.launch {
            val handle = connectionHandle
            val started = if (handle != null) gatewayController.start(handle, deviceId, workspaceId, docPath) else null
            gateway = started
            _state.value = deriveMarkdownUiState(started?.info, gatewayFailed = started == null)
        }
    }

    /**
     * Explicitly closes this document's gateway — called on screen
     * teardown (and by [onCleared] for the "user just backgrounds/kills the
     * app" path). Public (unlike `onCleared`, which `ViewModel` declares
     * `protected`) so it's directly callable both from Compose's
     * `DisposableEffect` teardown and from a test.
     */
    fun onClose() {
        gateway?.let { gatewayController.stop(it.handle) }
        gateway = null
    }

    override fun onCleared() {
        onClose()
    }
}
