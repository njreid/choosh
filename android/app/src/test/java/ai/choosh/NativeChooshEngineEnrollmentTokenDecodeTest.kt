package ai.choosh

import ai.choosh.engine.EnrollmentTokenResult
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * [decodeEnrollmentTokenResult] against real wire shapes
 * `Engine::request_enrollment_token` (rust/choosh-android-bridge/src/engine.rs)
 * actually produces — success (`{"token": ..., "expires_at": ...}`) and this
 * module's shared `{"error": ...}` failure shape, mirroring
 * [NativeChooshEngine]'s doc comment on why this decode function is
 * `internal` rather than `private`: [NativeChooshEngine] itself can't be
 * constructed in a plain JVM unit test (its constructor eagerly calls
 * `NativeBridge.nativeInit`, which needs the real native `.so`), so this
 * exercises the decode logic directly instead.
 */
class NativeChooshEngineEnrollmentTokenDecodeTest {

    @Test
    fun `decodes a real success response into EnrollmentTokenResult Success`() {
        val raw = """{"token":"tok-abc123","expires_at":"2026-08-16T00:15:00Z"}"""

        val result = decodeEnrollmentTokenResult(raw)

        assertTrue("expected Success, got $result", result is EnrollmentTokenResult.Success)
        val success = result as EnrollmentTokenResult.Success
        assertEquals("tok-abc123", success.token)
        assertEquals("2026-08-16T00:15:00Z", success.expiresAt)
    }

    @Test
    fun `decodes the shared error shape into EnrollmentTokenResult Failure`() {
        val raw = """{"error":"not connected: call nativeConnect first"}"""

        val result = decodeEnrollmentTokenResult(raw)

        assertTrue("expected Failure, got $result", result is EnrollmentTokenResult.Failure)
        assertEquals("not connected: call nativeConnect first", (result as EnrollmentTokenResult.Failure).message)
    }

    @Test
    fun `a server-side rejection also decodes as Failure with its own message`() {
        val raw = """{"error":"request_enrollment_token failed: server rejected request: internal: boom"}"""

        val result = decodeEnrollmentTokenResult(raw)

        assertTrue(result is EnrollmentTokenResult.Failure)
        assertTrue((result as EnrollmentTokenResult.Failure).message.contains("boom"))
    }

    @Test
    fun `a malformed body with neither token nor error still falls back to a Failure, not a crash`() {
        val raw = """{}"""

        val result = decodeEnrollmentTokenResult(raw)

        assertTrue("expected a fallback Failure rather than a thrown exception", result is EnrollmentTokenResult.Failure)
    }
}
