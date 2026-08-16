package ai.choosh

import ai.choosh.notifications.AndroidNotificationSink
import ai.choosh.notifications.NotificationDeepLink
import ai.choosh.notifications.NotificationDeepLinkTarget
import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/**
 * Per docs/specs/android-navigation.md's "Notification deep link" section /
 * notifications.md's "Actionability" section: a tapped `input_required`
 * notification's `PendingIntent` (built in
 * [ai.choosh.notifications.AndroidNotificationSink]) targets this Activity
 * explicitly, carrying [NotificationDeepLink]'s redacted host/workspace/item
 * extras. Both entry points funnel into the same [pendingDeepLink] Compose
 * state that [ChooshApp] reads to navigate:
 *  - [onCreate] — a cold start (app not running, or its task not alive),
 *    the tapped `Intent` is the one [getIntent] already returns.
 *  - [onNewIntent] — the app already has a running/backgrounded task; the
 *    `PendingIntent`'s `FLAG_ACTIVITY_CLEAR_TOP | FLAG_ACTIVITY_SINGLE_TOP`
 *    (see `AndroidNotificationSink.pendingIntentFor`) route back into this
 *    same instance instead of stacking a duplicate `MainActivity`.
 *
 * `auth_required` notifications never reach this Activity at all — per
 * notifications.md, their `PendingIntent` opens `verificationUri` directly
 * in a browser (see `AndroidNotificationSink.pendingIntentFor`'s own doc
 * comment), so there is no corresponding handling here.
 */
class MainActivity : ComponentActivity() {
    private var pendingDeepLink by mutableStateOf<NotificationDeepLinkTarget.OpenItem?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        pendingDeepLink = consumeDeepLink(intent)
        setContent {
            ChooshApp(
                context = applicationContext,
                pendingDeepLink = pendingDeepLink,
                onDeepLinkConsumed = { pendingDeepLink = null },
            )
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        pendingDeepLink = consumeDeepLink(intent)
    }

    /**
     * Extracts a notification tap's target from [intent]'s extras via
     * [NotificationDeepLink.parse] (`null` for a normal launcher-icon start
     * with no such extras) and, when present, acknowledges the originating
     * notification per notifications.md's "Dedup" section — "the user opens
     * the relevant terminal ... from the notification (acknowledging it)".
     * This is belt-and-suspenders alongside
     * `Notification.Builder.setAutoCancel(true)` (already clears the shade
     * entry on tap on its own): cancelling explicitly here by the same
     * `(host_id, workspace_id, item_id)` key
     * [ai.choosh.notifications.NotificationIntent.key] uses keeps this
     * correct even if that behavior ever changes.
     */
    private fun consumeDeepLink(intent: Intent?): NotificationDeepLinkTarget.OpenItem? {
        val extras = intent?.extras ?: return null
        val target = NotificationDeepLink.parse(
            mapOf(
                NotificationDeepLink.EXTRA_HOST_ID to extras.getString(NotificationDeepLink.EXTRA_HOST_ID),
                NotificationDeepLink.EXTRA_WORKSPACE_ID to extras.getString(NotificationDeepLink.EXTRA_WORKSPACE_ID),
                NotificationDeepLink.EXTRA_ITEM_ID to extras.getString(NotificationDeepLink.EXTRA_ITEM_ID),
            ),
        ) ?: return null
        runCatching {
            AndroidNotificationSink(applicationContext)
                .clear("${target.hostId}:${target.workspaceId}:${target.itemId}")
        }
        return target
    }
}
