package ai.choosh.viewmodel

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Direct coverage for [loadInto] itself, exercised against a small local
 * fake state rather than any of its three adopting ViewModels — those
 * ViewModels' own test suites (e.g. [ai.choosh.fleet.DevHostWorkspacesViewModelTest],
 * [ai.choosh.jj.JjDiffViewModelTest], [ai.choosh.jj.JjChangeGraphViewModelTest])
 * only ever exercise [loadInto] indirectly through their real `refresh()`/`load()`
 * call sites.
 */
private data class FakeState(
    val isLoading: Boolean = false,
    val error: String? = null,
    val value: Int = 0,
)

@OptIn(ExperimentalCoroutinesApi::class)
class StateFlowLoadTest {

    @Test
    fun `the loading flag is true while call is suspended and false again once it succeeds`() = runTest {
        val flow = MutableStateFlow(FakeState())
        val gate = CompletableDeferred<Int>()

        val job = launch {
            flow.loadInto(
                setLoading = { it.copy(isLoading = true, error = null) },
                call = { gate.await() },
                onSuccess = { state, result -> state.copy(isLoading = false, value = result) },
                onFailure = { state, failure -> state.copy(isLoading = false, error = failure.message) },
            )
        }

        // Let loadInto run up to the point where `call` suspends on the gate.
        runCurrent()
        assertTrue("isLoading must be set before call is awaited", flow.value.isLoading)

        gate.complete(42)
        job.join()

        assertFalse("isLoading must be cleared once call succeeds", flow.value.isLoading)
        assertEquals(42, flow.value.value)
    }

    @Test
    fun `onSuccess is invoked with the call's result and lands the returned state`() = runTest {
        val flow = MutableStateFlow(FakeState())

        flow.loadInto(
            setLoading = { it.copy(isLoading = true, error = null) },
            call = { "hello" },
            onSuccess = { state, result -> state.copy(isLoading = false, value = result.length) },
            onFailure = { state, failure -> state.copy(isLoading = false, error = failure.message) },
        )

        val state = flow.value
        assertFalse(state.isLoading)
        assertNull(state.error)
        assertEquals(5, state.value)
    }

    @Test
    fun `a failing call invokes onFailure and still clears the loading flag`() = runTest {
        val flow = MutableStateFlow(FakeState())

        flow.loadInto(
            setLoading = { it.copy(isLoading = true, error = null) },
            call = { error("simulated failure") },
            onSuccess = { state, result -> state.copy(isLoading = false, value = result) },
            onFailure = { state, failure -> state.copy(isLoading = false, error = failure.message) },
        )

        val state = flow.value
        assertFalse("a failed call must not leave the loading flag stuck true", state.isLoading)
        assertEquals("simulated failure", state.error)
        assertEquals("onFailure's returned state should stand, not an onSuccess-shaped one", 0, state.value)
    }

    @Test
    fun `an exception thrown inside call does not propagate out of loadInto and reaches onFailure verbatim`() = runTest {
        val flow = MutableStateFlow(FakeState())
        val thrown = IllegalStateException("boom")
        var observed: Throwable? = null

        // loadInto is documented as wrapping `call` in runCatching, so a thrown exception
        // must not escape this call — it must be routed to onFailure instead.
        flow.loadInto(
            setLoading = { it.copy(isLoading = true, error = null) },
            call = { throw thrown },
            onSuccess = { state, result -> state.copy(isLoading = false, value = result) },
            onFailure = { state, failure ->
                observed = failure
                state.copy(isLoading = false, error = failure.message)
            },
        )

        assertSame("onFailure should receive the exact throwable call raised", thrown, observed)
        assertFalse(flow.value.isLoading)
        assertEquals("boom", flow.value.error)
    }
}
