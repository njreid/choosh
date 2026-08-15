package ai.choosh.terminal

import ai.choosh.TerminalBridge
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.setValue

/**
 * Owns one native terminal session's lifecycle (create/attach/destroy),
 * wrapping [TerminalBridge]'s raw handle-based calls. `handle` is a Compose
 * `State` (not a plain `var`) specifically so [TerminalScreen]'s
 * `AndroidView` `update` lambda re-runs once [create] finishes and the
 * handle becomes non-zero, even though [create] itself is a synchronous
 * JNI call — Compose has no other signal that a plain field changed.
 */
class TerminalSession {
    var handle: Long by mutableLongStateOf(0L)
        private set

    fun create(cols: Int, rows: Int) {
        check(handle == 0L) { "TerminalSession.create called twice without destroy" }
        handle = TerminalBridge.nativeTerminalCreate(cols, rows)
    }

    /** Opens the PTY tunnel for this session. No-op if [create] hasn't run yet. */
    fun attachPty(connectionHandle: Long, targetDeviceId: String, itemId: String): Boolean {
        val handle = handle
        if (handle == 0L) return false
        return TerminalBridge.nativeTerminalAttachPty(handle, connectionHandle, targetDeviceId, itemId)
    }

    fun sendKey(keyCode: Int, ctrl: Boolean = false, alt: Boolean = false, shift: Boolean = false) {
        val handle = handle
        if (handle != 0L) TerminalBridge.nativeTerminalSendKey(handle, keyCode, ctrl, alt, shift)
    }

    fun sendText(text: String, ctrl: Boolean = false, alt: Boolean = false) {
        val handle = handle
        if (handle != 0L) TerminalBridge.nativeTerminalSendText(handle, text, ctrl, alt)
    }

    fun paste(data: ByteArray) {
        val handle = handle
        if (handle != 0L) TerminalBridge.nativeTerminalPaste(handle, data)
    }

    fun sendMouse(col: Int, row: Int, action: Int, button: Int, shift: Boolean = false, alt: Boolean = false, ctrl: Boolean = false) {
        val handle = handle
        if (handle != 0L) TerminalBridge.nativeTerminalSendMouse(handle, col, row, action, button, shift, alt, ctrl)
    }

    /** Test/demo-only synthetic byte injection — see [TerminalBridge.nativeTerminalTestInject]. */
    fun testInject(bytes: ByteArray) {
        val handle = handle
        if (handle != 0L) TerminalBridge.nativeTerminalTestInject(handle, bytes)
    }

    /**
     * The current visible grid as plain text, one line per row — used by
     * [ai.choosh.terminal.TerminalAccessibilityHelper] to expose real
     * terminal content to TalkBack. Returns an empty string before
     * [create] has run.
     */
    fun visibleText(): String {
        val handle = handle
        return if (handle != 0L) TerminalBridge.nativeTerminalGetText(handle) else ""
    }

    fun destroy() {
        val handle = handle
        if (handle != 0L) {
            TerminalBridge.nativeTerminalDestroy(handle)
            this.handle = 0L
        }
    }
}
