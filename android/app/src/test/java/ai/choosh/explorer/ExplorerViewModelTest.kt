package ai.choosh.explorer

import ai.choosh.engine.FakeChooshEngine
import ai.choosh.fleet.MainDispatcherRule
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class ExplorerViewModelTest {

    @get:Rule
    val mainDispatcherRule = MainDispatcherRule()

    @Test
    fun `refresh loads the changed-files summary including a conflicted path`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(!state.isLoading)
        assertNull(state.error)
        assertTrue(state.changed.isNotEmpty())
        assertEquals(1, state.conflicted.size)
        assertTrue(state.changed.any { it.path == state.conflicted.first() })
    }
}
