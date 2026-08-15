package ai.choosh.notifications;

import org.junit.Test;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotEquals;
import static org.junit.Assert.assertTrue;

public final class AndroidNotificationSinkTest {
    @Test public void notificationIdsAreStableAndNonZero() {
        int first = AndroidNotificationSink.notificationIdForKey("h:w:i");
        assertEquals(first, AndroidNotificationSink.notificationIdForKey("h:w:i"));
        assertNotEquals(0, first);
        assertNotEquals(first, AndroidNotificationSink.notificationIdForKey("h:w:j"));
    }

    // titleFor/textFor are the exact strings AndroidNotificationSink.upsert
    // passes to Notification.Builder — pulled out as pure static helpers
    // (see their doc comment) specifically so the rendered text can be
    // asserted here without a real Context/Notification.Builder.

    @Test public void inputRequiredRendersWorkspaceAndAgentAsTitleAndReasonAsText() {
        NotificationIntent intent = new NotificationIntent("h", "w", "i", "My Workspace", "claude-code", "permission");
        assertEquals("My Workspace · claude-code", AndroidNotificationSink.titleFor(intent));
        assertEquals("permission", AndroidNotificationSink.textFor(intent));
    }

    @Test public void authRequiredRendersProviderDisplayNameAndCodePlusUri() {
        AuthNotificationIntent intent = new AuthNotificationIntent("h", "aws", "WDJB-MJHT", "https://example.com/device");
        assertEquals("AWS sign-in required", AndroidNotificationSink.titleFor(intent));
        assertEquals("WDJB-MJHT · https://example.com/device", AndroidNotificationSink.textFor(intent));
    }

    @Test public void everyKnownProviderGetsADisplayName() {
        assertEquals("AWS", AndroidNotificationSink.providerDisplayName("aws"));
        assertEquals("Google Cloud", AndroidNotificationSink.providerDisplayName("gcp"));
        assertEquals("Azure", AndroidNotificationSink.providerDisplayName("azure"));
        assertEquals("GitHub", AndroidNotificationSink.providerDisplayName("github"));
    }

    /**
     * The hard redaction rule under test (notifications.md: "MUST NOT
     * contain command text, tool arguments, file contents, prompts,
     * tokens, session identifiers, or any other credential material"),
     * exercised against the actual rendered strings: an
     * {@link AuthNotificationIntent} carries no field capable of holding
     * that content in the first place (it has exactly four components —
     * hostId/provider/userCode/verificationUri), so no adversarial input
     * to this record can make the rendered title/text contain a token or
     * session identifier. This mirrors the type-level guarantee
     * `choosh_protocol::relay::WireAgentEvent::AuthRequired`'s own
     * round-trip test asserts on the Rust side.
     */
    @Test public void authRequiredRenderedTextNeverContainsTokenOrSessionLanguage() {
        AuthNotificationIntent intent = new AuthNotificationIntent(
                "h", "aws", "WDJB-MJHT", "https://example.com/device");
        String rendered = AndroidNotificationSink.titleFor(intent) + " " + AndroidNotificationSink.textFor(intent);
        for (String forbidden : new String[] {"token", "session", "credential", "password", "secret"}) {
            assertFalse("rendered text must never contain \"" + forbidden + "\": " + rendered,
                    rendered.toLowerCase(java.util.Locale.ROOT).contains(forbidden));
        }
        // Sanity check the test isn't vacuous: the intended, non-secret
        // fields must actually be present.
        assertTrue(rendered.contains("WDJB-MJHT"));
        assertTrue(rendered.contains("https://example.com/device"));
    }
}
