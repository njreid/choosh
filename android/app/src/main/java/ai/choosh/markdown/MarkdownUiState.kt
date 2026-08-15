package ai.choosh.markdown

/** What [MarkdownScreen] renders. */
sealed interface MarkdownUiState {
    data object Loading : MarkdownUiState
    data class Ready(val url: String, val cookieHeader: String) : MarkdownUiState
    data class Failed(val message: String) : MarkdownUiState
}

/** Pure derivation, unit-tested directly — see `ai.choosh.webservice.deriveWebServiceUiState`'s identical rationale for keeping this out of the `ViewModel` body. */
fun deriveMarkdownUiState(gateway: MarkdownGatewayInfo?, gatewayFailed: Boolean): MarkdownUiState = when {
    gatewayFailed -> MarkdownUiState.Failed("this document's local gateway failed to start")
    gateway != null -> MarkdownUiState.Ready(gateway.docUrl, gateway.cookieHeader)
    else -> MarkdownUiState.Loading
}
