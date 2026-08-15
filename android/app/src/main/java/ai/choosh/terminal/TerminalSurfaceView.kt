package ai.choosh.terminal

import ai.choosh.TerminalBridge
import android.content.Context
import android.graphics.Rect
import android.text.InputType
import android.util.AttributeSet
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import androidx.core.view.ViewCompat

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
 * Also owns everything `terminal-experience.md`'s "Kotlin owns Android
 * focus, IME integration, accessibility semantics, insets, gestures, and
 * haptics" assigns to the Kotlin side for this surface:
 * - **Hardware keyboard**: [dispatchKeyEvent] translates real
 *   `android.view.KeyEvent`s (via [TerminalKeyMapper]) into
 *   [TerminalSession.sendKey]/[TerminalSession.sendText] calls — those
 *   methods were real, tested, and completely unwired before this pass
 *   (`docs/accessibility-device-report.md`'s item 2).
 * - **IME**: [onCreateInputConnection] returns a real [InputConnection]
 *   ([TerminalInputConnection]) so the soft keyboard (and any hardware
 *   keyboard routed through the IME) can commit text and send key events.
 * - **Accessibility**: [TerminalAccessibilityHelper] exposes the terminal's
 *   current visible grid content to TalkBack as a virtual node
 *   (`docs/accessibility-device-report.md`'s item 1, gap 2).
 *
 * **A real, confirmed race, not just a hypothetical**: [attachSession] can
 * run before or after this view's own `surfaceCreated`/`surfaceChanged`
 * callbacks fire (both orderings are legitimate — `TerminalScreen.kt`'s
 * `AndroidView` `factory`/`update` timing relative to Compose's own
 * `DisposableEffect` isn't guaranteed, and a `SurfaceView`'s callbacks fire
 * as soon as it's laid out and attached to the window, independent of
 * Compose). Before this pass, a `surfaceCreated`/`surfaceChanged` that
 * raced *ahead* of a real session handle was silently dropped (the
 * `handle != 0L` guards below simply skipped the native call), permanently
 * losing the real surface size — this is the root cause behind
 * `docs/accessibility-device-report.md`'s item 3/4 finding that a
 * DeX/tablet-sized terminal rendered its glyphs "enormous" while staying
 * pinned at the hardcoded 80x24 grid `TerminalScreen.kt`'s `session.create`
 * call requests as its *initial* fallback size (real GPU/font metrics
 * aren't known before the first surface exists, so some initial value is
 * unavoidable — see `terminal_renderer.rs`'s `FALLBACK_CELL_WIDTH`/
 * `FALLBACK_CELL_HEIGHT` doc comment for the same fallback-then-replace
 * pattern on the Rust side). [attachSession] now replays whatever the
 * surface already delivered, closing that gap.
 */
class TerminalSurfaceView @JvmOverloads constructor(
    context: Context,
    attrs: AttributeSet? = null,
) : SurfaceView(context, attrs), SurfaceHolder.Callback {

    @Volatile
    private var sessionHandle: Long = 0
    private var session: TerminalSession? = null
    private var accessibilityHelper: TerminalAccessibilityHelper? = null

    // The most recent real surface/size this view's own SurfaceHolder
    // callbacks observed, kept independent of `sessionHandle` specifically
    // so `attachSession` can replay them — see this class's doc comment.
    private var liveSurface: android.view.Surface? = null
    private var liveWidth: Int = 0
    private var liveHeight: Int = 0

    init {
        holder.addCallback(this)
        isFocusable = true
        isFocusableInTouchMode = true
        // Tap-to-focus-and-show-keyboard: a `SurfaceView` never gains focus
        // or shows the IME on its own the way an `EditText` does:
        setOnClickListener {
            requestFocus()
            val imm = context.getSystemService(Context.INPUT_METHOD_SERVICE) as? InputMethodManager
            imm?.showSoftInput(this, InputMethodManager.SHOW_IMPLICIT)
        }
    }

    /** Binds this view to a live native terminal session. */
    fun attachSession(newSession: TerminalSession) {
        session = newSession
        val handle = newSession.handle
        sessionHandle = handle

        val helper = TerminalAccessibilityHelper(this, newSession)
        accessibilityHelper = helper
        ViewCompat.setAccessibilityDelegate(this, helper)
        helper.startPolling()

        if (handle != 0L) {
            // Replay whatever the surface already delivered before this
            // handle became non-zero — see class doc comment.
            liveSurface?.let { surface -> TerminalBridge.nativeTerminalSurfaceCreated(handle, surface) }
            if (liveWidth > 0 && liveHeight > 0) {
                TerminalBridge.nativeTerminalSurfaceChanged(handle, liveWidth, liveHeight)
            }
        }
    }

    fun detachSession() {
        accessibilityHelper?.stopPolling()
        accessibilityHelper = null
        ViewCompat.setAccessibilityDelegate(this, null)
        session = null
        sessionHandle = 0
    }

    override fun surfaceCreated(holder: SurfaceHolder) {
        liveSurface = holder.surface
        val handle = sessionHandle
        if (handle != 0L) {
            TerminalBridge.nativeTerminalSurfaceCreated(handle, holder.surface)
        }
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        liveWidth = width
        liveHeight = height
        val handle = sessionHandle
        if (handle != 0L) {
            TerminalBridge.nativeTerminalSurfaceChanged(handle, width, height)
        }
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        liveSurface = null
        liveWidth = 0
        liveHeight = 0
        val handle = sessionHandle
        if (handle != 0L) {
            TerminalBridge.nativeTerminalSurfaceDestroyed(handle)
        }
    }

    // --- Hardware keyboard ---

    /**
     * The hardware-keyboard input path: real `KeyEvent`s reach here
     * whenever this view has focus, independent of the IME/soft keyboard.
     * `dispatchKeyEvent` (not `onKeyDown`) is used so this reliably runs
     * before Android's own default key handling (e.g. Tab-focus-traversal)
     * intercepts a key this terminal should own instead — per
     * `terminal-experience.md`'s "handle... hardware keyboards" and
     * `docs/accessibility-device-report.md`'s item 2 confirmed gap.
     */
    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        val activeSession = session
        if (activeSession != null && event.action == KeyEvent.ACTION_DOWN && !TerminalKeyMapper.isSystemReserved(event.keyCode)) {
            val command = TerminalKeyMapper.map(event.keyCode, event.metaState, event.unicodeChar)
            if (command != null) {
                dispatchCommand(activeSession, command)
                return true
            }
        }
        return super.dispatchKeyEvent(event)
    }

    internal fun dispatchCommand(target: TerminalSession, command: TerminalKeyMapper.Command) {
        when (command) {
            is TerminalKeyMapper.Command.Key -> target.sendKey(command.code, ctrl = command.ctrl, alt = command.alt, shift = command.shift)
            is TerminalKeyMapper.Command.Text -> target.sendText(command.text, ctrl = command.ctrl, alt = command.alt)
        }
    }

    internal fun currentSession(): TerminalSession? = session

    // --- IME ---

    override fun onCheckIsTextEditor(): Boolean = true

    /**
     * A real `InputConnection` ([TerminalInputConnection]), per
     * `terminal-experience.md`'s "Android IME" section: "The native
     * terminal host MUST expose a real Android `InputConnection`." Disables
     * suggestions and personalized learning (terminal input is not
     * natural-language text an IME should learn from or autocorrect) and
     * extract/fullscreen editing UI, per that same section's "SHOULD"
     * list. IME composition stays local — [TerminalInputConnection] never
     * populates its scratch `Editable` with real terminal output, so
     * `getTextBeforeCursor`/`getTextAfterCursor` (which
     * `BaseInputConnection` derives from that `Editable`) never expose
     * terminal content back to the keyboard, per that section's "Terminal
     * output MUST NOT be exposed as surrounding text to the keyboard."
     */
    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
        outAttrs.inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI or
            EditorInfo.IME_FLAG_NO_FULLSCREEN or
            EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING or
            EditorInfo.IME_ACTION_NONE
        return TerminalInputConnection(this)
    }

    // --- Accessibility: ExploreByTouchHelper contract ---
    // `ExploreByTouchHelper`'s own documented usage pattern requires the
    // host view to forward these three callbacks to it.

    override fun dispatchHoverEvent(event: MotionEvent): Boolean =
        accessibilityHelper?.dispatchHoverEvent(event) == true || super.dispatchHoverEvent(event)

    override fun onFocusChanged(gainFocus: Boolean, direction: Int, previouslyFocusedRect: Rect?) {
        super.onFocusChanged(gainFocus, direction, previouslyFocusedRect)
        accessibilityHelper?.onFocusChanged(gainFocus, direction, previouslyFocusedRect)
    }

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        if (accessibilityHelper?.dispatchKeyEvent(event) == true) return true
        return super.onKeyDown(keyCode, event)
    }
}

