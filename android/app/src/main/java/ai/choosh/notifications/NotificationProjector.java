package ai.choosh.notifications;

import java.util.HashMap;
import java.util.Map;

/**
 * Projects committed notification state into one redacted notification per
 * dedup key — {@code (host_id, workspace_id, item_id)} for
 * {@link NotificationIntent} (`input_required`) or {@code (host_id,
 * provider)} for {@link AuthNotificationIntent} (`auth_required`), per
 * notifications.md's "Dedup" section. A second event for the same key
 * updates the existing notification in place ({@link RenderableNotification#key()}
 * is what makes the two intent shapes share one map here) rather than
 * creating an additional one.
 */
public final class NotificationProjector {
    private final NotificationSink sink;
    private final Map<String, RenderableNotification> active = new HashMap<>();

    public NotificationProjector(NotificationSink sink) { this.sink = sink; }

    public void apply(RenderableNotification intent) {
        RenderableNotification previous = active.put(intent.key(), intent);
        if (!intent.equals(previous)) sink.upsert(intent);
    }

    /** Clears an `input_required` notification by its `(host_id, workspace_id, item_id)` key. */
    public void clear(String hostId, String workspaceId, String itemId) {
        clearKey(hostId + ":" + workspaceId + ":" + itemId);
    }

    /**
     * Clears an `auth_required` notification by its `(host_id, provider)`
     * key — called when the SSO flow completes on its own, or the user
     * acknowledges it, per notifications.md's "Dedup" section.
     */
    public void clearAuth(String hostId, String provider) {
        clearKey(hostId + ":auth:" + provider);
    }

    private void clearKey(String key) {
        if (active.remove(key) != null) sink.clear(key);
    }

    public void clearAll() {
        for (String key : active.keySet()) sink.clear(key);
        active.clear();
    }

    public int activeCount() { return active.size(); }
}
