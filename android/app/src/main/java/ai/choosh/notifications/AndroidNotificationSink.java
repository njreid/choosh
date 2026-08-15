package ai.choosh.notifications;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.content.Context;

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

    @Override public void upsert(RenderableNotification intent) {
        Objects.requireNonNull(intent, "intent");
        if (context == null) throw new IllegalStateException("Context required for posting notifications");
        // minSdk is 26, so the channel-less pre-O Builder is unreachable; see ADR 0006.
        // auth_required notifications are always open-app-only per
        // notifications.md's "Actionability" section (no .addAction calls
        // here, for either intent type — input_required's own direct
        // approve/reject actions are a separate, not-yet-implemented gap;
        // see PLAN.md's "Terminal accessibility"/actionability follow-ups).
        Notification notification = new Notification.Builder(context, CHANNEL_ID)
                .setSmallIcon(android.R.drawable.ic_dialog_info)
                .setContentTitle(titleFor(intent))
                .setContentText(textFor(intent))
                .setAutoCancel(true)
                .setOnlyAlertOnce(true)
                .build();
        manager.notify(TAG, notificationIdForKey(intent.key()), notification);
    }

    @Override public void clear(String stableKey) {
        manager.cancel(TAG, notificationIdForKey(Objects.requireNonNull(stableKey, "stableKey")));
    }

    /**
     * Pure, Android-framework-free title/text construction — split out from
     * {@link #upsert} (which needs a real {@link Context}/
     * {@link Notification.Builder} to exercise) so this is unit-testable
     * without Robolectric, mirroring {@link #notificationIdForKey}'s
     * existing pure-static-helper convention. Both only ever read the
     * intent's own typed fields, never anything outside them, which is
     * what keeps a rendered notification's text within
     * notifications.md's redaction rule.
     */
    static String titleFor(RenderableNotification intent) {
        if (intent instanceof NotificationIntent waiting) {
            return waiting.workspaceName() + " · " + waiting.agentName();
        }
        if (intent instanceof AuthNotificationIntent auth) {
            return providerDisplayName(auth.provider()) + " sign-in required";
        }
        throw new IllegalArgumentException("unknown RenderableNotification: " + intent);
    }

    static String textFor(RenderableNotification intent) {
        if (intent instanceof NotificationIntent waiting) {
            return waiting.reason();
        }
        if (intent instanceof AuthNotificationIntent auth) {
            // user_code and verification_uri are explicitly not secrets on
            // their own per notifications.md's "Redaction" section — they
            // are exactly what the user is meant to see and act on here.
            return auth.userCode() + " · " + auth.verificationUri();
        }
        throw new IllegalArgumentException("unknown RenderableNotification: " + intent);
    }

    static String providerDisplayName(String provider) {
        return switch (provider) {
            case "aws" -> "AWS";
            case "gcp" -> "Google Cloud";
            case "azure" -> "Azure";
            case "github" -> "GitHub";
            default -> provider;
        };
    }

    public static int notificationIdForKey(String key) {
        byte[] bytes = Objects.requireNonNull(key, "key").getBytes(StandardCharsets.UTF_8);
        int hash = 0x811c9dc5;
        for (byte value : bytes) { hash ^= value & 0xff; hash *= 0x01000193; }
        return hash == 0 ? 1 : hash;
    }

    private void ensureChannel() {
        manager.createNotificationChannel(new NotificationChannel(
                CHANNEL_ID, "Choosh waiting items", NotificationManager.IMPORTANCE_DEFAULT));
    }
}
