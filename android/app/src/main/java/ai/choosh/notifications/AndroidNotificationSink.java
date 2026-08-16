package ai.choosh.notifications;

import ai.choosh.MainActivity;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.net.Uri;

import java.nio.charset.StandardCharsets;
import java.util.Map;
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
        // The single tap target itself IS wired below via setContentIntent,
        // per that same "Actionability" section's "tapping ... connects if
        // necessary, opens the workspace, ensures the relevant item is
        // pinned, focuses it, and acknowledges the notification".
        Notification notification = new Notification.Builder(context, CHANNEL_ID)
                .setSmallIcon(android.R.drawable.ic_dialog_info)
                .setContentTitle(titleFor(intent))
                .setContentText(textFor(intent))
                .setContentIntent(pendingIntentFor(intent))
                .setAutoCancel(true)
                .setOnlyAlertOnce(true)
                .build();
        manager.notify(TAG, notificationIdForKey(intent.key()), notification);
    }

    /**
     * The tap target for {@code intent}'s notification. {@link NotificationIntent}
     * (`input_required`) opens an explicit {@link MainActivity} {@code Intent}
     * carrying {@link NotificationDeepLink}'s redacted host/workspace/item
     * extras — {@code MainActivity} resolves those into a navigation target
     * on {@code onCreate}/{@code onNewIntent}, per
     * docs/specs/android-navigation.md's "Notification deep link" section.
     * {@code FLAG_ACTIVITY_CLEAR_TOP | FLAG_ACTIVITY_SINGLE_TOP} route back
     * into the single already-running {@code MainActivity} instance rather
     * than stacking a duplicate.
     *
     * {@link AuthNotificationIntent} (`auth_required`) is simpler per
     * notifications.md's "Actionability" section: "tapping opens the
     * verification_uri in a Custom Tab, per DESIGN.md §6" — this uses a
     * plain {@code ACTION_VIEW} browser {@code Intent} rather than a real
     * {@code androidx.browser} {@code CustomTabsIntent}: no
     * {@code androidx.browser} dependency is wired into this build yet (a
     * real, tracked gap — see PLAN.md — not silently substituted), but
     * {@code ACTION_VIEW} already delivers the substantive behavior this
     * section requires: tapping shows the user {@code verificationUri} to
     * act on, without ever routing through {@code MainActivity} at all.
     */
    private PendingIntent pendingIntentFor(RenderableNotification intent) {
        int requestCode = notificationIdForKey(intent.key());
        if (intent instanceof NotificationIntent waiting) {
            Intent activityIntent = new Intent(context, MainActivity.class);
            activityIntent.setFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP | Intent.FLAG_ACTIVITY_SINGLE_TOP);
            for (Map.Entry<String, String> extra : NotificationDeepLink.extrasFor(waiting).entrySet()) {
                activityIntent.putExtra(extra.getKey(), extra.getValue());
            }
            return PendingIntent.getActivity(context, requestCode, activityIntent,
                    PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        }
        if (intent instanceof AuthNotificationIntent auth) {
            Intent viewIntent = new Intent(Intent.ACTION_VIEW, Uri.parse(auth.verificationUri()));
            viewIntent.setFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            return PendingIntent.getActivity(context, requestCode, viewIntent,
                    PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        }
        throw new IllegalArgumentException("unknown RenderableNotification: " + intent);
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
