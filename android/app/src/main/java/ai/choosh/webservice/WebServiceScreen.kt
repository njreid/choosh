package ai.choosh.webservice

import android.annotation.SuppressLint
import android.content.Intent
import android.net.Uri
import android.webkit.CookieManager
import android.webkit.WebResourceRequest
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView

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
 * "WebView isolation": no JavaScript bridge (no `addJavascriptInterface`
 * call anywhere in this function — its absence *is* the enforcement, there
 * is no "disable" flag to set), no file/content access, no Choosh cookies
 * (Choosh's own auth never touches [CookieManager] at all — see this
 * function's cookie handling below, which only ever sets the gateway's own
 * per-pin token, never anything Choosh-identity-related), no internal
 * bearer token or relay/RPC access reachable from page JavaScript (there is
 * none to reach — this `WebView` only ever talks to `gatewayUrl`, a
 * loopback address with no route to anything else).
 */
// JavaScript is deliberately enabled below — real dev-server/SPA pages need
// it — and reviewed as safe specifically because this WebView never gets an
// `addJavascriptInterface` bridge, only talks to a loopback origin with no
// Choosh credential reachable, and blocks in-place external navigation (see
// this function's own doc comment) — the exact conditions
// `SetJavaScriptEnabled`'s lint check exists to make a reviewer confirm.
// `Uri.parse` (not the `String.toUri()` KTX extension) is used deliberately
// here rather than adding a new `androidx.core:core-ktx` dependency (and
// the corresponding `dependencyLocking` relock) for one call site — see
// `MarkdownScreen.kt`'s identical suppression for the same reasoning.
@SuppressLint("SetJavaScriptEnabled", "UseKtx")
@Composable
private fun IsolatedWebServiceWebView(gatewayUrl: String, cookieHeader: String, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val gatewayOrigin = remember(gatewayUrl) { Uri.parse(gatewayUrl) }

    AndroidView(
        modifier = modifier,
        factory = {
            val webView = WebView(context)
            webView.settings.javaScriptEnabled = true // dev servers/SPA pages need this; no bridge is ever attached regardless.
            webView.settings.allowFileAccess = false
            webView.settings.allowContentAccess = false
            webView.settings.domStorageEnabled = true // typical dev-server SPA requirement; scoped to this loopback origin only.

            // Per service-tunnels.md: "External navigation requires an
            // explicit user action and opens outside the trusted internal
            // surface." A navigation to this gateway's own loopback origin
            // stays in-WebView (that's the whole point of the gateway);
            // anything else only proceeds via an external browser Intent,
            // and only when Android reports a real user gesture behind it
            // — never silently, and never in-place.
            webView.webViewClient = object : WebViewClient() {
                override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                    val target = request.url
                    if (isSameLoopbackOrigin(gatewayOrigin, target)) return false
                    if (request.hasGesture()) {
                        runCatching { context.startActivity(Intent(Intent.ACTION_VIEW, target)) }
                    }
                    return true // never navigate this WebView itself to an external origin, gesture or not.
                }
            }

            val cookieManager = CookieManager.getInstance()
            cookieManager.setAcceptCookie(true)
            cookieManager.setAcceptThirdPartyCookies(webView, false)
            cookieManager.setCookie(gatewayUrl, cookieHeader)
            webView.loadUrl(gatewayUrl)
            webView
        },
        update = { view ->
            if (view.url == null) {
                CookieManager.getInstance().setCookie(gatewayUrl, cookieHeader)
                view.loadUrl(gatewayUrl)
            }
        },
    )

    DisposableEffect(Unit) {
        onDispose { /* WebView teardown is handled by AndroidView's own lifecycle; nothing gateway-specific to release here. */ }
    }
}

/** `true` only for the exact scheme+host+port this gateway is bound to — every other target (including a same-host-different-port loopback address) is "external" per this screen's navigation policy. */
private fun isSameLoopbackOrigin(gatewayOrigin: Uri, target: Uri): Boolean =
    target.scheme == gatewayOrigin.scheme && target.host == gatewayOrigin.host && target.port == gatewayOrigin.port
