package ai.choosh.jj

import ai.choosh.engine.DiffFileEntry
import ai.choosh.engine.FakeChooshEngine
import ai.choosh.fleet.MainDispatcherRule
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class JjDiffViewModelTest {

    @get:Rule
    val mainDispatcherRule = MainDispatcherRule()

    @Test
    fun `loads the default from-to diff on init`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = JjDiffViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(!state.isLoading)
        assertEquals(null, state.error)
        assertTrue("expected FakeChooshEngine's fixture diff, including a rename", state.files.any {
            it is DiffFileEntry.Hunks && it.oldPath != null && it.newPath != null && it.oldPath != it.newPath
        })
        assertTrue("expected a binary file entry", state.files.any { it is DiffFileEntry.Binary })
    }

    @Test
    fun `setFrom and setTo update state and load reflects the chosen revisions`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = JjDiffViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()

        viewModel.setFrom("change-a")
        viewModel.setTo("change-merge")
        assertEquals("change-a", viewModel.state.value.from)
        assertEquals("change-merge", viewModel.state.value.to)

        viewModel.load()
        advanceUntilIdle()

        assertTrue(!viewModel.state.value.isLoading)
        assertEquals(null, viewModel.state.value.error)
    }
}
