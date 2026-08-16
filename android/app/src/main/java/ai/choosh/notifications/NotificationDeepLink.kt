package ai.choosh.notifications

/**
 * A notification tap's fully-resolved navigation target, derived from the
 * redacted extras an `input_required` notification's `PendingIntent`
 * carries — per notifications.md's "Actionability" section ("tapping
 * connects if necessary, opens the workspace, ensures the relevant item is
 * pinned, focuses it, and acknowledges the notification") and
 * android-navigation.md's "Notification deep link" section.
 *
 * `auth_required` (`AuthNotificationIntent`) never produces one of these:
 * per notifications.md, its `PendingIntent` opens `verificationUri`
 * directly (see [AndroidNotificationSink]'s own construction) rather than
 * routing through `MainActivity`/this parsing path at all.
 */
sealed interface NotificationDeepLinkTarget {
    data class OpenItem(val hostId: String, val workspaceId: String, val itemId: String) : NotificationDeepLinkTarget
}

/**
 * Pure, Android-framework-free construction/parsing of an `input_required`
 * notification tap's `PendingIntent` extras — split out from
 * [AndroidNotificationSink] (which needs a real `Context`/`Intent` to
 * exercise) so this is unit-testable without Robolectric, mirroring
 * [FcmNotificationParser]'s and [AndroidNotificationSink.notificationIdForKey]'s
 * existing pure-static-helper convention. [extrasFor] is the write side
 * (called from [AndroidNotificationSink] when building the explicit
 * `Intent` targeting `ai.choosh.MainActivity`); [parse] is the read side
 * (called from `MainActivity` on `onCreate`/`onNewIntent`), operating on
 * plain `Map<String, String?>` rather than `android.content.Intent`/`Bundle`
 * directly so both directions stay testable as ordinary JVM unit tests.
 */
object NotificationDeepLink {
    const val EXTRA_HOST_ID = "ai.choosh.notifications.EXTRA_HOST_ID"
    const val EXTRA_WORKSPACE_ID = "ai.choosh.notifications.EXTRA_WORKSPACE_ID"
    const val EXTRA_ITEM_ID = "ai.choosh.notifications.EXTRA_ITEM_ID"

    /** Extras to attach to the explicit `MainActivity` `Intent` an `input_required` `PendingIntent` carries. */
    @JvmStatic
    fun extrasFor(intent: NotificationIntent): Map<String, String> = mapOf(
        EXTRA_HOST_ID to intent.hostId(),
        EXTRA_WORKSPACE_ID to intent.workspaceId(),
        EXTRA_ITEM_ID to intent.itemId(),
    )

    /**
     * Parses extras (keyed identically to [extrasFor]'s output — the
     * caller reads them off `Intent.getStringExtra`/`Bundle.getString`)
     * back into a navigation target. `null` when any required key is
     * missing/blank — a cold start via the launcher icon (no notification
     * extras at all) is the common case this must not misfire on.
     */
    @JvmStatic
    fun parse(extras: Map<String, String?>): NotificationDeepLinkTarget.OpenItem? {
        val hostId = extras[EXTRA_HOST_ID]
        val workspaceId = extras[EXTRA_WORKSPACE_ID]
        val itemId = extras[EXTRA_ITEM_ID]
        if (hostId.isNullOrBlank() || workspaceId.isNullOrBlank() || itemId.isNullOrBlank()) return null
        return NotificationDeepLinkTarget.OpenItem(hostId, workspaceId, itemId)
    }
}
