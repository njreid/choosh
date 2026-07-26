package ai.choosh.notifications;

import java.util.HashMap;
import java.util.Map;

/** Projects committed waiting state into one redacted notification per item. */
public final class NotificationProjector {
    private final NotificationSink sink;
    private final Map<String, NotificationIntent> active = new HashMap<>();

    public NotificationProjector(NotificationSink sink) { this.sink = sink; }

    public void apply(NotificationIntent intent) {
        NotificationIntent previous = active.put(intent.key(), intent);
        if (!intent.equals(previous)) sink.upsert(intent);
    }

    public void clear(String hostId, String workspaceId, String itemId) {
        String key = hostId + ":" + workspaceId + ":" + itemId;
        if (active.remove(key) != null) sink.clear(key);
    }

    public void clearAll() {
        for (String key : active.keySet()) sink.clear(key);
        active.clear();
    }

    public int activeCount() { return active.size(); }
}
