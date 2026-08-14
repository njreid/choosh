package ai.choosh.engine

import kotlinx.coroutines.delay

/**
 * In-memory [ChooshEngine] for previews, UI tests, and early UI development
 * against no real backend. The WebAuthn ceremony always "succeeds" here —
 * the real Credential Manager call still happens in
 * [ai.choosh.connection.ConnectionScreen] (this fake only stands in for the
 * server round-trip either side of it), so the passkey UI path is exercised
 * for real even though this pass doesn't wire it to a live `choosh-relayd`.
 *
 * The devhost/Project/Workspace fixture data here deliberately includes a
 * mix of online/offline hosts and at least one attention-needing workspace
 * so the fleet drawer's every row state (per docs/specs/android-navigation.md)
 * is exercisable without a real backend.
 */
class FakeChooshEngine : ChooshEngine {
    private var connected = false

    override suspend fun webauthnRegisterStart(): String {
        delay(FAKE_LATENCY_MS)
        return """{"challenge":"fake-challenge","rp":{"id":"choosh.local"}}"""
    }

    override suspend fun webauthnRegisterFinish(credentialJson: String): WebauthnResult {
        delay(FAKE_LATENCY_MS)
        return WebauthnResult.Success(sessionCredential = "fake-session-credential")
    }

    override suspend fun webauthnLoginStart(): String {
        delay(FAKE_LATENCY_MS)
        return """{"challenge":"fake-challenge"}"""
    }

    override suspend fun webauthnLoginFinish(credentialJson: String): WebauthnResult {
        delay(FAKE_LATENCY_MS)
        return WebauthnResult.Success(sessionCredential = "fake-session-credential")
    }

    override suspend fun connect(sessionCredential: String): Boolean {
        delay(FAKE_LATENCY_MS)
        connected = true
        return true
    }

    override suspend fun listDevhosts(): List<DevHostPresence> {
        delay(FAKE_LATENCY_MS)
        check(connected) { "listDevhosts() called before connect() succeeded" }
        return FIXTURE_DEVHOSTS
    }

    override fun close() {
        connected = false
    }

    companion object {
        private const val FAKE_LATENCY_MS = 120L

        val FIXTURE_DEVHOSTS = listOf(
            DevHostPresence(
                deviceId = "dev-mbp-home",
                alias = "mbp-home",
                platform = "macos",
                accountLabel = null,
                connectionState = ConnectionState.ONLINE,
                lastSeen = "2026-08-14T20:00:00Z",
            ),
            DevHostPresence(
                deviceId = "dev-build-box-large",
                alias = "build-box-large",
                platform = "linux",
                accountLabel = "aws:123456789012",
                connectionState = ConnectionState.ONLINE,
                lastSeen = "2026-08-14T19:58:00Z",
            ),
            DevHostPresence(
                deviceId = "dev-old-cloud-box",
                alias = "old-cloud-box",
                platform = "linux",
                accountLabel = "aws:987654321098",
                connectionState = ConnectionState.OFFLINE,
                lastSeen = "2026-08-10T12:00:00Z",
            ),
        )
    }
}
