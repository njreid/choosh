package ai.choosh.markdown

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class MarkdownUiStateTest {
    private val gateway = MarkdownGatewayInfo(port = 51234, token = "deadbeef")

    @Test
    fun `no gateway yet is loading`() {
        assertEquals(MarkdownUiState.Loading, deriveMarkdownUiState(gateway = null, gatewayFailed = false))
    }

    @Test
    fun `a live gateway is ready, pointed at the doc route`() {
        val state = deriveMarkdownUiState(gateway, gatewayFailed = false)
        assertTrue(state is MarkdownUiState.Ready)
        state as MarkdownUiState.Ready
        assertEquals("http://127.0.0.1:51234/doc", state.url)
    }

    @Test
    fun `a bind failure is a failed state even without an explicit gateway value`() {
        assertTrue(deriveMarkdownUiState(gateway = null, gatewayFailed = true) is MarkdownUiState.Failed)
    }

    @Test
    fun `cookie header uses the markdown-specific token cookie name and is HttpOnly`() {
        assertEquals("choosh_md_token=deadbeef; Path=/; HttpOnly", gateway.cookieHeader)
    }

    @Test
    fun `web service and markdown gateways use different cookie names`() {
        org.junit.Assert.assertNotEquals(
            ai.choosh.webservice.WEB_GATEWAY_TOKEN_COOKIE_NAME,
            MARKDOWN_GATEWAY_TOKEN_COOKIE_NAME,
        )
    }
}
