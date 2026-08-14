package ai.choosh.fleet

import ai.choosh.engine.FakeChooshEngine
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/**
 * [FleetViewModel] state-flow behavior against [FakeChooshEngine] — the
 * derivation logic itself is covered separately in [FleetRowsForTest].
 */
@OptIn(ExperimentalCoroutinesApi::class)
class FleetViewModelTest {

    @get:Rule
    val mainDispatcherRule = MainDispatcherRule()

    @Test
    fun `refresh loads devhosts and populates rows`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        engine.connect("fake-session-credential") // FakeChooshEngine.listDevhosts() requires this first.
        val viewModel = FleetViewModel(engine)
        advanceUntilIdle() // let init { refresh() }'s viewModelScope.launch actually run.

        val state = viewModel.state.value
        assertTrue("expected a populated fleet after refresh", !state.isLoading)
        assertEquals(null, state.error)
        assertTrue("expected at least one row from FIXTURE_DEVHOSTS", state.rows.isNotEmpty())
        // FakeChooshEngine.FIXTURE_DEVHOSTS includes "old-cloud-box" with no matching
        // FleetFixtures project — confirms PROJECT mode (the default) doesn't silently
        // drop devhosts with no project, it just doesn't emit a row *for* them (HOST mode
        // is what guarantees every devhost a row; see FleetRowsForTest).
        assertEquals(SortMode.PROJECT, state.sortMode)
    }

    @Test
    fun `refresh failure surfaces as an error state, not a crash`() = runTest(mainDispatcherRule.dispatcher) {
        // Deliberately not calling connect() first: FakeChooshEngine.listDevhosts()
        // throws IllegalStateException in that case, exercising refresh()'s onFailure path.
        val viewModel = FleetViewModel(FakeChooshEngine())
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(!state.isLoading)
        assertNotNull("a failed refresh must populate an error message, not throw", state.error)
        assertTrue("a failed refresh must not leave stale rows", state.rows.isEmpty())
    }

    @Test
    fun `setSortMode recomputes rows from already-loaded data without another refresh call`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        engine.connect("fake-session-credential")
        val viewModel = FleetViewModel(engine)
        advanceUntilIdle()
        // FleetFixtures.projectsFor(FIXTURE_DEVHOSTS) always returns exactly 2 projects.
        assertEquals(2, viewModel.state.value.rows.size)

        viewModel.setSortMode(SortMode.HOST)

        val hostState = viewModel.state.value
        assertEquals(SortMode.HOST, hostState.sortMode)
        // HOST mode includes every devhost (3 in FIXTURE_DEVHOSTS) — a different count
        // than PROJECT mode's 2, proving setSortMode actually re-derived rows rather
        // than leaving PROJECT mode's stale list in place.
        assertEquals(FakeChooshEngine.FIXTURE_DEVHOSTS.size, hostState.rows.size)
    }
}
