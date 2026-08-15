package ai.choosh.markdown

import ai.choosh.MarkdownGatewayBridge
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier

/**
 * Demo/verification-only entry point: renders [MarkdownScreen] against
 * [MarkdownGatewayBridge.nativeMarkdownGatewayStartFixture]'s fixed
 * in-memory fixture document — no relay connection or live `choosh-hostd`
 * needed. This is what the on-device Markdown-rendering verification
 * screenshot is taken through, per this task's verification requirement
 * ("a hand-constructed RPC response fixture if that's not ready"), mirroring
 * [ai.choosh.terminal.TerminalScreen]'s existing demo-output affordance for
 * the terminal renderer.
 */
@Composable
fun MarkdownFixtureDemoScreen(modifier: Modifier = Modifier) {
    var gatewayHandle by remember { mutableLongStateOf(0L) }
    var state by remember { mutableStateOf<MarkdownUiState>(MarkdownUiState.Loading) }

    DisposableEffect(Unit) {
        val handle = MarkdownGatewayBridge.nativeMarkdownGatewayStartFixture()
        gatewayHandle = handle
        state = if (handle == 0L) {
            MarkdownUiState.Failed("fixture gateway failed to start")
        } else {
            val port = MarkdownGatewayBridge.nativeMarkdownGatewayPort(handle)
            val token = MarkdownGatewayBridge.nativeMarkdownGatewayToken(handle)
            val info = MarkdownGatewayInfo(port, token)
            MarkdownUiState.Ready(info.docUrl, info.cookieHeader)
        }
        onDispose {
            if (gatewayHandle != 0L) MarkdownGatewayBridge.nativeMarkdownGatewayStop(gatewayHandle)
        }
    }

    MarkdownScreen(state, modifier)
}
