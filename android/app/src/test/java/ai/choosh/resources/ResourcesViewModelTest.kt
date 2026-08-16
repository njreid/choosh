package ai.choosh.resources

import ai.choosh.engine.FakeChooshEngine
import ai.choosh.engine.MobileProfile
import ai.choosh.engine.ResourcePattern
import ai.choosh.fleet.MainDispatcherRule
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/**
 * [ResourcesViewModel] against [FakeChooshEngine] — covers `resource.list`'s
 * loading/error/success states and the `resource.propose` ->
 * `resource.confirm` two-step round trip this screen's own "declare a
 * Resource directly" form exercises, per [ResourcesViewModel]'s own doc
 * comment on why that round trip is never auto-confirmed.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ResourcesViewModelTest {

    @get:Rule
    val mainDispatcherRule = MainDispatcherRule()

    private val deviceId = FakeChooshEngine.FIXTURE_DEVICE_ID

    @Test
    fun `refresh loads the fixture resource list`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        engine.connect("fake-session-credential")
        val viewModel = ResourcesViewModel(engine, deviceId)
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(!state.isLoading)
        assertNull(state.error)
        assertTrue("expected at least one fixture resource", state.resources.isNotEmpty())
        assertTrue("expected at least one non-reauth (pattern = null) fixture resource", state.resources.any { it.pattern == null })
    }

    @Test
    fun `propose queues a pending self proposal without yet appearing in resources`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        engine.connect("fake-session-credential")
        val viewModel = ResourcesViewModel(engine, deviceId)
        advanceUntilIdle()
        val resourceCountBefore = viewModel.state.value.resources.size

        viewModel.propose("New Resource", "custom", ResourcePattern.C, "some reauth command", MobileProfile.ASK)
        advanceUntilIdle()

        val state = viewModel.state.value
        assertEquals(1, state.pendingSelfProposals.size)
        assertEquals("New Resource", state.pendingSelfProposals[0].displayName)
        assertEquals("propose alone must not add to the confirmed list", resourceCountBefore, state.resources.size)
    }

    @Test
    fun `confirmSelfProposal with approve = true moves the proposal into the confirmed resource list`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        engine.connect("fake-session-credential")
        val viewModel = ResourcesViewModel(engine, deviceId)
        advanceUntilIdle()
        viewModel.propose("New Resource", "custom", null, null, MobileProfile.ASK)
        advanceUntilIdle()
        val resourceId = viewModel.state.value.pendingSelfProposals.single().resourceId

        viewModel.confirmSelfProposal(resourceId, approve = true)
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue("approving must clear the pending proposal", state.pendingSelfProposals.isEmpty())
        assertTrue("the newly-confirmed resource must now be listed", state.resources.any { it.resourceId == resourceId })
    }

    @Test
    fun `confirmSelfProposal with approve = false discards the proposal without listing it`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        engine.connect("fake-session-credential")
        val viewModel = ResourcesViewModel(engine, deviceId)
        advanceUntilIdle()
        viewModel.propose("Rejected Resource", "custom", null, null, MobileProfile.ASK)
        advanceUntilIdle()
        val resourceId = viewModel.state.value.pendingSelfProposals.single().resourceId

        viewModel.confirmSelfProposal(resourceId, approve = false)
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(state.pendingSelfProposals.isEmpty())
        assertTrue(state.resources.none { it.resourceId == resourceId })
    }

    @Test
    fun `a propose failure surfaces as proposeError, not a crash`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        engine.connect("fake-session-credential")
        engine.simulateResourceRpcFailure = "simulated resource.propose failure"
        val viewModel = ResourcesViewModel(engine, deviceId)
        advanceUntilIdle()

        viewModel.propose("New Resource", "custom", null, null, MobileProfile.ASK)
        advanceUntilIdle()

        assertEquals("simulated resource.propose failure", viewModel.state.value.proposeError)
        assertTrue(viewModel.state.value.pendingSelfProposals.isEmpty())
    }

    @Test
    fun `propose rejects a blank display name before touching the engine`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        engine.connect("fake-session-credential")
        val viewModel = ResourcesViewModel(engine, deviceId)
        advanceUntilIdle()

        viewModel.propose("", "custom", null, null, MobileProfile.ASK)
        advanceUntilIdle()

        assertTrue(viewModel.state.value.pendingSelfProposals.isEmpty())
        assertTrue(viewModel.state.value.proposeError != null)
    }
}
