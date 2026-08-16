package ai.choosh.webservice

import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.FakeChooshEngine
import ai.choosh.engine.ItemSummary
import ai.choosh.engine.ItemType
import ai.choosh.engine.WebServiceStatus
import ai.choosh.fleet.MainDispatcherRule
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceUntilIdle
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

    // --- WebServiceViewModel.retry() (UX-friction audit finding #7) -------
    //
    // ensureGatewayStarted()'s own guard used to mean a WebGatewayHandleState.Failed
    // gateway never got another attempt, automatically or otherwise, even once the
    // underlying failure (e.g. a transient bind failure) was gone. retry() is the fix.

    @Test
    fun `retry after a gateway bind failure re-attempts and can succeed`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = ScriptedItemChooshEngine().apply { items = listOf(webServiceItem(WebServiceStatus.RUNNING)) }
        val gatewayController = FakeWebServiceGatewayController().apply { failToStart = true }
        val viewModel = WebServiceViewModel(engine, "dev-1", "ws-1", "item-1", connectionHandle = 42L, gatewayController = gatewayController)
        viewModel.poll()
        assertTrue("a bind failure must surface as Failed", viewModel.state.value is WebServiceUiState.Failed)
        assertEquals(1, gatewayController.startCalls)

        // The underlying failure is gone now (e.g. the port freed up) — retry() must actually
        // attempt again, which ensureGatewayStarted()'s own guard alone could never do from Failed.
        gatewayController.failToStart = false
        viewModel.retry()
        advanceUntilIdle()

        assertTrue("retry must transition out of Failed once the gateway actually starts", viewModel.state.value is WebServiceUiState.Ready)
        assertEquals("retry must issue a fresh start() call, not reuse the failed attempt", 2, gatewayController.startCalls)
    }

    @Test
    fun `retry while still failing surfaces Failed again, not a crash`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = ScriptedItemChooshEngine().apply { items = listOf(webServiceItem(WebServiceStatus.RUNNING)) }
        val gatewayController = FakeWebServiceGatewayController().apply { failToStart = true }
        val viewModel = WebServiceViewModel(engine, "dev-1", "ws-1", "item-1", connectionHandle = 42L, gatewayController = gatewayController)
        viewModel.poll()
        assertTrue(viewModel.state.value is WebServiceUiState.Failed)

        viewModel.retry()
        advanceUntilIdle()

        assertTrue("still failing must stay Failed, not silently succeed", viewModel.state.value is WebServiceUiState.Failed)
        assertEquals(2, gatewayController.startCalls)
    }

    @Test
    fun `retry is a no-op when the gateway never failed`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = ScriptedItemChooshEngine().apply { items = listOf(webServiceItem(WebServiceStatus.RUNNING)) }
        val gatewayController = FakeWebServiceGatewayController()
        val viewModel = WebServiceViewModel(engine, "dev-1", "ws-1", "item-1", connectionHandle = 42L, gatewayController = gatewayController)
        viewModel.poll()
        assertTrue("expected a successful gateway start", viewModel.state.value is WebServiceUiState.Ready)
        assertEquals(1, gatewayController.startCalls)

        // Nothing failed — retry() must not tear down or re-start an already-working gateway.
        viewModel.retry()
        advanceUntilIdle()

        assertTrue(viewModel.state.value is WebServiceUiState.Ready)
        assertEquals("retry() on a healthy gateway must not issue a second start() call", 1, gatewayController.startCalls)
    }

    @Test
    fun `FakeChooshEngine's own itemList polling cycle transitions from starting to running`() = runTest(mainDispatcherRule.dispatcher) {
        // Every other test in this class uses ScriptedItemChooshEngine, which bypasses
        // FakeChooshEngine.itemList() entirely in favor of a test-scripted result — so none
        // of them actually exercise FakeChooshEngine's own "starting for the first two polls,
        // running after" fixture cycle (see that class's itemList() doc comment). This test
        // drives a real FakeChooshEngine end to end specifically to close that gap.
        val engine = FakeChooshEngine()
        val gatewayController = FakeWebServiceGatewayController()
        val viewModel = WebServiceViewModel(
            engine,
            FakeChooshEngine.FIXTURE_DEVICE_ID,
            FakeChooshEngine.FIXTURE_WORKSPACE_ID,
            itemId = "item-web-1",
            connectionHandle = 42L,
            gatewayController = gatewayController,
        )

        viewModel.poll()
        assertEquals(WebServiceUiState.Starting, viewModel.state.value)
        assertEquals(0, gatewayController.startCalls)

        viewModel.poll()
        assertEquals(
            "FakeChooshEngine.itemList() returns STARTING for exactly the first two polls",
            WebServiceUiState.Starting,
            viewModel.state.value,
        )
        assertEquals(0, gatewayController.startCalls)

        viewModel.poll()
        assertTrue(
            "the third poll should observe RUNNING and open the gateway",
            viewModel.state.value is WebServiceUiState.Ready,
        )
        assertEquals(1, gatewayController.startCalls)
    }
}
