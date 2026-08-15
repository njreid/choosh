package ai.choosh.notifications;

/** Injectable boundary to Android's notification manager. */
public interface NotificationSink {
    void upsert(RenderableNotification intent);
    void clear(String stableKey);
}
