package ai.choosh;

import static org.junit.Assert.assertTrue;
import org.junit.Test;

/** Headless contract for stable accessibility targets; no UI toolkit or device required. */
public final class AccessibilitySemanticsTest {
    @Test public void allConnectionTargetsUseStableNames() {
        assertTrue(AccessibilitySemantics.isStableId(AccessibilitySemantics.HEADING));
        assertTrue(AccessibilitySemantics.isStableId(AccessibilitySemantics.PROFILE_FIELD));
        assertTrue(AccessibilitySemantics.isStableId(AccessibilitySemantics.CONNECT_ACTION));
        assertTrue(AccessibilitySemantics.isStableId(AccessibilitySemantics.STATUS));
    }
}
