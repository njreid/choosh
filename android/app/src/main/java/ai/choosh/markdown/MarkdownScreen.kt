package ai.choosh.markdown

import ai.choosh.IsolatedLoopbackWebView
import androidx.compose.foundation.layout.Box
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

/**
 * No JavaScript/DOM storage needed to render static Markdown->HTML — kept off entirely
 * (stricter than [ai.choosh.webservice.WebServiceScreen]'s call to the same shared
 * [IsolatedLoopbackWebView], which must allow both for real dev-server SPAs) since this
 * surface has no interactive script content of its own.
 */
@Composable
private fun IsolatedMarkdownWebView(docUrl: String, cookieHeader: String, modifier: Modifier = Modifier) {
    IsolatedLoopbackWebView(docUrl, cookieHeader, javaScriptEnabled = false, domStorageEnabled = false, modifier = modifier)
}
