package ai.choosh.webservice

import ai.choosh.engine.WebServiceStatus

/**
 * What [WebServiceScreen] renders, per `docs/specs/service-tunnels.md`'s
 * "Readiness" section: "A service can be pinned while starting and shows a
 * retrying interstitial until ready or failed."
 */
sealed interface WebServiceUiState {
    /** `starting` (or `unknown`, treated identically — see [deriveWebServiceUiState]'s doc comment) — the retrying interstitial. */
    data object Starting : WebServiceUiState

    /** `running` and the local gateway is up: the `WebView` renders, pointed at [url] with [cookieHeader] set. */
    data class Ready(val url: String, val cookieHeader: String) : WebServiceUiState

    /** `stopped`: no `WebView`, an explicit "not running" state distinct from a transport/gateway failure. */
    data object Stopped : WebServiceUiState

    /** `failed`, or the gateway itself failed to bind despite the item reporting `running`. */
    data class Failed(val message: String) : WebServiceUiState
}

/**
 * Pure derivation, unit-tested directly rather than only through
 * [WebServiceViewModel]'s `StateFlow` — mirrors this codebase's existing
 * `FleetViewModel.rowsFor`/`JjDiffViewModel` precedent of keeping UI-state
 * logic as a plain function.
 *
 * `unknown` is treated the same as `starting` (retry) rather than as a
 * terminal error: `service-tunnels.md`'s five statuses don't specify how a
 * client should render `unknown` specifically, and per its own name it is
 * not evidence of failure — retrying is the conservative choice, and this
 * client already retries on a poll interval regardless.
 */
fun deriveWebServiceUiState(status: WebServiceStatus, gateway: WebGatewayInfo?, gatewayFailed: Boolean): WebServiceUiState = when {
    gatewayFailed -> WebServiceUiState.Failed("the local gateway failed to start")
    status == WebServiceStatus.FAILED -> WebServiceUiState.Failed("this service failed to start")
    status == WebServiceStatus.STOPPED -> WebServiceUiState.Stopped
    status == WebServiceStatus.RUNNING && gateway != null -> WebServiceUiState.Ready(gateway.url, gateway.cookieHeader)
    else -> WebServiceUiState.Starting // STARTING, UNKNOWN, or RUNNING-but-gateway-not-up-yet
}
