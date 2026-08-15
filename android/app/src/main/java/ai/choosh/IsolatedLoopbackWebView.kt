package ai.choosh

import android.annotation.SuppressLint
import android.content.Intent
import android.net.Uri
import android.webkit.CookieManager
import android.webkit.WebResourceRequest
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.viewinterop.AndroidView

/**
 * The isolated-`WebView`-against-one-loopback-gateway-origin factory shared by
 * `ai.choosh.webservice.WebServiceScreen` and `ai.choosh.markdown.MarkdownScreen` — both
 * screens host a `WebView` locked to a single loopback gateway origin with the identical
 * isolation posture `docs/specs/service-tunnels.md`'s "WebView isolation" section requires:
 * no JS bridge (no `addJavascriptInterface` call anywhere here — its absence *is* the
 * enforcement), no file/content access, no Choosh cookies/tokens (only ever sets the
 * gateway's own per-pin token via [cookieHeader], never anything Choosh-identity-related),
 * external navigation only via an explicit-gesture browser Intent, in-place navigation only
 * within [url]'s own scheme+host+port.
 *
 * This was independently hand-rolled twice (once per screen, added by different milestone
 * passes) before being unified here — the two copies differed only in whether JavaScript/DOM
 * storage were enabled, which [javaScriptEnabled]/[domStorageEnabled] now parameterize.
 * WebService's gateway proxies real dev-server/SPA pages that need both; Markdown's gateway
 * only ever serves static rendered HTML and needs neither. Centralizing this means a future
 * isolation-posture fix lands once, not in one copy while the other silently drifts.
 */
@SuppressLint("SetJavaScriptEnabled", "UseKtx")
@Composable
internal fun IsolatedLoopbackWebView(
    url: String,
    cookieHeader: String,
    javaScriptEnabled: Boolean,
    domStorageEnabled: Boolean,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    val gatewayOrigin = remember(url) { Uri.parse(url) }

    AndroidView(
        modifier = modifier,
        factory = {
            val webView = WebView(context)
            webView.settings.javaScriptEnabled = javaScriptEnabled
            webView.settings.allowFileAccess = false
            webView.settings.allowContentAccess = false
            webView.settings.domStorageEnabled = domStorageEnabled

            // Per service-tunnels.md: "External navigation requires an explicit user action
            // and opens outside the trusted internal surface." A navigation to this gateway's
            // own loopback origin stays in-WebView (that's the whole point of the gateway);
            // anything else only proceeds via an external browser Intent, and only when
            // Android reports a real user gesture behind it — never silently, never in-place.
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
            cookieManager.setCookie(url, cookieHeader)
            webView.loadUrl(url)
            webView
        },
        update = { view ->
            if (view.url == null) {
                CookieManager.getInstance().setCookie(url, cookieHeader)
                view.loadUrl(url)
            }
        },
    )
}

/** `true` only for the exact scheme+host+port this gateway is bound to — every other target (including a same-host-different-port loopback address) is "external" per this screen's navigation policy. */
private fun isSameLoopbackOrigin(gatewayOrigin: Uri, target: Uri): Boolean =
    target.scheme == gatewayOrigin.scheme && target.host == gatewayOrigin.host && target.port == gatewayOrigin.port
