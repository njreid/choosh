package ai.choosh.notifications;

/**
 * Common contract for the two redacted notification intent shapes that
 * flow through {@link NotificationSink}/{@link NotificationProjector} —
 * {@link NotificationIntent} for
 * {@code input_required} (keyed {@code (host_id, workspace_id, item_id)})
 * and {@link AuthNotificationIntent} for {@code auth_required} (keyed
 * {@code (host_id, provider)}). Sealed to exactly these two per
 * notifications.md's "Notifying events" section: "Only two normalized
 * events... produce a notification".
 */
public sealed interface RenderableNotification permits NotificationIntent, AuthNotificationIntent {
    /** Stable dedup key, per notifications.md's "Dedup" section. */
    String key();
}
