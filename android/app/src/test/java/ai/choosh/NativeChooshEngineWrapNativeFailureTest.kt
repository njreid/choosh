package ai.choosh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * [wrapNativeFailure] against the specific gap found during a dedicated
 * JNI-boundary review: no call site in [NativeChooshEngine] previously
 * handled an unexpected native-side failure at all, so it would have
 * propagated as an unrecognized exception type and crashed the app
 * uncaught rather than degrading to the same [IllegalStateException]
 * shape every caller already handles for [decodeOrThrow]'s `{"error":
 * ...}` convention. [NativeChooshEngine] itself can't be constructed in a
 * plain JVM unit test (see [NativeChooshEngineEnrollmentTokenDecodeTest]'s
 * doc comment), so this exercises the wrapper directly instead.
 */
class NativeChooshEngineWrapNativeFailureTest {

    @Test
    fun `a successful block returns its value unchanged`() {
        assertEquals("ok", wrapNativeFailure { "ok" })
    }

    @Test
    fun `an IllegalStateException from decodeOrThrow's error convention passes through completely unchanged`() {
        val original = IllegalStateException("hostd rejected the request")

        val thrown = runCatching { wrapNativeFailure<Unit> { throw original } }.exceptionOrNull()

        assertSame("must not re-wrap a failure already in the expected shape", original, thrown)
    }

    @Test
    fun `an unexpected native-side exception degrades to the same IllegalStateException shape callers already handle`() {
        val nativeFailure = RuntimeException("JNI-level failure")

        val thrown = runCatching { wrapNativeFailure<Unit> { throw nativeFailure } }.exceptionOrNull()

        assertTrue("must degrade to IllegalStateException, not propagate the raw native exception type", thrown is IllegalStateException)
        assertSame("original failure must be preserved as the cause, not discarded", nativeFailure, thrown?.cause)
        assertTrue(thrown?.message.orEmpty().contains("JNI-level failure"))
    }
}
