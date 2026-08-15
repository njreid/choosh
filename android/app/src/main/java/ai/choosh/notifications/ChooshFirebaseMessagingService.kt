package ai.choosh.notifications

import android.util.Log
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage

/**
 * Receives FCM token rotation and incoming pushes system-wide, independent
 * of whether any part of the app is in the foreground — per DESIGN.md §7,
 * this is the backstop when the persistent relay connection isn't alive to
 * deliver an `agent-event` push directly.
 *
 * [onMessageReceived] parses the data payload via [FcmNotificationParser]
 * (kept Android-framework-free so it's unit-testable) and, for a
 * recognized `input_required`/`auth_required` `kind`, projects it through
 * the same [NotificationProjector]/[AndroidNotificationSink] path
 * `input_required`'s live persistent-connection delivery uses, per
 * notifications.md's "The data message MUST carry the same redacted
 * payload shape the app would have rendered locally". Every other `kind`
 * (or a missing/malformed payload) produces no notification, per
 * notifications.md's "Notifying events" section.
 *
 * Deliberately minimal for this pass, with the gaps stated rather than
 * hidden:
 *  - [onNewToken] logs the rotated token but does not re-register it with
 *    `relayd` itself — [ai.choosh.connection.ConnectionViewModel] registers
 *    the *current* token (Firebase's own SDK always returns the latest
 *    value from [com.google.firebase.messaging.FirebaseMessaging.getToken],
 *    regardless of when it last rotated) on every successful connect, which
 *    covers the common case. A token rotating while a connection is already
 *    live and staying live indefinitely afterward is not re-registered
 *    until the next connect — wiring a live-connection token-refresh path
 *    needs a way to reach an active [ai.choosh.engine.ChooshEngine] from a
 *    system-triggered callback that may run with no `ViewModel` alive at
 *    all, which is a real design question for a later pass, not solved
 *    here.
 *  - [projector] is a fresh [NotificationProjector] per service instance
 *    (Android may recreate this service between messages), so its
 *    in-memory dedup map does not persist across instances, and is not
 *    shared with the persistent-connection path's own projector (which
 *    doesn't exist as a live wiring yet either — see PLAN.md's "Known
 *    follow-ups"). This does not affect on-screen correctness: the actual
 *    Android notification is still upserted in place by
 *    [AndroidNotificationSink.notificationIdForKey]'s stable per-key id
 *    regardless of in-memory projector state, so a repeat push for the
 *    same key still updates rather than duplicates. It just means the
 *    "only re-alert if content actually changed" optimization
 *    [NotificationProjector.apply] provides can reset across service
 *    instances — sharing one projector/sink instance across both delivery
 *    paths is real follow-up work for whenever the persistent-connection
 *    path is itself wired to a live engine.
 */
class ChooshFirebaseMessagingService : FirebaseMessagingService() {

    private val projector: NotificationProjector by lazy {
        NotificationProjector(AndroidNotificationSink(applicationContext))
    }

    override fun onNewToken(token: String) {
        Log.i(TAG, "FCM token rotated (not yet re-registered outside the connect flow — see class doc)")
    }

    override fun onMessageReceived(message: RemoteMessage) {
        val intent = FcmNotificationParser.parse(message.data)
        if (intent == null) {
            // Redacted by construction: only logging the coarse `kind` tag
            // (an enum discriminator, not free-form content — the same
            // redaction discipline `relayd`'s own dispatch log lines use)
            // and whether it produced a notification, never any other
            // field value, per notifications.md's redaction rule.
            Log.i(TAG, "FCM message received (kind=${message.data["kind"]}), no notification produced")
            return
        }
        projector.apply(intent)
    }

    private companion object {
        const val TAG = "ChooshFcm"
    }
}
