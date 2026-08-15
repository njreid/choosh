package ai.choosh.terminal

import ai.choosh.TerminalBridge
import android.content.Context
import android.util.AttributeSet
import android.view.SurfaceHolder
import android.view.SurfaceView

/**
 * Hosts the native wgpu/glyphon terminal renderer, per
 * `docs/specs/terminal-experience.md`'s "native Android surface backed by
 * Rust". Ports the `SurfaceView` -> `ANativeWindow` lifecycle Zelland's
 * `MainActivity.kt` used (`surfaceCreated`/`surfaceChanged`/
 * `surfaceDestroyed` -> `passSurfaceToRust`/`passResizeToRust`/
 * `passSurfaceDestroyedToRust`, per `docs/licenses/terminal-provenance.md`),
 * replacing the JNI class name and per-callback plumbing with this file's
 * `TerminalBridge` calls.
 *
 * One [sessionHandle] must be set (via [attachSession]) before any surface
 * callback fires meaningfully; callbacks arriving before that are no-ops —
 * this can legitimately happen if the view is inflated before the native
 * session finishes creating.
 */
class TerminalSurfaceView @JvmOverloads constructor(
    context: Context,
    attrs: AttributeSet? = null,
) : SurfaceView(context, attrs), SurfaceHolder.Callback {

    @Volatile
    private var sessionHandle: Long = 0

    init {
        holder.addCallback(this)
    }

    /** Binds this view to a live native terminal session handle. */
    fun attachSession(handle: Long) {
        sessionHandle = handle
    }

    fun detachSession() {
        sessionHandle = 0
    }

    override fun surfaceCreated(holder: SurfaceHolder) {
        val handle = sessionHandle
        if (handle != 0L) {
            TerminalBridge.nativeTerminalSurfaceCreated(handle, holder.surface)
        }
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        val handle = sessionHandle
        if (handle != 0L) {
            TerminalBridge.nativeTerminalSurfaceChanged(handle, width, height)
        }
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        val handle = sessionHandle
        if (handle != 0L) {
            TerminalBridge.nativeTerminalSurfaceDestroyed(handle)
        }
    }
}
