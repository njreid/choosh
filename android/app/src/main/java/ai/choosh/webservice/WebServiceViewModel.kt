package ai.choosh.webservice

import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.WebServiceStatus
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/**
 * Owns one pinned `WebService` item's lifecycle: polls `item.list` for its
 * status (`docs/specs/service-tunnels.md`'s "Readiness"), starts the local
 * loopback gateway once the item is `running`, and tears the gateway down
 * on [onUnpin]/[onCleared] — never anything else. Per the milestone's exit
 * criterion ("Unpinning a `WebService` closes its tunnel/gateway but leaves
 * the remote devhost process running"), nothing in this class or anywhere
 * it calls ever issues `item.stop` or any other RPC that could affect the
 * remote process — only [WebServiceGatewayController.stop], which is
 * purely local (closes this phone's loopback listener and relay tunnel).
 *
 * **Design note: the polling loop is NOT auto-started from `init`.** An
 * earlier version launched it unconditionally in `init` via
 * `viewModelScope.launch { while (isActive) { poll(); delay(...) } }`. That
 * is correct in production (`viewModelScope` is torn down by the real
 * `ViewModel` lifecycle's `onCleared`), but a plain JVM `ViewModel` unit
 * test never drives that lifecycle — and `kotlinx-coroutines-test`'s
 * `runTest` drains *every* coroutine scheduled on the shared
 * `TestDispatcher` (via `MainDispatcherRule`, `Dispatchers.Main` and
 * `runTest`'s scheduler are the same instance) before the test completes,
 * including ones outside the test's own scope. An infinite periodic
 * `delay`-loop has no "idle" point to drain to, so every test in this
 * class's original form hung — confirmed for real: a JVM test process spun
 * at ~100% CPU for over ten minutes before being killed. [startPolling] is
 * the fix: production (`WebServiceScreen`'s host, via
 * `LaunchedEffect(Unit) { viewModel.startPolling() }`) calls it explicitly
 * once; tests call [poll] directly instead and never touch the loop at all.
 */
class WebServiceViewModel(
    private val engine: ChooshEngine,
    private val deviceId: String,
    private val workspaceId: String,
    private val itemId: String,
    private val connectionHandle: Long?,
    private val gatewayController: WebServiceGatewayController = RealWebServiceGatewayController(),
    private val pollIntervalMs: Long = DEFAULT_POLL_INTERVAL_MS,
) : ViewModel() {
    private val _state = MutableStateFlow<WebServiceUiState>(WebServiceUiState.Starting)
    val state: StateFlow<WebServiceUiState> = _state.asStateFlow()

    private var gateway: WebGatewayHandleState = WebGatewayHandleState.None
    private var pollingJob: Job? = null

    /** Starts the periodic `item.list` polling loop, if not already running. Idempotent — safe to call from `LaunchedEffect(Unit)` on every (re)composition. */
    fun startPolling() {
        if (pollingJob?.isActive == true) return
        pollingJob = viewModelScope.launch {
            while (isActive) {
                poll()
                delay(pollIntervalMs)
            }
        }
    }

    /** `internal` (not `private`) so tests can drive state transitions deterministically without racing the background polling loop's virtual-time `delay` — see `WebServiceViewModelTest`. */
    internal suspend fun poll() {
        val status = runCatching { engine.itemList(deviceId, workspaceId) }
            .getOrNull()
            ?.firstOrNull { it.itemId == itemId }
            ?.status
            ?: WebServiceStatus.UNKNOWN

        if (status == WebServiceStatus.RUNNING) {
            ensureGatewayStarted()
        } else {
            stopGatewayIfStarted()
        }

        val gatewayInfo = (gateway as? WebGatewayHandleState.Started)?.handle?.info
        val gatewayFailed = gateway is WebGatewayHandleState.Failed
        _state.value = deriveWebServiceUiState(status, gatewayInfo, gatewayFailed)
    }

    private fun ensureGatewayStarted() {
        if (gateway !is WebGatewayHandleState.None) return
        val handle = connectionHandle
        if (handle == null) {
            gateway = WebGatewayHandleState.Failed
            return
        }
        val started = gatewayController.start(handle, deviceId, itemId)
        gateway = if (started != null) WebGatewayHandleState.Started(started) else WebGatewayHandleState.Failed
    }

    private fun stopGatewayIfStarted() {
        val current = gateway
        if (current is WebGatewayHandleState.Started) {
            gatewayController.stop(current.handle.handle)
        }
        gateway = WebGatewayHandleState.None
    }

    /** Explicit unpin: closes the gateway/tunnel only — see this class's doc comment. */
    fun onUnpin() {
        stopGatewayIfStarted()
    }

    override fun onCleared() {
        pollingJob?.cancel()
        stopGatewayIfStarted()
    }

    private sealed interface WebGatewayHandleState {
        data object None : WebGatewayHandleState
        data class Started(val handle: WebGatewayHandle) : WebGatewayHandleState
        data object Failed : WebGatewayHandleState
    }

    companion object {
        const val DEFAULT_POLL_INTERVAL_MS = 2_000L
    }
}
