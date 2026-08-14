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
 *  - [onMessageReceived] logs receipt only. There is nothing real to act on
 *    yet: `relayd`'s FCM dispatch is itself a logged stub (no GCP
 *    credentials exist in the environment this was built in — see
 *    `rust/choosh-relayd/src/ws.rs`'s `dispatch_fcm_push_stub`), so no real
 *    push has ever been sent to build real handling against. Rendering an
 *    actionable, redacted notification per notifications.md is real work
 *    for whenever a real push actually needs handling.
 */
class ChooshFirebaseMessagingService : FirebaseMessagingService() {

    override fun onNewToken(token: String) {
        Log.i(TAG, "FCM token rotated (not yet re-registered outside the connect flow — see class doc)")
    }

    override fun onMessageReceived(message: RemoteMessage) {
        // Redacted by construction: only logging that *a* message arrived,
        // never its content, per notifications.md's redaction rule.
        Log.i(TAG, "FCM message received (handling not yet implemented — see class doc)")
    }

    private companion object {
        const val TAG = "ChooshFcm"
    }
}
