package ai.choosh.notifications;

import java.util.Map;

/**
 * Pure, Android-framework-free parser from an FCM data message's redacted
 * key/value payload into the same {@link RenderableNotification} shapes
 * the persistent-connection path would project through
 * {@link NotificationProjector}. Per notifications.md's "Delivery
 * mechanism": "The data message MUST carry the same redacted payload shape
 * the app would have rendered locally, not a bare 'open the app' ping".
 *
 * Kept separate from {@link ChooshFirebaseMessagingService} (which depends
 * on Android's {@code RemoteMessage}/{@code Context}) so this
 * parsing/redaction-boundary logic is unit-testable without Robolectric —
 * mirroring {@link AndroidNotificationSink#notificationIdForKey}'s existing
 * pure-static-helper convention.
 *
 * The wire shape read here matches `choosh-relayd`'s
 * {@code rust/choosh-relayd/src/fcm.rs} (`redacted_data_payload`), which is
 * itself derived directly from
 * {@code choosh_protocol::relay::WireAgentEvent}'s
 * {@code #[serde(tag = "kind", rename_all = "snake_case")]} shape: a
 * {@code "kind"} discriminator plus that event's own fields, string-valued,
 * plus a {@code "host_id"} the relay adds on top (the wire event itself
 * never carries which devhost it came from).
 *
 * <p><b>Redaction boundary</b>: this parser only ever reads the exact named
 * keys below off {@code data} for a recognized {@code kind} — any other key
 * present (a stray {@code "token"}/{@code "session_id"}/free-form field a
 * misbehaving relayd build might one day add to the payload) is never read,
 * so it can never reach a constructed {@link RenderableNotification} or,
 * downstream, a rendered notification string. This is the Android-side
 * mirror of the type-level guarantee
 * {@code choosh_protocol::relay::WireAgentEvent::AuthRequired}'s doc
 * comment and round-trip test enforce on the Rust side, and of
 * {@code choosh-hostd::auth_detect}'s narrow-capture discipline.
 */
public final class FcmNotificationParser {
    private FcmNotificationParser() {}

    /**
     * Returns {@code null} for a {@code kind} outside {@code input_required}/
     * {@code auth_required} (per notifications.md, every other normalized
     * event "MUST NOT produce a notification"), a missing/unrecognized
     * {@code kind}, or a missing/blank required field. Never throws — a
     * malformed or unrecognized push must degrade to "no notification
     * produced", not a crash in a system-triggered callback.
     */
    public static RenderableNotification parse(Map<String, String> data) {
        if (data == null) return null;
        String kind = data.get("kind");
        if (kind == null) return null;
        return switch (kind) {
            case "input_required" -> parseInputRequired(data);
            case "auth_required" -> parseAuthRequired(data);
            default -> null;
        };
    }

    private static RenderableNotification parseInputRequired(Map<String, String> data) {
        String hostId = data.get("host_id");
        String workspaceId = data.get("workspace_id");
        String itemId = data.get("item_id");
        String reason = data.get("reason");
        if (isBlank(hostId) || isBlank(workspaceId) || isBlank(itemId) || isBlank(reason)) return null;
        // The FCM data payload — unlike the live persistent-connection
        // path, which has a locally cached workspace/agent registry to
        // draw a display name from — carries no workspace display name or
        // agent name: `WireAgentEvent::InputRequired` itself has neither
        // field (see relay.rs), so `relayd`'s FCM payload can't either.
        // Falling back to the raw ids keeps `NotificationIntent`'s
        // non-blank invariant satisfied without inventing content. This is
        // a real display-fidelity gap in the FCM backstop path specifically
        // — not a redaction or dedup defect — tracked here rather than
        // silently papered over.
        try {
            return new NotificationIntent(hostId, workspaceId, itemId, workspaceId, itemId, reason);
        } catch (IllegalArgumentException e) {
            return null;
        }
    }

    private static RenderableNotification parseAuthRequired(Map<String, String> data) {
        String hostId = data.get("host_id");
        String provider = data.get("provider");
        String userCode = data.get("user_code");
        String verificationUri = data.get("verification_uri");
        if (isBlank(hostId) || isBlank(provider) || isBlank(userCode) || isBlank(verificationUri)) return null;
        try {
            return new AuthNotificationIntent(hostId, provider, userCode, verificationUri);
        } catch (IllegalArgumentException e) {
            return null;
        }
    }

    private static boolean isBlank(String value) { return value == null || value.isBlank(); }
}
