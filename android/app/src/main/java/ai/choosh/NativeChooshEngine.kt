package ai.choosh

import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.ConnectionState
import ai.choosh.engine.DevHostPresence
import ai.choosh.engine.WebauthnResult
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * The real, JNI-backed [ChooshEngine] implementation, wrapping
 * `rust/choosh-android-bridge`'s native surface (a sibling increment to this
 * one owns that crate — see its report for the exact function set actually
 * shipped; this class is written against the contract this fork specified
 * for it, and the JNI method names below are deliberately kept in this
 * top-level `ai.choosh` package, not a subpackage, so the
 * `Java_ai_choosh_NativeChooshEngine_native...` symbol names the native side
 * needs to export match exactly rather than needing package-segment
 * reconciliation).
 *
 * Every `native*` call is blocking (it crosses into Rust, which owns its own
 * async runtime on the other side of the JNI boundary) — each is wrapped in
 * `withContext(Dispatchers.IO)` here so it never runs on the caller's
 * dispatcher, matching how this codebase's Rust engine has always been the
 * one that owns blocking/async boundary decisions, not Compose.
 */
class NativeChooshEngine : ChooshEngine {

    private external fun nativeInit(relayUrl: String)
    private external fun nativeWebauthnRegisterStart(): String
    private external fun nativeWebauthnRegisterFinish(credentialJson: String): String
    private external fun nativeWebauthnLoginStart(): String
    private external fun nativeWebauthnLoginFinish(credentialJson: String): String
    private external fun nativeConnect(sessionCredential: String): Boolean
    private external fun nativeListDevhosts(): String
    private external fun nativeClose()

    init {
        nativeInit(BuildConfig.CHOOSH_RELAYD_URL)
    }

    override suspend fun webauthnRegisterStart(): String =
        withContext(Dispatchers.IO) { nativeWebauthnRegisterStart() }

    override suspend fun webauthnRegisterFinish(credentialJson: String): WebauthnResult =
        withContext(Dispatchers.IO) { decodeWebauthnResult(nativeWebauthnRegisterFinish(credentialJson)) }

    override suspend fun webauthnLoginStart(): String =
        withContext(Dispatchers.IO) { nativeWebauthnLoginStart() }

    override suspend fun webauthnLoginFinish(credentialJson: String): WebauthnResult =
        withContext(Dispatchers.IO) { decodeWebauthnResult(nativeWebauthnLoginFinish(credentialJson)) }

    override suspend fun connect(sessionCredential: String): Boolean =
        withContext(Dispatchers.IO) { nativeConnect(sessionCredential) }

    override suspend fun listDevhosts(): List<DevHostPresence> = withContext(Dispatchers.IO) {
        json.decodeFromString<List<WireDevHostPresence>>(nativeListDevhosts()).map { it.toDomain() }
    }

    override fun close() = nativeClose()

    companion object {
        init { System.loadLibrary("choosh_android_bridge") }
        private val json = Json { ignoreUnknownKeys = true }
    }
}

/**
 * Matches the wire shape `relayd`/`hostd` actually serialize
 * (`choosh_protocol::relay::DevHostPresence`'s `snake_case` JSON field
 * names) before translating into this module's `camelCase` domain type.
 */
@Serializable
private data class WireDevHostPresence(
    val device_id: String,
    val alias: String,
    val platform: String,
    val account_label: String?,
    val connection_state: String,
    val last_seen: String,
) {
    fun toDomain() = DevHostPresence(
        deviceId = device_id,
        alias = alias,
        platform = platform,
        accountLabel = account_label,
        connectionState = if (connection_state == "online") ConnectionState.ONLINE else ConnectionState.OFFLINE,
        lastSeen = last_seen,
    )
}

@Serializable
private data class WireWebauthnResult(
    val ok: Boolean,
    val session_credential: String? = null,
    val code: String? = null,
    val message: String? = null,
)

private fun decodeWebauthnResult(raw: String): WebauthnResult {
    val json = Json { ignoreUnknownKeys = true }
    val wire = json.decodeFromString<WireWebauthnResult>(raw)
    return if (wire.ok && wire.session_credential != null) {
        WebauthnResult.Success(wire.session_credential)
    } else {
        WebauthnResult.Failure(wire.code ?: "unknown", wire.message ?: "native engine returned no message")
    }
}
