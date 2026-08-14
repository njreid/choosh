package ai.choosh.engine

import kotlinx.serialization.Serializable

/**
 * The Kotlin-facing contract for the Rust engine, per DESIGN.md's "Rust owns
 * durable state; views are projections" principle. Every method's wire
 * payload is JSON matching `choosh_protocol::relay`'s shared types
 * (docs/specs/relay-protocol.md, docs/specs/auth-and-enrollment.md) — this
 * interface just gives Kotlin callers a typed surface over that JSON rather
 * than passing raw strings around application code.
 *
 * Two implementations: [NativeChooshEngine] (real, JNI-backed) and
 * [FakeChooshEngine] (in-memory, for previews/tests/early UI development).
 * The composition root in [ai.choosh.ChooshApp] is the single place that
 * chooses which one the rest of the app sees.
 */
interface ChooshEngine {
    /** Starts a WebAuthn passkey registration ceremony. Returns creation-options JSON. */
    suspend fun webauthnRegisterStart(): String

    /** Finishes registration with the Credential Manager response JSON; returns a [WebauthnResult]. */
    suspend fun webauthnRegisterFinish(credentialJson: String): WebauthnResult

    /** Starts a WebAuthn passkey login (assertion) ceremony. Returns request-options JSON. */
    suspend fun webauthnLoginStart(): String

    /** Finishes login with the Credential Manager response JSON; returns a [WebauthnResult]. */
    suspend fun webauthnLoginFinish(credentialJson: String): WebauthnResult

    /** Opens the persistent relay connection using a stored session credential. */
    suspend fun connect(sessionCredential: String): Boolean

    /** Lists every devhost visible to this authenticated connection. */
    suspend fun listDevhosts(): List<DevHostPresence>

    /**
     * Registers this phone's current FCM token with `relayd`, per
     * notifications.md — replaces any previously registered token. `false`
     * if not connected or the call fails; callers should retry after the
     * next successful [connect] rather than treat this as fatal.
     */
    suspend fun registerFcmToken(fcmToken: String): Boolean

    /** Closes the relay connection. Idempotent. */
    fun close()
}

/**
 * `relayd`'s WebAuthn HTTP endpoints return either a success payload or a
 * typed failure — this mirrors that rather than throwing for an expected
 * rejection (a stale/invalid ceremony response is not exceptional, callers
 * are expected to handle it as UI state).
 */
sealed interface WebauthnResult {
    @Serializable
    data class Success(val sessionCredential: String) : WebauthnResult

    @Serializable
    data class Failure(val code: String, val message: String) : WebauthnResult
}

/** Mirrors `choosh_protocol::relay::DevHostPresence` exactly (see relay-protocol.md). */
@Serializable
data class DevHostPresence(
    val deviceId: String,
    val alias: String,
    val platform: String,
    val accountLabel: String?,
    val connectionState: ConnectionState,
    val lastSeen: String,
)

enum class ConnectionState { ONLINE, OFFLINE }
