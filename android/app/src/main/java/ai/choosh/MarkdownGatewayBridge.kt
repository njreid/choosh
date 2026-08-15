package ai.choosh

/**
 * The literal JNI surface for `rust/choosh-android-bridge/src/gateway_jni.rs`'s
 * `MarkdownGatewayBridge` native class — see [WebGatewayBridge]'s doc
 * comment for why this is a separate JNI class and why it must live
 * directly in package `ai.choosh`.
 *
 * Nothing but [ai.choosh.markdown.MarkdownGatewaySession] should call these
 * directly.
 */
internal object MarkdownGatewayBridge {
    init { System.loadLibrary("choosh_android_bridge") }

    /**
     * Starts a Markdown loopback gateway for `docPath` within
     * `workspaceId`, fetching content over the connection identified by
     * `connectionHandle` via the existing `"rpc"`-purpose tunnel machinery
     * (no new relay tunnel purpose — see `markdown_gateway.rs`'s module
     * doc). Returns a gateway handle, or `0` if not connected or the bind
     * failed.
     */
    @JvmStatic external fun nativeMarkdownGatewayStart(connectionHandle: Long, targetDeviceId: String, workspaceId: String, docPath: String): Long

    /** The gateway's bound loopback port, or `0` if `handle` is unknown. */
    @JvmStatic external fun nativeMarkdownGatewayPort(handle: Long): Int

    /** The gateway's per-instance random token — set as the `WebView`'s `HttpOnly` gateway cookie. */
    @JvmStatic external fun nativeMarkdownGatewayToken(handle: Long): String

    /** Stops the gateway immediately. Called on screen teardown. */
    @JvmStatic external fun nativeMarkdownGatewayStop(handle: Long)

    /**
     * Test/demo-only: starts a gateway backed by a fixed in-memory fixture
     * document, with no relay connection needed at all — mirrors
     * [ai.choosh.TerminalBridge.nativeTerminalTestInject]'s precedent for
     * on-device verification without a live backend. See
     * `gateway_jni.rs::native_markdown_gateway_start_fixture`'s doc
     * comment.
     */
    @JvmStatic external fun nativeMarkdownGatewayStartFixture(): Long
}
