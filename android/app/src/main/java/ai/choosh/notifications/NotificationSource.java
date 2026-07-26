package ai.choosh.notifications;

/** Injectable source of committed waiting intents (for example an SSH replay stream). */
public interface NotificationSource {
    void start(java.util.function.Consumer<NotificationIntent> listener);
    void stop();
}
