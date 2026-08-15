package ai.choosh.markdown

import ai.choosh.MarkdownGatewayBridge

/** Name of the `HttpOnly` cookie the Markdown gateway checks. MUST match `choosh-android-bridge::markdown_gateway::TOKEN_COOKIE_NAME` exactly — see `ai.choosh.webservice.WEB_GATEWAY_TOKEN_COOKIE_NAME`'s identical duplication note. */
const val MARKDOWN_GATEWAY_TOKEN_COOKIE_NAME = "choosh_md_token"

data class MarkdownGatewayInfo(val port: Int, val token: String) {
    val docUrl: String get() = "http://127.0.0.1:$port/doc"
    val cookieHeader: String get() = "$MARKDOWN_GATEWAY_TOKEN_COOKIE_NAME=$token; Path=/; HttpOnly"
}

data class MarkdownGatewayHandle(val handle: Long, val info: MarkdownGatewayInfo)

/** Abstraction over [MarkdownGatewayBridge]'s raw JNI calls — see `ai.choosh.webservice.WebServiceGatewayController`'s identical host-JVM-testability rationale. */
interface MarkdownGatewayController {
    fun start(connectionHandle: Long, targetDeviceId: String, workspaceId: String, docPath: String): MarkdownGatewayHandle?

    fun stop(gatewayHandle: Long)
}

class RealMarkdownGatewayController : MarkdownGatewayController {
    override fun start(connectionHandle: Long, targetDeviceId: String, workspaceId: String, docPath: String): MarkdownGatewayHandle? {
        val handle = MarkdownGatewayBridge.nativeMarkdownGatewayStart(connectionHandle, targetDeviceId, workspaceId, docPath)
        if (handle == 0L) return null
        val port = MarkdownGatewayBridge.nativeMarkdownGatewayPort(handle)
        val token = MarkdownGatewayBridge.nativeMarkdownGatewayToken(handle)
        if (port == 0) {
            stop(handle)
            return null
        }
        return MarkdownGatewayHandle(handle, MarkdownGatewayInfo(port, token))
    }

    override fun stop(gatewayHandle: Long) {
        if (gatewayHandle != 0L) MarkdownGatewayBridge.nativeMarkdownGatewayStop(gatewayHandle)
    }
}
