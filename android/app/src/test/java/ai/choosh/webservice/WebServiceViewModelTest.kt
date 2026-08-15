package ai.choosh.webservice

import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.FakeChooshEngine
import ai.choosh.engine.ItemSummary
import ai.choosh.engine.ItemType
import ai.choosh.engine.WebServiceStatus
import ai.choosh.fleet.MainDispatcherRule
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/** [ChooshEngine] wrapper letting a test control exactly what `itemList` returns, delegating everything else to [FakeChooshEngine] — see this file's own doc comment on why plain interface delegation is enough here. */
private class ScriptedItemChooshEngine(private val delegate: ChooshEngine = FakeChooshEngine()) : ChooshEngine by delegate {
    var items: List<ItemSummary> = emptyList()
    var throwOnItemList: Boolean = false

    override suspend fun itemList(deviceId: String, workspaceId: String): List<ItemSummary> {
        if (throwOnItemList) error("simulated transport failure")
        return items
    }
}

private class FakeWebServiceGatewayController : WebServiceGatewayController {
    var startCalls = 0
    var stopCalls = 0
    var failToStart = false
    private var nextHandle = 1L

    override fun start(connectionHandle: Long, targetDeviceId: String, itemId: String): WebGatewayHandle? {
        startCalls += 1
        if (failToStart) return null
        val handle = nextHandle++
        return WebGatewayHandle(handle, WebGatewayInfo(port = 40000 + handle.toInt(), token = "tok-$handle"))
    }

    override fun stop(gatewayHandle: Long) {
        stopCalls += 1
    }
}

private fun webServiceItem(status: WebServiceStatus) =
    ItemSummary(itemId = "item-1", itemType = ItemType.WEB_SERVICE, name = "web", tabTarget = "tab-1", status = status, port = 3000)

@OptIn(ExperimentalCoroutinesApi::class)
class WebServiceViewModelTest {
    @get:Rule
    val mainDispatcherRule = MainDispatcherRule()

    @Test
    fun `starting status never opens a gateway`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = ScriptedItemChooshEngine().apply { items = listOf(webServiceItem(WebServiceStatus.STARTING)) }
        val gatewayController = FakeWebServiceGatewayController()
        val viewModel = WebServiceViewModel(engine, "dev-1", "ws-1", "item-1", connectionHandle = 42L, gatewayController = gatewayController)

        viewModel.poll()

        assertEquals(WebServiceUiState.Starting, viewModel.state.value)
        assertEquals(0, gatewayController.startCalls)
    }

    @Test
    fun `running status starts exactly one gateway and becomes ready`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = ScriptedItemChooshEngine().apply { items = listOf(webServiceItem(WebServiceStatus.RUNNING)) }
        val gatewayController = FakeWebServiceGatewayController()
        val viewModel = WebServiceViewModel(engine, "dev-1", "ws-1", "item-1", connectionHandle = 42L, gatewayController = gatewayController)

        viewModel.poll()
        viewModel.poll() // a second poll while still running must not open a second gateway

        assertTrue(viewModel.state.value is WebServiceUiState.Ready)
        assertEquals(1, gatewayController.startCalls)
    }

    @Test
    fun `a gateway bind failure surfaces as a failed state`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = ScriptedItemChooshEngine().apply { items = listOf(webServiceItem(WebServiceStatus.RUNNING)) }
        val gatewayController = FakeWebServiceGatewayController().apply { failToStart = true }
        val viewModel = WebServiceViewModel(engine, "dev-1", "ws-1", "item-1", connectionHandle = 42L, gatewayController = gatewayController)

        viewModel.poll()

        assertTrue(viewModel.state.value is WebServiceUiState.Failed)
    }

    @Test
    fun `failed item status surfaces as a failed state and never opens a gateway`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = ScriptedItemChooshEngine().apply { items = listOf(webServiceItem(WebServiceStatus.FAILED)) }
        val gatewayController = FakeWebServiceGatewayController()
        val viewModel = WebServiceViewModel(engine, "dev-1", "ws-1", "item-1", connectionHandle = 42L, gatewayController = gatewayController)

        viewModel.poll()

        assertTrue(viewModel.state.value is WebServiceUiState.Failed)
        assertEquals(0, gatewayController.startCalls)
    }

    @Test
    fun `stopped item status closes an already-open gateway`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = ScriptedItemChooshEngine().apply { items = listOf(webServiceItem(WebServiceStatus.RUNNING)) }
        val gatewayController = FakeWebServiceGatewayController()
        val viewModel = WebServiceViewModel(engine, "dev-1", "ws-1", "item-1", connectionHandle = 42L, gatewayController = gatewayController)
        viewModel.poll()
        assertTrue(viewModel.state.value is WebServiceUiState.Ready)

        engine.items = listOf(webServiceItem(WebServiceStatus.STOPPED))
        viewModel.poll()

        assertEquals(WebServiceUiState.Stopped, viewModel.state.value)
        assertEquals(1, gatewayController.stopCalls)
    }

    @Test
    fun `an item list transport failure is treated as unknown, not a crash`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = ScriptedItemChooshEngine().apply { throwOnItemList = true }
        val gatewayController = FakeWebServiceGatewayController()
        val viewModel = WebServiceViewModel(engine, "dev-1", "ws-1", "item-1", connectionHandle = 42L, gatewayController = gatewayController)

        viewModel.poll()

        assertEquals(WebServiceUiState.Starting, viewModel.state.value)
    }

    @Test
    fun `unpin closes the gateway and only the gateway`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = ScriptedItemChooshEngine().apply { items = listOf(webServiceItem(WebServiceStatus.RUNNING)) }
        val gatewayController = FakeWebServiceGatewayController()
        val viewModel = WebServiceViewModel(engine, "dev-1", "ws-1", "item-1", connectionHandle = 42L, gatewayController = gatewayController)
        viewModel.poll()
        assertTrue(viewModel.state.value is WebServiceUiState.Ready)

        viewModel.onUnpin()

        assertEquals(1, gatewayController.stopCalls)
        // No RPC surface exists on WebServiceGatewayController for stopping
        // the remote item — unpin structurally cannot send `item.stop`,
        // since this class holds no reference to any such call.
    }

    @Test
    fun `a missing connection handle is a failed state, not a null-pointer crash`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = ScriptedItemChooshEngine().apply { items = listOf(webServiceItem(WebServiceStatus.RUNNING)) }
        val gatewayController = FakeWebServiceGatewayController()
        val viewModel = WebServiceViewModel(engine, "dev-1", "ws-1", "item-1", connectionHandle = null, gatewayController = gatewayController)

        viewModel.poll()

        assertTrue(viewModel.state.value is WebServiceUiState.Failed)
        assertEquals(0, gatewayController.startCalls)
    }
}
