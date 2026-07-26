package ai.choosh.notifications;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.content.Context;
import android.os.Build;

import java.nio.charset.StandardCharsets;
import java.util.Objects;

/** Concrete, API-safe NotificationSink backed by Android's NotificationManager. */
public final class AndroidNotificationSink implements NotificationSink {
    public static final String CHANNEL_ID = "choosh.waiting";
    private static final String TAG = "choosh";

    private final NotificationManager manager;
    private final Context context;

    public AndroidNotificationSink(Context context) {
        this.context = Objects.requireNonNull(context, "context").getApplicationContext();
        this.manager = this.context.getSystemService(NotificationManager.class);
        if (manager == null) throw new IllegalStateException("NotificationManager unavailable");
        ensureChannel();
    }

    public AndroidNotificationSink(NotificationManager manager) {
        this.context = null;
        this.manager = Objects.requireNonNull(manager, "manager");
        ensureChannel();
    }

    @Override public void upsert(NotificationIntent intent) {
        Objects.requireNonNull(intent, "intent");
        if (context == null) throw new IllegalStateException("Context required for posting notifications");
        Notification.Builder builder = Build.VERSION.SDK_INT >= Build.VERSION_CODES.O
                ? new Notification.Builder(context, CHANNEL_ID)
                : new Notification.Builder(context);
        Notification notification = builder.setSmallIcon(android.R.drawable.ic_dialog_info)
                .setContentTitle(intent.workspaceName() + " · " + intent.agentName())
                .setContentText(intent.reason())
                .setAutoCancel(true)
                .setOnlyAlertOnce(true)
                .build();
        manager.notify(TAG, notificationIdForKey(intent.key()), notification);
    }

    @Override public void clear(String stableKey) {
        manager.cancel(TAG, notificationIdForKey(Objects.requireNonNull(stableKey, "stableKey")));
    }

    public static int notificationIdForKey(String key) {
        byte[] bytes = Objects.requireNonNull(key, "key").getBytes(StandardCharsets.UTF_8);
        int hash = 0x811c9dc5;
        for (byte value : bytes) { hash ^= value & 0xff; hash *= 0x01000193; }
        return hash == 0 ? 1 : hash;
    }

    private void ensureChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            manager.createNotificationChannel(new NotificationChannel(
                    CHANNEL_ID, "Choosh waiting items", NotificationManager.IMPORTANCE_DEFAULT));
        }
    }
}
