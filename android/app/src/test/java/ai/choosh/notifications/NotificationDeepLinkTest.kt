package ai.choosh.notifications

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * Exercises [NotificationDeepLink]'s pure extras-construction and
 * extras-to-navigation-target parsing, without any real `Context`/
 * `Intent`/`Bundle` — mirroring [FcmNotificationParserTest]'s existing
 * pure-JVM-unit-test convention for this package. This is the logic
 * [AndroidNotificationSink.pendingIntentFor] and `MainActivity.consumeDeepLink`
 * are thin Android-framework glue around.
 */
class NotificationDeepLinkTest {
    private val intent = NotificationIntent(
        "host-1", "workspace-1", "item-1", "My Workspace", "claude-code", "permission",
    )

    @Test fun extrasForCarriesRedactedHostWorkspaceItemOnly() {
        val extras = NotificationDeepLink.extrasFor(intent)
        assertEquals(
            mapOf(
                NotificationDeepLink.EXTRA_HOST_ID to "host-1",
                NotificationDeepLink.EXTRA_WORKSPACE_ID to "workspace-1",
                NotificationDeepLink.EXTRA_ITEM_ID to "item-1",
            ),
            extras,
        )
        // Per notifications.md's "Redaction" section: workspaceName/agentName/reason
        // must never leak into the tap-target extras — they're display-only.
        assertEquals(3, extras.size)
    }

    @Test fun parseRoundTripsExtrasForIntoTheSameTarget() {
        val target = NotificationDeepLink.parse(NotificationDeepLink.extrasFor(intent))
        assertEquals(NotificationDeepLinkTarget.OpenItem("host-1", "workspace-1", "item-1"), target)
    }

    @Test fun parseReturnsNullWhenAnyRequiredKeyIsMissing() {
        assertNull(NotificationDeepLink.parse(emptyMap()))
        assertNull(
            NotificationDeepLink.parse(
                mapOf(
                    NotificationDeepLink.EXTRA_HOST_ID to "host-1",
                    NotificationDeepLink.EXTRA_WORKSPACE_ID to "workspace-1",
                    // item id missing
                ),
            ),
        )
    }

    @Test fun parseReturnsNullWhenAnyRequiredValueIsBlank() {
        assertNull(
            NotificationDeepLink.parse(
                mapOf(
                    NotificationDeepLink.EXTRA_HOST_ID to "host-1",
                    NotificationDeepLink.EXTRA_WORKSPACE_ID to "  ",
                    NotificationDeepLink.EXTRA_ITEM_ID to "item-1",
                ),
            ),
        )
    }

    // A cold start from the launcher icon has a `null`-extras `Intent`
    // (`MainActivity.consumeDeepLink` returns `null` before ever calling
    // [NotificationDeepLink.parse] in that case) — this asserts the
    // narrower but related case of extras present with `null` values
    // (`Bundle.getString` returns `null` for an absent key), the shape
    // `MainActivity` actually feeds this function.
    @Test fun parseReturnsNullWhenAllValuesAreNull() {
        assertNull(
            NotificationDeepLink.parse(
                mapOf(
                    NotificationDeepLink.EXTRA_HOST_ID to null,
                    NotificationDeepLink.EXTRA_WORKSPACE_ID to null,
                    NotificationDeepLink.EXTRA_ITEM_ID to null,
                ),
            ),
        )
    }
}
