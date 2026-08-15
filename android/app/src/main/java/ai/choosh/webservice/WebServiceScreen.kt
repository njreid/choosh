package ai.choosh.webservice

import ai.choosh.IsolatedLoopbackWebView
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp

/**
 * Hosts a pinned `WebService` item, per `docs/specs/service-tunnels.md`'s
 * "WebView isolation" and "Readiness" sections. `state` drives what's
 * shown: [WebServiceUiState.Starting] is the retrying interstitial,
 * [WebServiceUiState.Ready] mounts the isolated `WebView`,
 * [WebServiceUiState.Stopped]/[WebServiceUiState.Failed] show a plain
 * status message (no `WebView` in either case — nothing to load).
 */
@Composable
fun WebServiceScreen(state: WebServiceUiState, modifier: Modifier = Modifier) {
    Box(modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        when (state) {
            is WebServiceUiState.Starting -> RetryingInterstitial()
            is WebServiceUiState.Ready -> IsolatedWebServiceWebView(state.url, state.cookieHeader, Modifier.fillMaxSize())
            is WebServiceUiState.Stopped -> StatusMessage("This service isn't running.")
            is WebServiceUiState.Failed -> StatusMessage(state.message)
        }
    }
}

@Composable
private fun RetryingInterstitial() {
    Column(Modifier.testTag("web-service-starting"), horizontalAlignment = Alignment.CenterHorizontally) {
        CircularProgressIndicator()
        Text("Starting…", modifier = Modifier.padding(top = 12.dp), style = MaterialTheme.typography.bodyMedium)
    }
}

@Composable
private fun StatusMessage(message: String) {
    Text(message, modifier = Modifier.testTag("web-service-status").padding(24.dp), style = MaterialTheme.typography.bodyMedium)
}

/**
 * The `WebService` `WebView` itself, locked down per service-tunnels.md's
 * "WebView isolation" — see [IsolatedLoopbackWebView]'s doc comment for the full posture.
 * JavaScript/DOM storage are enabled here (unlike [ai.choosh.markdown.MarkdownScreen]'s
 * call to the same shared composable): real dev-server/SPA pages need both, and this
 * remains safe specifically because this `WebView` never gets an `addJavascriptInterface`
 * bridge, only talks to a loopback origin with no Choosh credential reachable, and blocks
 * in-place external navigation.
 */
@Composable
private fun IsolatedWebServiceWebView(gatewayUrl: String, cookieHeader: String, modifier: Modifier = Modifier) {
    IsolatedLoopbackWebView(gatewayUrl, cookieHeader, javaScriptEnabled = true, domStorageEnabled = true, modifier = modifier)
}
