package ai.choosh.engine

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * [FakeChooshEngine.requestEnrollmentToken] directly, independent of
 * [ai.choosh.fleet.FleetViewModel] (which already exercises it indirectly in
 * `FleetViewModelTest`) — confirms the fake's own not-connected/failure/
 * success behavior in isolation, matching this fake's other methods'
 * "genuine behavior, not a static stub" precedent (e.g.
 * `workspaceOpUndo`/`setPrimaryWorkspace`).
 */
class FakeChooshEngineEnrollmentTokenTest {

    @Test
    fun `returns a Failure before connect`() = runTest {
        val engine = FakeChooshEngine()

        val result = engine.requestEnrollmentToken()

        assertTrue(result is EnrollmentTokenResult.Failure)
        assertTrue((result as EnrollmentTokenResult.Failure).message.contains("not connected"))
    }

    @Test
    fun `returns a plausible Success token and a future expiry once connected`() = runTest {
        val engine = FakeChooshEngine()
        engine.connect("fake-session-credential")

        val result = engine.requestEnrollmentToken()

        assertTrue(result is EnrollmentTokenResult.Success)
        val success = result as EnrollmentTokenResult.Success
        assertTrue("expected a non-blank fake token", success.token.isNotBlank())
        assertTrue("expected a non-blank, parseable expiry", success.expiresAt.isNotBlank())
        java.time.Instant.parse(success.expiresAt) // throws if not a real ISO-8601 instant
    }

    @Test
    fun `each call mints a distinct token, per single-use semantics`() = runTest {
        val engine = FakeChooshEngine()
        engine.connect("fake-session-credential")

        val first = engine.requestEnrollmentToken() as EnrollmentTokenResult.Success
        val second = engine.requestEnrollmentToken() as EnrollmentTokenResult.Success

        assertNotEquals(first.token, second.token)
    }

    @Test
    fun `simulateEnrollmentTokenFailure forces a Failure without disconnecting`() = runTest {
        val engine = FakeChooshEngine()
        engine.connect("fake-session-credential")
        engine.simulateEnrollmentTokenFailure = true

        val result = engine.requestEnrollmentToken()

        assertTrue(result is EnrollmentTokenResult.Failure)
    }
}
