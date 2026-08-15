package ai.choosh.notifications;

import java.util.Objects;

/**
 * Owns the lifecycle boundary between a subscription source and Android notifications.
 * This class is platform-neutral so service wiring can be exercised without an Android OS.
 */
public final class NotificationServiceLifecycle {
    private final NotificationSource source;
    private final NotificationProjector projector;
    private boolean started;

    public NotificationServiceLifecycle(NotificationSource source, NotificationProjector projector) {
        this.source = Objects.requireNonNull(source, "source");
        this.projector = Objects.requireNonNull(projector, "projector");
    }

    /** Starts replay/live delivery once; repeated starts are harmless. */
    public void start() {
        if (started) return;
        started = true;
        source.start(this::onIntent);
    }

    /** Stops delivery and removes all projected notifications. */
    public void stop() {
        if (!started) return;
        started = false;
        source.stop();
        projector.clearAll();
    }

    public boolean isStarted() { return started; }

    private void onIntent(RenderableNotification intent) {
        if (started) projector.apply(Objects.requireNonNull(intent, "intent"));
    }
}
