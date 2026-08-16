package ai.choosh.resources

import ai.choosh.engine.ResourcePattern
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * [resourceReauthDialogSections]'s per-[ResourcePattern] derivation — see
 * that function's own doc comment for why this is pulled out of
 * [ResourceReauthOverlay]'s Compose branching and unit-tested directly,
 * rather than only exercised (if at all) through an instrumented Compose
 * test. Pins down all four patterns' distinct shape explicitly, not just
 * one representative case, since a `==`/`!=` typo on any one of them would
 * otherwise only be caught by eyeballing the dialog on a real device.
 */
class ResourceReauthOverlayTest {

    @Test
    fun `pattern a shows url and code but no value field — it resolves on its own`() {
        val sections = resourceReauthDialogSections(ResourcePattern.A)
        assertEquals(
            ResourceReauthDialogSections(showsFetchInstructions = false, showsUrlAndCode = true, showsValueField = false),
            sections,
        )
    }

    @Test
    fun `pattern b shows url and code plus a value field for the pasted-back code`() {
        val sections = resourceReauthDialogSections(ResourcePattern.B)
        assertEquals(
            ResourceReauthDialogSections(showsFetchInstructions = false, showsUrlAndCode = true, showsValueField = true),
            sections,
        )
    }

    @Test
    fun `pattern c shows fetch instructions instead of url and code, plus a value field`() {
        val sections = resourceReauthDialogSections(ResourcePattern.C)
        assertEquals(
            ResourceReauthDialogSections(showsFetchInstructions = true, showsUrlAndCode = false, showsValueField = true),
            sections,
        )
    }

    @Test
    fun `pattern d shows url and code plus a value field, same shape as pattern b`() {
        val sections = resourceReauthDialogSections(ResourcePattern.D)
        assertEquals(
            ResourceReauthDialogSections(showsFetchInstructions = false, showsUrlAndCode = true, showsValueField = true),
            sections,
        )
    }

    @Test
    fun `an UNKNOWN forward-compat pattern falls back to the url-and-code-plus-value-field shape, not fetch instructions`() {
        // Per ResourcePattern.UNKNOWN's own doc comment (this app's forward-compatibility
        // fallback for a wire pattern value it doesn't yet recognize): the dialog must still
        // let the human act on it rather than silently rendering nothing, and must not
        // misread it as pattern c's fetch-instructions shape.
        val sections = resourceReauthDialogSections(ResourcePattern.UNKNOWN)
        assertEquals(
            ResourceReauthDialogSections(showsFetchInstructions = false, showsUrlAndCode = true, showsValueField = true),
            sections,
        )
    }
}
