package ai.choosh.notifications;

/**
 * Redacted, platform-neutral notification intent for {@code auth_required}
 * — a headless devhost needs the user to complete an SSO device-code flow.
 * Per notifications.md's "Redaction" section, this carries only the
 * provider's {@code user_code} and {@code verification_uri} ("since those
 * are meant to be shown to the user and are not secrets on their own") —
 * never a token, session identifier, or other credential material. Keyed
 * {@code (host_id, provider)}, unlike {@link NotificationIntent}'s
 * {@code (host_id, workspace_id, item_id)} — an {@code auth_required} event
 * has no workspace/item, per
 * {@code choosh_protocol::relay::WireAgentEvent::AuthRequired}'s own doc
 * comment.
 */
public record AuthNotificationIntent(
        String hostId,
        String provider,
        String userCode,
        String verificationUri) implements RenderableNotification {
    public AuthNotificationIntent {
        require(hostId, "hostId"); require(provider, "provider");
        require(userCode, "userCode"); require(verificationUri, "verificationUri");
    }
    @Override public String key() { return hostId + ":auth:" + provider; }
    private static void require(String value, String name) {
        if (value == null || value.isBlank() || value.indexOf('\0') >= 0) throw new IllegalArgumentException(name);
    }
}
