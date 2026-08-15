package ai.choosh.terminal

import android.graphics.Rect
import android.os.Handler
import android.os.Looper
import android.view.View
import android.view.accessibility.AccessibilityEvent
import androidx.core.view.accessibility.AccessibilityNodeInfoCompat
import androidx.customview.widget.ExploreByTouchHelper

/**
 * Exposes the terminal's current visible grid content to TalkBack via a
 * single virtual accessibility node covering the whole
 * [TerminalSurfaceView] — the standard Android pattern for a custom-drawn
 * `SurfaceView`/`View` with real text-equivalent content but no natural
 * child views for a screen reader to walk, per
 * `docs/accessibility-device-report.md`'s item 1, gap 2: "the Terminal
 * surface has zero accessible content... A screen-reader user gets no
 * indication this region contains a terminal, let alone its content."
 *
 * Deliberately **one** virtual node for the whole grid (row-by-row text
 * joined into one block), not one node per cell or row — the task this
 * pass covers explicitly allows "even without fine-grained per-cell
 * navigation", and per-cell nodes would be a much larger, separate design
 * (cursor-following focus, live per-cell diffing) out of scope here.
 *
 * Content is pulled from [TerminalSession.visibleText] (backed by the Rust
 * engine's real `Grid::visible_text`, not a Kotlin-side shadow copy) on a
 * fixed poll interval while attached, since the Rust renderer has no
 * existing Kotlin-facing callback for "content changed" — redraws happen
 * entirely inside the JNI boundary (see `terminal_jni.rs::redraw`). This is
 * a real, working live-region approximation (new output becomes audible
 * within one poll interval of arriving), not the fuller "true push-based
 * live-region announcement" the accessibility report flagged as a further,
 * separate design question ("a fuller fix... is a larger design question
 * out of scope for a one-line fix").
 */
class TerminalAccessibilityHelper(private val host: View, private val session: TerminalSession) : ExploreByTouchHelper(host) {

    private val handler = Handler(Looper.getMainLooper())
    private var lastText: String = ""
    private var polling = false

    private val pollRunnable = object : Runnable {
        override fun run() {
            val text = session.visibleText()
            if (text != lastText) {
                lastText = text
                invalidateVirtualView(VIRTUAL_TERMINAL_ID, AccessibilityEvent.CONTENT_CHANGE_TYPE_TEXT)
            }
            if (polling) handler.postDelayed(this, POLL_INTERVAL_MS)
        }
    }

    /** Starts polling for content changes — call once the session is attached and the view is on screen. */
    fun startPolling() {
        if (polling) return
        polling = true
        handler.post(pollRunnable)
    }

    fun stopPolling() {
        polling = false
        handler.removeCallbacks(pollRunnable)
    }

    override fun getVirtualViewAt(x: Float, y: Float): Int = VIRTUAL_TERMINAL_ID

    override fun getVisibleVirtualViews(virtualViewIds: MutableList<Int>) {
        virtualViewIds.add(VIRTUAL_TERMINAL_ID)
    }

    override fun onPopulateNodeForVirtualView(virtualViewId: Int, node: AccessibilityNodeInfoCompat) {
        val bounds = Rect(0, 0, host.width.coerceAtLeast(1), host.height.coerceAtLeast(1))
        node.setBoundsInParent(bounds)
        val text = session.visibleText()
        lastText = text
        node.text = text.ifBlank { "Terminal, no output yet" }
        node.className = "android.widget.TextView"
        node.contentDescription = "Terminal"
        node.isFocusable = true
        node.isClickable = true
    }

    override fun onPerformActionForVirtualView(virtualViewId: Int, action: Int, arguments: android.os.Bundle?): Boolean = false

    private companion object {
        const val VIRTUAL_TERMINAL_ID = 1
        const val POLL_INTERVAL_MS = 750L
    }
}
