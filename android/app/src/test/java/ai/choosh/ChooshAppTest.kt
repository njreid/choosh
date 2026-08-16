package ai.choosh

import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.FakeChooshEngine
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Regression coverage for the real, live bug [EngineHolderViewModel]'s own
 * doc comment describes: a native engine handle (Tokio runtime + live
 * relayd WebSocket) leaking on every configuration change, because nothing
 * anywhere in the app ever called [ChooshEngine.close]. Found during a
 * dedicated JNI-boundary review.
 */
class ChooshAppTest {
    @Test
    fun `engine is built exactly once and the same instance is returned on repeated access`() {
        var buildCount = 0
        val viewModel = EngineHolderViewModel(buildEngine = { buildCount++; FakeChooshEngine() })

        val first = viewModel.engine
        val second = viewModel.engine

        assertSame("must not rebuild the engine on repeated access", first, second)
        assertTrue("must build exactly once, not once per access", buildCount == 1)
    }

    @Test
    fun `onCleared closes the engine`() {
        // A real, cross-thread wait (not a TestDispatcher) — onCleared's own
        // doc comment explains why it deliberately uses GlobalScope +
        // Dispatchers.IO rather than a scope this test could control
        // synchronously: onCleared() is not a suspend function, and
        // viewModelScope is already being cancelled by the time it runs.
        val closedLatch = CountDownLatch(1)
        val engine = object : ChooshEngine by FakeChooshEngine() {
            override suspend fun close() {
                closedLatch.countDown()
            }
        }
        val viewModel = EngineHolderViewModel(buildEngine = { engine })
        viewModel.engine // force creation, mirroring real usage

        viewModel.onCleared()

        assertTrue(
            "onCleared must close the engine — this is the one real place close() is exercised, per the fix's own doc comment",
            closedLatch.await(5, TimeUnit.SECONDS),
        )
    }
}
