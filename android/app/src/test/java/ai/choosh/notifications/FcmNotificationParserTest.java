package ai.choosh.notifications;

import java.util.HashMap;
import java.util.Map;
import org.junit.Test;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

public final class FcmNotificationParserTest {

    @Test public void parsesAnInputRequiredDataPayloadIntoANotificationIntent() {
        Map<String, String> data = Map.of(
                "kind", "input_required",
                "host_id", "devhost-1",
                "workspace_id", "ws-1",
                "item_id", "item-1",
                "reason", "permission");
        RenderableNotification parsed = FcmNotificationParser.parse(data);
        assertTrue(parsed instanceof NotificationIntent);
        NotificationIntent intent = (NotificationIntent) parsed;
        assertEquals("devhost-1", intent.hostId());
        assertEquals("ws-1", intent.workspaceId());
        assertEquals("item-1", intent.itemId());
        assertEquals("permission", intent.reason());
        assertEquals("devhost-1:ws-1:item-1", intent.key());
    }

    @Test public void parsesAnAuthRequiredDataPayloadIntoAnAuthNotificationIntent() {
        Map<String, String> data = Map.of(
                "kind", "auth_required",
                "host_id", "devhost-1",
                "provider", "aws",
                "user_code", "WDJB-MJHT",
                "verification_uri", "https://example.com/device");
        RenderableNotification parsed = FcmNotificationParser.parse(data);
        assertTrue(parsed instanceof AuthNotificationIntent);
        AuthNotificationIntent intent = (AuthNotificationIntent) parsed;
        assertEquals("devhost-1", intent.hostId());
        assertEquals("aws", intent.provider());
        assertEquals("WDJB-MJHT", intent.userCode());
        assertEquals("https://example.com/device", intent.verificationUri());
        assertEquals("devhost-1:auth:aws", intent.key());
    }

    @Test public void aSecondAuthRequiredPushForTheSameHostAndProviderParsesToTheSameDedupKey() {
        Map<String, String> first = Map.of(
                "kind", "auth_required", "host_id", "devhost-1", "provider", "gcp",
                "user_code", "AAAA-1111", "verification_uri", "https://example.com/device");
        Map<String, String> second = Map.of(
                "kind", "auth_required", "host_id", "devhost-1", "provider", "gcp",
                "user_code", "BBBB-2222", "verification_uri", "https://example.com/device");
        RenderableNotification a = FcmNotificationParser.parse(first);
        RenderableNotification b = FcmNotificationParser.parse(second);
        assertEquals(a.key(), b.key());
    }

    @Test public void unrecognizedKindsProduceNoNotification() {
        for (String kind : new String[] {"turn_completed", "files_changed", "agent_status",
                "editor_attached", "editor_detached", "something_unknown"}) {
            Map<String, String> data = new HashMap<>();
            data.put("kind", kind);
            data.put("host_id", "devhost-1");
            assertNull("kind=" + kind + " must not produce a notification per notifications.md",
                    FcmNotificationParser.parse(data));
        }
    }

    @Test public void missingKindOrNullDataProducesNoNotification() {
        assertNull(FcmNotificationParser.parse(null));
        assertNull(FcmNotificationParser.parse(Map.of("host_id", "devhost-1")));
    }

    @Test public void missingRequiredFieldsProduceNoNotificationRatherThanThrowing() {
        assertNull(FcmNotificationParser.parse(Map.of("kind", "input_required", "host_id", "devhost-1")));
        assertNull(FcmNotificationParser.parse(Map.of(
                "kind", "auth_required", "host_id", "devhost-1", "provider", "aws")));
        assertNull(FcmNotificationParser.parse(Map.of(
                "kind", "auth_required", "host_id", "", "provider", "aws",
                "user_code", "WDJB-MJHT", "verification_uri", "https://example.com/device")));
    }

    /**
     * The Android-side mirror of `rust/choosh-relayd/src/fcm.rs`'s
     * `redacted_data_payload_for_auth_required_has_exactly_the_spec_fields_and_nothing_else`
     * test: even if a malformed/compromised payload smuggled extra keys
     * like "token" or "session_id" alongside the legitimate fields, this
     * parser only ever reads the four named keys for `auth_required` — so
     * nothing else can reach the constructed intent or, downstream, a
     * rendered notification.
     */
    @Test public void extraKeysInAnAdversarialPayloadNeverReachTheParsedIntent() {
        Map<String, String> data = new HashMap<>();
        data.put("kind", "auth_required");
        data.put("host_id", "devhost-1");
        data.put("provider", "aws");
        data.put("user_code", "WDJB-MJHT");
        data.put("verification_uri", "https://example.com/device");
        data.put("token", "super-secret-oauth-token");
        data.put("session_id", "sess-abc123");
        data.put("command", "aws sso login --profile prod");

        AuthNotificationIntent intent = (AuthNotificationIntent) FcmNotificationParser.parse(data);
        String everything = intent.toString();
        assertFalse(everything.contains("super-secret-oauth-token"));
        assertFalse(everything.contains("sess-abc123"));
        assertFalse(everything.contains("aws sso login"));
    }
}
