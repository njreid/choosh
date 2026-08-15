package ai.choosh.markdown

import ai.choosh.fleet.MainDispatcherRule
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

private class FakeMarkdownGatewayController : MarkdownGatewayController {
    var startCalls = 0
    var stopCalls = 0
    var failToStart = false

    override fun start(connectionHandle: Long, targetDeviceId: String, workspaceId: String, docPath: String): MarkdownGatewayHandle? {
        startCalls += 1
        if (failToStart) return null
        return MarkdownGatewayHandle(handle = 7L, MarkdownGatewayInfo(port = 51234, token = "tok"))
    }

    override fun stop(gatewayHandle: Long) {
        stopCalls += 1
    }
}

@OptIn(ExperimentalCoroutinesApi::class)
class MarkdownViewModelTest {
    @get:Rule
    val mainDispatcherRule = MainDispatcherRule()

    @Test
    fun `a live connection starts exactly one gateway and becomes ready`() = runTest(mainDispatcherRule.dispatcher) {
        val gatewayController = FakeMarkdownGatewayController()
        val viewModel = MarkdownViewModel("dev-1", "ws-1", "README.md", connectionHandle = 42L, gatewayController = gatewayController)
        advanceUntilIdle()

        assertTrue(viewModel.state.value is MarkdownUiState.Ready)
        assertEquals(1, gatewayController.startCalls)
    }

    @Test
    fun `no connection handle is a failed state, not a crash`() = runTest(mainDispatcherRule.dispatcher) {
        val gatewayController = FakeMarkdownGatewayController()
        val viewModel = MarkdownViewModel("dev-1", "ws-1", "README.md", connectionHandle = null, gatewayController = gatewayController)
        advanceUntilIdle()

        assertTrue(viewModel.state.value is MarkdownUiState.Failed)
        assertEquals(0, gatewayController.startCalls)
    }

    @Test
    fun `a gateway bind failure is a failed state`() = runTest(mainDispatcherRule.dispatcher) {
        val gatewayController = FakeMarkdownGatewayController().apply { failToStart = true }
        val viewModel = MarkdownViewModel("dev-1", "ws-1", "README.md", connectionHandle = 42L, gatewayController = gatewayController)
        advanceUntilIdle()

        assertTrue(viewModel.state.value is MarkdownUiState.Failed)
    }

    @Test
    fun `onClose stops the gateway`() = runTest(mainDispatcherRule.dispatcher) {
        val gatewayController = FakeMarkdownGatewayController()
        val viewModel = MarkdownViewModel("dev-1", "ws-1", "README.md", connectionHandle = 42L, gatewayController = gatewayController)
        advanceUntilIdle()
        assertTrue(viewModel.state.value is MarkdownUiState.Ready)

        viewModel.onClose()

        assertEquals(1, gatewayController.stopCalls)
    }
}
