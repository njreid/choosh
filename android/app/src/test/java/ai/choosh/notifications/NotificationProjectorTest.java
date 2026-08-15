package ai.choosh.notifications;

import static org.junit.Assert.assertEquals;
import java.util.ArrayList;
import java.util.List;
import org.junit.Test;

public final class NotificationProjectorTest {
    @Test public void deduplicatesAndUpdatesByStableKey() {
        RecordingSink sink = new RecordingSink();
        NotificationProjector projector = new NotificationProjector(sink);
        NotificationIntent first = new NotificationIntent("h", "w", "i", "Workspace", "Agent", "permission");
        projector.apply(first); projector.apply(first);
        projector.apply(new NotificationIntent("h", "w", "i", "Workspace", "Agent", "question"));
        assertEquals(List.of("upsert:h:w:i", "upsert:h:w:i"), sink.operations);
        assertEquals(1, projector.activeCount());
    }

    @Test public void clearIsIdempotentAndClearAllRemovesOnlyActive() {
        RecordingSink sink = new RecordingSink();
        NotificationProjector projector = new NotificationProjector(sink);
        projector.apply(new NotificationIntent("h", "w", "i1", "W", "A", "question"));
        projector.apply(new NotificationIntent("h", "w", "i2", "W", "A", "question"));
        projector.clear("h", "w", "i1"); projector.clear("h", "w", "i1"); projector.clearAll();
        assertEquals(List.of("upsert:h:w:i1", "upsert:h:w:i2", "clear:h:w:i1", "clear:h:w:i2"), sink.operations);
    }

    // --- auth_required: keyed (host_id, provider), per notifications.md's
    // "Dedup" section. Mirrors the input_required tests above, exercised
    // through the same NotificationProjector/NotificationSink path.

    @Test public void authRequiredDeduplicatesAndUpdatesByHostAndProvider() {
        RecordingSink sink = new RecordingSink();
        NotificationProjector projector = new NotificationProjector(sink);
        AuthNotificationIntent first = new AuthNotificationIntent("h", "aws", "WDJB-MJHT", "https://example.com/device");
        projector.apply(first); projector.apply(first);
        // A second event for the same (host_id, provider) with a *different*
        // user_code (e.g. the device-code prompt refreshed) still updates in
        // place rather than creating a second notification.
        projector.apply(new AuthNotificationIntent("h", "aws", "ABCD-1234", "https://example.com/device"));
        assertEquals(List.of("upsert:h:auth:aws", "upsert:h:auth:aws"), sink.operations);
        assertEquals(1, projector.activeCount());
    }

    @Test public void authRequiredForDifferentProvidersOnTheSameHostAreIndependentNotifications() {
        RecordingSink sink = new RecordingSink();
        NotificationProjector projector = new NotificationProjector(sink);
        projector.apply(new AuthNotificationIntent("h", "aws", "WDJB-MJHT", "https://example.com/device"));
        projector.apply(new AuthNotificationIntent("h", "gcp", "ABCD-1234", "https://example.com/device"));
        assertEquals(2, projector.activeCount());
        assertEquals(List.of("upsert:h:auth:aws", "upsert:h:auth:gcp"), sink.operations);
    }

    @Test public void authRequiredClearIsIdempotentAndKeyedSeparatelyFromInputRequired() {
        RecordingSink sink = new RecordingSink();
        NotificationProjector projector = new NotificationProjector(sink);
        projector.apply(new AuthNotificationIntent("h", "aws", "WDJB-MJHT", "https://example.com/device"));
        // An input_required notification for the same host_id, coincidentally
        // sharing "aws" as its workspace_id, must not collide with the
        // auth_required key above.
        projector.apply(new NotificationIntent("h", "aws", "i1", "W", "A", "question"));
        assertEquals(2, projector.activeCount());
        projector.clearAuth("h", "aws"); projector.clearAuth("h", "aws");
        assertEquals(1, projector.activeCount());
        projector.clear("h", "aws", "i1");
        assertEquals(0, projector.activeCount());
    }

    private static final class RecordingSink implements NotificationSink {
        final List<String> operations = new ArrayList<>();
        public void upsert(RenderableNotification intent) { operations.add("upsert:" + intent.key()); }
        public void clear(String key) { operations.add("clear:" + key); }
    }
}