/**
 * A minimal, real [InputConnection] wiring committed IME text and IME-sent
 * key events to [TerminalSession]'s already-existing `sendKey`/`sendText`.
 * Built on [BaseInputConnection] (`dummy = false`, so it keeps a real local
 * scratch `Editable` for in-progress composition) rather than implementing
 * [InputConnection] from scratch — that scratch buffer is exactly the
 * "IME composition MUST remain local until committed" behavior
 * `terminal-experience.md` requires, for free, as long as this class never
 * copies real terminal content into it (it doesn't: [commitText] forwards
 * to the terminal and clears the buffer immediately rather than
 * accumulating it).
 */
private class TerminalInputConnection(private val view: TerminalSurfaceView) : BaseInputConnection(view, false) {

    override fun commitText(text: CharSequence, newCursorPosition: Int): Boolean {
        val session = view.currentSession()
        if (session != null && text.isNotEmpty()) {
            session.sendText(text.toString())
        }
        // Clears the local scratch buffer immediately: nothing about this
        // connection's job needs it to persist past the commit, and
        // leaving it would let it grow unboundedly over a long session.
        editable?.clear()
        return true
    }

    override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
        val session = view.currentSession() ?: return false
        repeat(beforeLength) { session.sendKey(TerminalKeyMapper.KEY_BACKSPACE) }
        repeat(afterLength) { session.sendKey(TerminalKeyMapper.KEY_DELETE) }
        return true
    }

    override fun sendKeyEvent(event: KeyEvent): Boolean {
        val session = view.currentSession()
        if (session != null && event.action == KeyEvent.ACTION_DOWN) {
            val command = TerminalKeyMapper.map(event.keyCode, event.metaState, event.unicodeChar)
            if (command != null) {
                view.dispatchCommand(session, command)
                return true
            }
        }
        return super.sendKeyEvent(event)
    }

    override fun performEditorAction(editorAction: Int): Boolean {
        val session = view.currentSession() ?: return false
        session.sendKey(TerminalKeyMapper.KEY_ENTER)
        return true
    }
}
