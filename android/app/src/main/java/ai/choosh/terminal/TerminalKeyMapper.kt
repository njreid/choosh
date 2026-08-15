package ai.choosh.terminal

import android.view.KeyEvent

/**
 * Translates a real Android hardware-keyboard [KeyEvent] into a command for
 * [TerminalSession]'s existing, previously-unwired `sendKey`/`sendText`
 * methods — the hardware-keyboard input path
 * `docs/accessibility-device-report.md`'s item 2 found completely absent
 * ("no `KeyEvent`, `onKeyEvent`... usage exists anywhere in
 * `android/app/src/main/java/ai/choosh/`"). A pure function (no `View`/JNI
 * dependency) so it is directly unit-testable without instrumentation —
 * `android.view.KeyEvent`'s `KEYCODE_*`/`META_*` fields are compile-time
 * `public static final int` constants, safe to reference from a plain JVM
 * unit test against the Android SDK stub jar (no real method call needed).
 *
 * Both the hardware-keyboard path ([TerminalSurfaceView.dispatchKeyEvent])
 * and the extra-keys bar ([TerminalScreen]'s modifier-aware buttons) funnel
 * through the same [TerminalSession.sendKey]/[TerminalSession.sendText]
 * pair the Rust engine already exposes, per `terminal-experience.md`'s "IME,
 * hardware keyboard, extra-key, paste, and accessibility actions MUST all
 * use the same command dispatcher".
 */
object TerminalKeyMapper {
    /** Wire values matching `terminal_jni.rs::key_from_code` exactly. */
    const val KEY_ENTER: Int = 0
    const val KEY_TAB: Int = 1
    const val KEY_BACKSPACE: Int = 2
    const val KEY_DELETE: Int = 3
    const val KEY_ESCAPE: Int = 4
    const val KEY_UP: Int = 5
    const val KEY_DOWN: Int = 6
    const val KEY_LEFT: Int = 7
    const val KEY_RIGHT: Int = 8
    const val KEY_HOME: Int = 9
    const val KEY_END: Int = 10
    const val KEY_PAGE_UP: Int = 11
    const val KEY_PAGE_DOWN: Int = 12

    sealed class Command {
        data class Key(val code: Int, val ctrl: Boolean, val alt: Boolean, val shift: Boolean) : Command()
        data class Text(val text: String, val ctrl: Boolean, val alt: Boolean) : Command()
    }

    /**
     * Maps one `ACTION_DOWN` hardware key event to a [Command], or `null`
     * if this event isn't terminal input at all (a bare modifier key by
     * itself, a media/volume/navigation key, or a key with no printable
     * mapping and no unicode char) — callers should let the system handle
     * those (`super.dispatchKeyEvent`), not consume them.
     */
    fun map(keyCode: Int, metaState: Int, unicodeChar: Int): Command? {
        val ctrl = metaState and KeyEvent.META_CTRL_ON != 0
        val alt = metaState and KeyEvent.META_ALT_ON != 0
        val shift = metaState and KeyEvent.META_SHIFT_ON != 0

        nonPrintableKeyCode(keyCode)?.let { code -> return Command.Key(code, ctrl, alt, shift) }

        // Ctrl+<letter> (Ctrl+C, Ctrl+D, ...): Android's own `unicodeChar`
        // computation generally does not produce a control character for a
        // ctrl-held letter, so this is handled directly from the keycode
        // range rather than relying on `unicodeChar` — mirrors
        // `keys.rs::ctrl_byte`'s own A-Z handling on the Rust side, which
        // this then reaches via `encode_key`'s ctrl-modifier path.
        if (ctrl && keyCode in KeyEvent.KEYCODE_A..KeyEvent.KEYCODE_Z) {
            val letter = 'a' + (keyCode - KeyEvent.KEYCODE_A)
            return Command.Text(letter.toString(), ctrl = true, alt = alt)
        }

        // `unicodeChar` already factors in Shift (and any other active
        // meta state) to produce the correctly-cased/shifted character —
        // no separate shift handling needed for the printable-text path.
        if (unicodeChar != 0) {
            return Command.Text(unicodeChar.toChar().toString(), ctrl = false, alt = alt)
        }

        return null
    }

    private fun nonPrintableKeyCode(keyCode: Int): Int? = when (keyCode) {
        KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER -> KEY_ENTER
        KeyEvent.KEYCODE_TAB -> KEY_TAB
        KeyEvent.KEYCODE_DEL -> KEY_BACKSPACE
        KeyEvent.KEYCODE_FORWARD_DEL -> KEY_DELETE
        KeyEvent.KEYCODE_ESCAPE -> KEY_ESCAPE
        KeyEvent.KEYCODE_DPAD_UP -> KEY_UP
        KeyEvent.KEYCODE_DPAD_DOWN -> KEY_DOWN
        KeyEvent.KEYCODE_DPAD_LEFT -> KEY_LEFT
        KeyEvent.KEYCODE_DPAD_RIGHT -> KEY_RIGHT
        KeyEvent.KEYCODE_MOVE_HOME -> KEY_HOME
        KeyEvent.KEYCODE_MOVE_END -> KEY_END
        KeyEvent.KEYCODE_PAGE_UP -> KEY_PAGE_UP
        KeyEvent.KEYCODE_PAGE_DOWN -> KEY_PAGE_DOWN
        else -> null
    }

    /** Keys the system (IME show/hide, app back-navigation) must keep handling — never consumed as terminal input. */
    fun isSystemReserved(keyCode: Int): Boolean = keyCode == KeyEvent.KEYCODE_BACK ||
        keyCode == KeyEvent.KEYCODE_VOLUME_UP ||
        keyCode == KeyEvent.KEYCODE_VOLUME_DOWN ||
        keyCode == KeyEvent.KEYCODE_VOLUME_MUTE ||
        keyCode == KeyEvent.KEYCODE_POWER ||
        keyCode == KeyEvent.KEYCODE_HOME ||
        keyCode == KeyEvent.KEYCODE_APP_SWITCH
}
