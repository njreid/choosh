package ai.choosh.webservice

import ai.choosh.engine.WebServiceStatus
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class WebServiceUiStateTest {
    private val gateway = WebGatewayInfo(port = 45678, token = "abc123")

    @Test
    fun `starting status shows the retrying interstitial`() {
        assertEquals(WebServiceUiState.Starting, deriveWebServiceUiState(WebServiceStatus.STARTING, gateway = null, gatewayFailed = false))
    }

    @Test
    fun `unknown status is treated the same as starting, not as an error`() {
        assertEquals(WebServiceUiState.Starting, deriveWebServiceUiState(WebServiceStatus.UNKNOWN, gateway = null, gatewayFailed = false))
    }

    @Test
    fun `running status with no gateway yet still shows the interstitial`() {
        assertEquals(WebServiceUiState.Starting, deriveWebServiceUiState(WebServiceStatus.RUNNING, gateway = null, gatewayFailed = false))
    }

    @Test
    fun `running status with a live gateway is ready`() {
        val state = deriveWebServiceUiState(WebServiceStatus.RUNNING, gateway, gatewayFailed = false)
        assertTrue(state is WebServiceUiState.Ready)
        state as WebServiceUiState.Ready
        assertEquals("http://127.0.0.1:45678/", state.url)
        assertTrue(state.cookieHeader.contains("HttpOnly"))
    }

    @Test
    fun `stopped status shows the stopped state even if a gateway happens to be present`() {
        assertEquals(WebServiceUiState.Stopped, deriveWebServiceUiState(WebServiceStatus.STOPPED, gateway, gatewayFailed = false))
    }

    @Test
    fun `failed status shows a failed state`() {
        val state = deriveWebServiceUiState(WebServiceStatus.FAILED, gateway = null, gatewayFailed = false)
        assertTrue(state is WebServiceUiState.Failed)
    }

    @Test
    fun `a gateway bind failure overrides a running item status`() {
        val state = deriveWebServiceUiState(WebServiceStatus.RUNNING, gateway = null, gatewayFailed = true)
        assertTrue(state is WebServiceUiState.Failed)
    }

    @Test
    fun `cookie header carries the gateway token cookie name and HttpOnly, never a bare value`() {
        assertEquals("choosh_gw_token=abc123; Path=/; HttpOnly", gateway.cookieHeader)
    }
}
