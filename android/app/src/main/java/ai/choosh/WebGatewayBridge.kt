package ai.choosh

/**
 * The literal JNI surface for `rust/choosh-android-bridge/src/gateway_jni.rs`'s
 * `WebGatewayBridge` native class — a **separate** Kotlin object from
 * `ai.choosh.NativeBridge`/`ai.choosh.TerminalBridge`, per this crate's
 * existing "unrelated responsibilities get separate JNI classes" discipline
 * (see [TerminalBridge]'s doc comment for why, and why this MUST live
 * directly in package `ai.choosh` to match `gateway_jni.rs`'s
 * `java_type = "ai.choosh.WebGatewayBridge"` exactly).
 *
 * Nothing but [ai.choosh.webservice.WebServiceGatewaySession] should call
 * these directly.
 */
internal object WebGatewayBridge {
    init { System.loadLibrary("choosh_android_bridge") }

    /**
     * Starts a `WebService` loopback gateway for `itemId`, tunneling
     * through the connection identified by `connectionHandle` (the
     * `NativeBridge` handle from [ai.choosh.NativeChooshEngine]). Returns a
     * gateway handle, or `0` if not connected or the bind failed.
     */
    @JvmStatic external fun nativeWebGatewayStart(connectionHandle: Long, targetDeviceId: String, itemId: String): Long

    /** The gateway's bound loopback port, or `0` if `handle` is unknown. */
    @JvmStatic external fun nativeWebGatewayPort(handle: Long): Int

    /** The gateway's per-pin random token — set as the `WebView`'s `HttpOnly` gateway cookie. */
    @JvmStatic external fun nativeWebGatewayToken(handle: Long): String

    /**
     * Stops the gateway immediately — per service-tunnels.md, this closes
     * only the local gateway/tunnel; it never sends any "stop the remote
     * service" RPC. Called on unpin and on screen teardown.
     */
    @JvmStatic external fun nativeWebGatewayStop(handle: Long)
}
