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

    private static final class RecordingSink implements NotificationSink {
        final List<String> operations = new ArrayList<>();
        public void upsert(NotificationIntent intent) { operations.add("upsert:" + intent.key()); }
        public void clear(String key) { operations.add("clear:" + key); }
    }
}
