package ai.choosh.notifications;

import java.util.ArrayList;
import java.util.List;
import java.util.function.Consumer;
import org.junit.Test;
import static org.junit.Assert.*;

public final class NotificationServiceLifecycleTest {
    @Test public void startIsIdempotentAndProjectsOnlyWhileStarted() {
        FakeSource source = new FakeSource();
        RecordingSink sink = new RecordingSink();
        NotificationServiceLifecycle service = new NotificationServiceLifecycle(source,
                new NotificationProjector(sink));
        service.start(); service.start();
        assertEquals(1, source.starts);
        source.emit(intent("i1"));
        service.stop();
        source.emit(intent("i2"));
        assertEquals(List.of("upsert:h:w:i1", "clear:h:w:i1"), sink.operations);
        assertEquals(1, source.stops);
    }

    @Test public void stopBeforeStartDoesNothing() {
        FakeSource source = new FakeSource();
        NotificationServiceLifecycle service = new NotificationServiceLifecycle(source,
                new NotificationProjector(new RecordingSink()));
        service.stop();
        assertFalse(service.isStarted());
        assertEquals(0, source.stops);
    }

    private static NotificationIntent intent(String id) {
        return new NotificationIntent("h", "w", id, "Workspace", "Agent", "question");
    }
    private static final class FakeSource implements NotificationSource {
        Consumer<NotificationIntent> listener; int starts; int stops;
        public void start(Consumer<NotificationIntent> listener) { this.listener = listener; starts++; }
        public void stop() { stops++; }
        void emit(NotificationIntent intent) { listener.accept(intent); }
    }
    private static final class RecordingSink implements NotificationSink {
        final List<String> operations = new ArrayList<>();
        public void upsert(NotificationIntent i) { operations.add("upsert:" + i.key()); }
        public void clear(String key) { operations.add("clear:" + key); }
    }
}
