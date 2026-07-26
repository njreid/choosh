package ai.choosh.notifications;

import org.junit.Test;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNotEquals;

public final class AndroidNotificationSinkTest {
    @Test public void notificationIdsAreStableAndNonZero() {
        int first = AndroidNotificationSink.notificationIdForKey("h:w:i");
        assertEquals(first, AndroidNotificationSink.notificationIdForKey("h:w:i"));
        assertNotEquals(0, first);
        assertNotEquals(first, AndroidNotificationSink.notificationIdForKey("h:w:j"));
    }
}
