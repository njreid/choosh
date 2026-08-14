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
 * `rust/choosh-android-bridge`'s native surface.
 *
 * Every native method there is `static`, takes an explicit `handle: jlong`
 * (except `native_init`, which *produces* that handle), and — because
 * standard JNI symbol resolution (`Java_<package>_<class>_<method>`) binds
 * a native implementation to the exact class the `external fun` is declared
 * in — must be declared inside a Kotlin type literally named
 * `ai.choosh.NativeBridge`, matching every `java_type = "ai.choosh.NativeBridge"`
 * in `lib.rs`'s `native_method!` calls exactly. `NativeChooshEngine` itself
 * is *not* that class (a prior pass declared these as instance methods
 * directly on `NativeChooshEngine`, with mismatched arity/return types on
 * top of the wrong class name — that binding could never have resolved at
 * runtime; it just went unnoticed because the composition root has always
 * defaulted to `FakeChooshEngine`, so `NativeChooshEngine` was never
 * actually instantiated). [NativeBridge] below is the real JNI surface;
 * this class owns the per-instance `handle` and adapts it to [ChooshEngine].
 */
class NativeChooshEngine : ChooshEngine {

    private val handle: Long = NativeBridge.nativeInit(BuildConfig.CHOOSH_RELAYD_HTTP_URL, BuildConfig.CHOOSH_RELAYD_URL)

    override suspend fun webauthnRegisterStart(): String =
        withContext(Dispatchers.IO) { NativeBridge.nativeWebauthnRegisterStart(handle) }

    override suspend fun webauthnRegisterFinish(credentialJson: String): WebauthnResult =
        withContext(Dispatchers.IO) { decodeWebauthnResult(NativeBridge.nativeWebauthnRegisterFinish(handle, credentialJson)) }

    override suspend fun webauthnLoginStart(): String =
        withContext(Dispatchers.IO) { NativeBridge.nativeWebauthnLoginStart(handle) }

    override suspend fun webauthnLoginFinish(credentialJson: String): WebauthnResult =
        withContext(Dispatchers.IO) { decodeWebauthnResult(NativeBridge.nativeWebauthnLoginFinish(handle, credentialJson)) }

    override suspend fun connect(sessionCredential: String): Boolean =
        withContext(Dispatchers.IO) { NativeBridge.nativeConnect(handle, sessionCredential) }

    override suspend fun listDevhosts(): List<DevHostPresence> = withContext(Dispatchers.IO) {
        json.decodeFromString<List<WireDevHostPresence>>(NativeBridge.nativeListDevhosts(handle)).map { it.toDomain() }
    }

    override suspend fun registerFcmToken(fcmToken: String): Boolean =
        withContext(Dispatchers.IO) { NativeBridge.nativeRegisterFcmToken(handle, fcmToken) }

    override fun close() = NativeBridge.nativeClose(handle)

    companion object {
        private val json = Json { ignoreUnknownKeys = true }
    }
}

/**
 * The literal JNI surface — every signature here must match `lib.rs`'s
 * `native_method!` declarations exactly (type, arity, and `static`-ness).
 * Nothing but [NativeChooshEngine] should call these directly.
 */
private object NativeBridge {
    init { System.loadLibrary("choosh_android_bridge") }

    // `native_method!` on the Rust side registers these as true JVM `static`
    // methods — a plain `external fun` in a Kotlin `object` compiles as an
    // instance method on the singleton (called via a static `INSTANCE`
    // field), which is a different JVM method shape and fails to link
    // ("registered as static but called as instance method", confirmed by
    // a real crash against the real .so before `@JvmStatic` was added
    // here). `@JvmStatic` is required on every one of these, not optional.
    @JvmStatic external fun nativeInit(httpBaseUrl: String, wsUrl: String): Long
    @JvmStatic external fun nativeWebauthnRegisterStart(handle: Long): String
    @JvmStatic external fun nativeWebauthnRegisterFinish(handle: Long, credentialJson: String): String
    @JvmStatic external fun nativeWebauthnLoginStart(handle: Long): String
    @JvmStatic external fun nativeWebauthnLoginFinish(handle: Long, credentialJson: String): String
    @JvmStatic external fun nativeConnect(handle: Long, sessionCredential: String): Boolean
    @JvmStatic external fun nativeListDevhosts(handle: Long): String
    @JvmStatic external fun nativeRegisterFcmToken(handle: Long, fcmToken: String): Boolean
    @JvmStatic external fun nativeClose(handle: Long)
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
