package ai.choosh.notifications;

/** Injectable source of committed notification intents (for example an SSH replay stream). */
public interface NotificationSource {
    void start(java.util.function.Consumer<RenderableNotification> listener);
    void stop();
}
