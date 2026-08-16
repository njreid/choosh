package ai.choosh.terminal

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * UX-friction audit finding #11: [TerminalScreen] used to show
 * `"Terminal: $itemId@$deviceId"` — two raw ids concatenated — even when a
 * real name was available earlier in the flow. [terminalScreenTitle] is the
 * fix, pinned down directly here.
 */
class TerminalScreenTitleTest {
    @Test
    fun `prefers the real item name when available`() {
        assertEquals("Terminal: app", terminalScreenTitle("dev-mbp-home", "ws-choosh-app", "app"))
    }

    @Test
    fun `falls back to the raw id pairing only when the name is genuinely unavailable`() {
        assertEquals("Terminal: ws-choosh-app@dev-mbp-home", terminalScreenTitle("dev-mbp-home", "ws-choosh-app", null))
    }
}
