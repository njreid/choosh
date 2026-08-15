package ai.choosh.webservice

import ai.choosh.WebGatewayBridge

/** Name of the `HttpOnly` cookie the loopback gateway checks on every accepted connection's first request. MUST match `choosh-android-bridge::web_gateway::TOKEN_COOKIE_NAME` exactly — this string is duplicated (not shared) across the JNI boundary the same way every other wire-shape constant in this codebase is, so a mismatch here is a compile-visible-only-by-inspection risk, called out explicitly rather than left implicit. */
const val WEB_GATEWAY_TOKEN_COOKIE_NAME = "choosh_gw_token"

/** What a live gateway hands back: enough for [ai.choosh.webservice.WebServiceScreen] to point a `WebView` at it and set its gateway cookie. */
data class WebGatewayInfo(val port: Int, val token: String) {
    val url: String get() = "http://127.0.0.1:$port/"

    /**
     * The literal `Set-Cookie`-style string handed to
     * `CookieManager.setCookie` — includes `HttpOnly` (per
     * service-tunnels.md: "a random per-pin token held in an `HttpOnly`
     * WebView cookie") so page JavaScript in the `WebService` `WebView`
     * cannot read this token via `document.cookie`, even though the
     * `WebView`'s own network stack still attaches it to every request to
     * this loopback origin.
     */
    val cookieHeader: String get() = "$WEB_GATEWAY_TOKEN_COOKIE_NAME=$token; Path=/; HttpOnly"
}

/** A started gateway: its opaque native handle (for [WebServiceGatewayController.stop]) plus what the `WebView` needs. */
data class WebGatewayHandle(val handle: Long, val info: WebGatewayInfo)

/**
 * Abstraction over [WebGatewayBridge]'s raw JNI calls, so
 * [WebServiceViewModel] is testable on the host JVM (where
 * `System.loadLibrary("choosh_android_bridge")` would throw
 * `UnsatisfiedLinkError` — there is no real `.so` loaded in a JVM unit
 * test) without needing a real native gateway or relay connection.
 */
interface WebServiceGatewayController {
    /** Starts a gateway for `itemId`; `null` if not connected or the bind failed. */
    fun start(connectionHandle: Long, targetDeviceId: String, itemId: String): WebGatewayHandle?

    fun stop(gatewayHandle: Long)
}

/** The real, JNI-backed [WebServiceGatewayController]. */
class RealWebServiceGatewayController : WebServiceGatewayController {
    override fun start(connectionHandle: Long, targetDeviceId: String, itemId: String): WebGatewayHandle? {
        val handle = WebGatewayBridge.nativeWebGatewayStart(connectionHandle, targetDeviceId, itemId)
        if (handle == 0L) return null
        val port = WebGatewayBridge.nativeWebGatewayPort(handle)
        val token = WebGatewayBridge.nativeWebGatewayToken(handle)
        if (port == 0) {
            stop(handle)
            return null
        }
        return WebGatewayHandle(handle, WebGatewayInfo(port, token))
    }

    override fun stop(gatewayHandle: Long) {
        if (gatewayHandle != 0L) WebGatewayBridge.nativeWebGatewayStop(gatewayHandle)
    }
}
