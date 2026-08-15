package ai.choosh.markdown

import android.annotation.SuppressLint
import android.content.Intent
import android.net.Uri
import android.webkit.CookieManager
import android.webkit.WebResourceRequest
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView

/**
 * Choosh's Markdown/Datastar `WebView`, per
 * `docs/specs/service-tunnels.md`'s "WebView isolation" section — a
 * **separate** `WebView`/gateway from [ai.choosh.webservice.WebServiceScreen]'s
 * (different loopback server, different purpose; see
 * `markdown_gateway.rs`'s module doc for why), but the same isolation
 * posture: no JS bridge, no Choosh cookies/tokens, no file/content access,
 * external navigation only via an explicit-gesture browser Intent.
 */
@Composable
fun MarkdownScreen(state: MarkdownUiState, modifier: Modifier = Modifier) {
    Box(modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        when (state) {
            is MarkdownUiState.Loading -> CircularProgressIndicator(Modifier.testTag("markdown-loading"))
            is MarkdownUiState.Ready -> IsolatedMarkdownWebView(state.url, state.cookieHeader, Modifier.fillMaxSize())
            is MarkdownUiState.Failed -> Text(state.message, modifier = Modifier.testTag("markdown-failed").padding(24.dp), style = MaterialTheme.typography.bodyMedium)
        }
    }
}

// `Uri.parse` (not `String.toUri()`) rather than adding a new
// `androidx.core:core-ktx` dependency (and its `dependencyLocking` relock)
// for one call site — a deliberate, reviewed choice, not an oversight.
@SuppressLint("UseKtx")
@Composable
private fun IsolatedMarkdownWebView(docUrl: String, cookieHeader: String, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val gatewayOrigin = remember(docUrl) { Uri.parse(docUrl) }

    AndroidView(
        modifier = modifier,
        factory = {
            val webView = WebView(context)
            // No JavaScript needed to render static Markdown->HTML — kept
            // off entirely (stricter than WebServiceScreen's `WebView`,
            // which must allow it for real dev-server SPAs) since this
            // surface has no interactive script content of its own.
            webView.settings.javaScriptEnabled = false
            webView.settings.allowFileAccess = false
            webView.settings.allowContentAccess = false

            webView.webViewClient = object : WebViewClient() {
                override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                    val target = request.url
                    if (isSameLoopbackOrigin(gatewayOrigin, target)) return false
                    if (request.hasGesture()) {
                        runCatching { context.startActivity(Intent(Intent.ACTION_VIEW, target)) }
                    }
                    return true
                }
            }

            val cookieManager = CookieManager.getInstance()
            cookieManager.setAcceptCookie(true)
            cookieManager.setAcceptThirdPartyCookies(webView, false)
            cookieManager.setCookie(docUrl, cookieHeader)
            webView.loadUrl(docUrl)
            webView
        },
        update = { view ->
            if (view.url == null) {
                CookieManager.getInstance().setCookie(docUrl, cookieHeader)
                view.loadUrl(docUrl)
            }
        },
    )
}

private fun isSameLoopbackOrigin(gatewayOrigin: Uri, target: Uri): Boolean =
    target.scheme == gatewayOrigin.scheme && target.host == gatewayOrigin.host && target.port == gatewayOrigin.port
