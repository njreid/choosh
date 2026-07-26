package ai.choosh.notifications;

/** Injectable boundary to Android's notification manager. */
public interface NotificationSink {
    void upsert(NotificationIntent intent);
    void clear(String stableKey);
}
