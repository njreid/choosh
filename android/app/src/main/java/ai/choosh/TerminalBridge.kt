package ai.choosh

/**
 * The literal JNI surface for `rust/choosh-android-bridge/src/terminal_jni.rs`
 * — a **separate** Kotlin object from `ai.choosh.NativeBridge` (see
 * `NativeChooshEngine.kt`), per `docs/specs/terminal-experience.md`: a
 * per-surface terminal renderer's lifecycle (surface-created/-changed/
 * -destroyed, write, resize) is a distinct, much higher-frequency
 * responsibility from connection/auth, so it gets its own native class
 * name (`ai.choosh.TerminalBridge`) rather than being added to
 * `NativeBridge`.
 *
 * Every signature here must match `terminal_jni.rs`'s `native_method!`
 * declarations exactly (type, arity, and `static`-ness) — see
 * `NativeChooshEngine.kt`'s `NativeBridge` doc comment for why `@JvmStatic`
 * is required on every one of these, not optional.
 *
 * This object MUST live directly in package `ai.choosh` (not
 * `ai.choosh.terminal`, where the rest of the terminal UI code lives) —
 * `terminal_jni.rs`'s `native_method!` invocations declare
 * `java_type = "ai.choosh.TerminalBridge"`, and standard JNI implicit
 * symbol resolution requires the compiled class's fully-qualified name to
 * match exactly (confirmed the hard way: an earlier pass placed this
 * object in `ai.choosh.terminal` and every native call crashed at runtime
 * with `UnsatisfiedLinkError: No implementation found for ...
 * ai.choosh.terminal.TerminalBridge.nativeTerminalCreate`, tried against
 * a real device before being moved here). Nothing but
 * [ai.choosh.terminal.TerminalSession] should call these directly.
 */
internal object TerminalBridge {
    init { System.loadLibrary("choosh_android_bridge") }

    /** Creates a new terminal session with the given initial cell grid size. Returns a session handle, or 0 on failure. */
    @JvmStatic external fun nativeTerminalCreate(cols: Int, rows: Int): Long

    /**
     * Opens the `pty:<itemId>` relay tunnel for this session over the
     * connection identified by `connectionHandle` (the `NativeBridge`
     * handle from [ai.choosh.NativeChooshEngine]) and starts streaming PTY
     * output into the VT parser. Returns `false` if not connected or the
     * tunnel failed to open.
     */
    @JvmStatic external fun nativeTerminalAttachPty(handle: Long, connectionHandle: Long, targetDeviceId: String, itemId: String): Boolean

    /**
     * Called from `SurfaceHolder.Callback.surfaceCreated`. Declared as
     * `Any` (not `android.view.Surface`) because the Rust side's
     * `native_method!` shorthand declares this parameter as the generic
     * `JObject` — the JVM method descriptor computed from each side's
     * declared type must match exactly for JNI's implicit symbol
     * resolution to find this native method, and `Any`/`JObject` both
     * compile to the same `Ljava/lang/Object;` descriptor. Callers pass a
     * real `android.view.Surface`, which is a valid `Any` at the call site.
     */
    @JvmStatic external fun nativeTerminalSurfaceCreated(handle: Long, surface: Any)

    /** Called from `SurfaceHolder.Callback.surfaceChanged`, with the surface's pixel dimensions. */
    @JvmStatic external fun nativeTerminalSurfaceChanged(handle: Long, widthPx: Int, heightPx: Int)

    /** Called from `SurfaceHolder.Callback.surfaceDestroyed`. */
    @JvmStatic external fun nativeTerminalSurfaceDestroyed(handle: Long)

    /**
     * Sends one non-printable key. `keyCode` is this boundary's own small
     * wire enum: 0=Enter,1=Tab,2=Backspace,3=Delete,4=Escape,5=Up,6=Down,
     * 7=Left,8=Right,9=Home,10=End,11=PageUp,12=PageDown — see
     * `terminal_jni.rs::key_from_code`.
     */
    @JvmStatic external fun nativeTerminalSendKey(handle: Long, keyCode: Int, ctrl: Boolean, alt: Boolean, shift: Boolean)

    /** Commits already-composed IME/hardware-keyboard text. */
    @JvmStatic external fun nativeTerminalSendText(handle: Long, text: String, ctrl: Boolean, alt: Boolean)

    /** Sends clipboard bytes, wrapped in bracketed-paste markers by the Rust engine if the terminal has that mode enabled. */
    @JvmStatic external fun nativeTerminalPaste(handle: Long, data: ByteArray)

    /**
     * Forwards one touch-derived mouse event, gated by the terminal's
     * active tracking mode on the Rust side. `action`: 0=press,1=release,
     * 2=motion. `button`: 0=left,1=middle,2=right,3=scrollUp,4=scrollDown,5=none.
     */
    @JvmStatic external fun nativeTerminalSendMouse(handle: Long, col: Int, row: Int, action: Int, button: Int, shift: Boolean, alt: Boolean, ctrl: Boolean)

    /**
     * Test-only synthetic byte source: feeds `bytes` directly into the VT
     * parser and redraws, bypassing the PTY tunnel — used for on-device
     * rendering verification with a canned byte sequence (no live relay
     * connection required).
     */
    @JvmStatic external fun nativeTerminalTestInject(handle: Long, bytes: ByteArray)

    /**
     * Returns the current visible grid as plain text, one line per row —
     * the accessibility content source for `TerminalSurfaceView`'s virtual
     * accessibility node (`docs/accessibility-device-report.md`'s item 1,
     * gap 2). Always returns a string (never null), empty for an
     * absent/destroyed handle.
     */
    @JvmStatic external fun nativeTerminalGetText(handle: Long): String

    @JvmStatic external fun nativeTerminalDestroy(handle: Long)
}
