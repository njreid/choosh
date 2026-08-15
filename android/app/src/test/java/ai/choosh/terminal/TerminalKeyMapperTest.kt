package ai.choosh.terminal

import android.view.KeyEvent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * [TerminalKeyMapper.map] against the exact key-event shapes
 * `docs/accessibility-device-report.md`'s item 2 named as the confirmed gap
 * (Ctrl+C, arrow keys, plain typed text) — a plain JVM unit test, since the
 * mapper is a pure function over `KeyEvent`'s compile-time
 * `KEYCODE_*`/`META_*` constants, not a `View`/JNI dependency.
 */
class TerminalKeyMapperTest {
    @Test
    fun plain_letter_maps_to_text_with_no_modifiers() {
        val command = TerminalKeyMapper.map(KeyEvent.KEYCODE_A, 0, 'a'.code)
        assertEquals(TerminalKeyMapper.Command.Text("a", ctrl = false, alt = false), command)
    }

    @Test
    fun shifted_letter_uses_the_already_shifted_unicode_char() {
        // Android's real KeyEvent.getUnicodeChar(metaState) already
        // produces the uppercase char when shift is held — the mapper must
        // not re-derive case itself.
        val command = TerminalKeyMapper.map(KeyEvent.KEYCODE_A, KeyEvent.META_SHIFT_ON, 'A'.code)
        assertEquals(TerminalKeyMapper.Command.Text("A", ctrl = false, alt = false), command)
    }

    @Test
    fun ctrl_c_maps_to_text_c_with_ctrl_true() {
        // Real devices generally report unicodeChar=0 for a ctrl-held
        // letter, so this must come from the keycode range, not unicodeChar.
        val command = TerminalKeyMapper.map(KeyEvent.KEYCODE_C, KeyEvent.META_CTRL_ON, 0)
        assertEquals(TerminalKeyMapper.Command.Text("c", ctrl = true, alt = false), command)
    }

    @Test
    fun arrow_keys_map_to_the_wire_key_codes_matching_terminal_jni_rs() {
        assertEquals(TerminalKeyMapper.Command.Key(TerminalKeyMapper.KEY_UP, ctrl = false, alt = false, shift = false), TerminalKeyMapper.map(KeyEvent.KEYCODE_DPAD_UP, 0, 0))
        assertEquals(TerminalKeyMapper.Command.Key(TerminalKeyMapper.KEY_DOWN, ctrl = false, alt = false, shift = false), TerminalKeyMapper.map(KeyEvent.KEYCODE_DPAD_DOWN, 0, 0))
        assertEquals(TerminalKeyMapper.Command.Key(TerminalKeyMapper.KEY_LEFT, ctrl = false, alt = false, shift = false), TerminalKeyMapper.map(KeyEvent.KEYCODE_DPAD_LEFT, 0, 0))
        assertEquals(TerminalKeyMapper.Command.Key(TerminalKeyMapper.KEY_RIGHT, ctrl = false, alt = false, shift = false), TerminalKeyMapper.map(KeyEvent.KEYCODE_DPAD_RIGHT, 0, 0))
    }

    @Test
    fun enter_tab_backspace_escape_map_to_their_wire_codes() {
        assertEquals(TerminalKeyMapper.KEY_ENTER, (TerminalKeyMapper.map(KeyEvent.KEYCODE_ENTER, 0, 10) as TerminalKeyMapper.Command.Key).code)
        assertEquals(TerminalKeyMapper.KEY_TAB, (TerminalKeyMapper.map(KeyEvent.KEYCODE_TAB, 0, 9) as TerminalKeyMapper.Command.Key).code)
        assertEquals(TerminalKeyMapper.KEY_BACKSPACE, (TerminalKeyMapper.map(KeyEvent.KEYCODE_DEL, 0, 8) as TerminalKeyMapper.Command.Key).code)
        assertEquals(TerminalKeyMapper.KEY_ESCAPE, (TerminalKeyMapper.map(KeyEvent.KEYCODE_ESCAPE, 0, 27) as TerminalKeyMapper.Command.Key).code)
    }

    @Test
    fun home_end_page_up_page_down_map_to_their_wire_codes() {
        assertEquals(TerminalKeyMapper.KEY_HOME, (TerminalKeyMapper.map(KeyEvent.KEYCODE_MOVE_HOME, 0, 0) as TerminalKeyMapper.Command.Key).code)
        assertEquals(TerminalKeyMapper.KEY_END, (TerminalKeyMapper.map(KeyEvent.KEYCODE_MOVE_END, 0, 0) as TerminalKeyMapper.Command.Key).code)
        assertEquals(TerminalKeyMapper.KEY_PAGE_UP, (TerminalKeyMapper.map(KeyEvent.KEYCODE_PAGE_UP, 0, 0) as TerminalKeyMapper.Command.Key).code)
        assertEquals(TerminalKeyMapper.KEY_PAGE_DOWN, (TerminalKeyMapper.map(KeyEvent.KEYCODE_PAGE_DOWN, 0, 0) as TerminalKeyMapper.Command.Key).code)
    }

    @Test
    fun alt_modifier_is_carried_through_on_text_commands() {
        val command = TerminalKeyMapper.map(KeyEvent.KEYCODE_X, KeyEvent.META_ALT_ON, 'x'.code)
        assertEquals(TerminalKeyMapper.Command.Text("x", ctrl = false, alt = true), command)
    }

    @Test
    fun a_key_with_no_unicode_char_and_no_special_mapping_returns_null() {
        // e.g. a bare modifier key by itself (KEYCODE_CTRL_LEFT) never
        // reaches the mapper with a printable char nor a special mapping.
        assertNull(TerminalKeyMapper.map(KeyEvent.KEYCODE_CTRL_LEFT, KeyEvent.META_CTRL_ON, 0))
    }

    @Test
    fun back_and_volume_keys_are_reserved_for_the_system() {
        assert(TerminalKeyMapper.isSystemReserved(KeyEvent.KEYCODE_BACK))
        assert(TerminalKeyMapper.isSystemReserved(KeyEvent.KEYCODE_VOLUME_UP))
        assert(!TerminalKeyMapper.isSystemReserved(KeyEvent.KEYCODE_A))
    }
}
