package ai.choosh.resources

import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.MobileProfile
import ai.choosh.engine.ResourcePattern
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch

/**
 * The phone-side UI for the two agent-event-triggered prompts
 * docs/specs/resources-and-reauth.md introduces — rendered as a dialog
 * overlay on top of whatever screen is currently showing, the same
 * "notification-style, actionable surface" shape `ai.choosh.notifications`
 * already uses for FCM-delivered `auth_required`/`input_required` pushes,
 * but for the *live*, already-connected-to-the-app case
 * [ai.choosh.agentevents.AgentEventSubscription] feeds. One instance, owned
 * by the composition root ([ai.choosh.ChooshApp]) alongside
 * [ai.choosh.agentevents.AgentAttentionTracker]/[ai.choosh.agentevents.AgentStatusTracker],
 * so it's visible regardless of which [ai.choosh.Screen] is on top —
 * exactly the "reauth is devhost-scoped, not screen-scoped" property
 * `resources-and-reauth.md` itself describes.
 *
 * Shows at most one dialog at a time — a [ResourceReauthDialog] takes
 * priority over a [PendingResourceProposalDialog] if both are queued;
 * `resourceEventTracker`'s own queues (not a "latest wins" slot) mean
 * nothing is silently dropped, just ordered.
 */
@Composable
fun ResourceEventOverlay(engine: ChooshEngine, resourceEventTracker: ResourceEventTracker, modifier: Modifier = Modifier) {
    val reauthPrompts by resourceEventTracker.reauthPrompts.collectAsState()
    val pendingProposals by resourceEventTracker.pendingProposals.collectAsState()
    val scope = rememberCoroutineScope()

    val activePrompt = reauthPrompts.firstOrNull()
    if (activePrompt != null) {
        ResourceReauthDialog(
            prompt = activePrompt,
            onSubmit = { value ->
                val verified = engine.resourceReauthComplete(activePrompt.fromDeviceId, activePrompt.resourceId, value)
                if (verified) resourceEventTracker.dismissReauthPrompt(activePrompt.resourceId)
                verified
            },
            onDismiss = { resourceEventTracker.dismissReauthPrompt(activePrompt.resourceId) },
            modifier = modifier,
        )
        return
    }

    val activeProposal = pendingProposals.firstOrNull()
    if (activeProposal != null) {
        PendingResourceProposalDialog(
            proposal = activeProposal,
            onDecide = { approve ->
                scope.launch {
                    runCatching { engine.resourceConfirm(activeProposal.fromDeviceId, activeProposal.resourceId, approve) }
                    resourceEventTracker.dismissPendingProposal(activeProposal.resourceId)
                }
            },
            modifier = modifier,
        )
    }
}

/**
 * Which affordances [ResourceReauthDialog] shows for a given [ResourcePattern] —
 * pulled out of the composable as pure state, per this project's
 * "pure derivation, unit-tested directly" precedent
 * ([ai.choosh.connection.describeCreateCredentialFailure],
 * [ai.choosh.webservice.deriveWebServiceUiState]), since Compose branching
 * inline in the composable itself has no unit-test surface — the dialog's
 * per-pattern shape (fetch instructions vs. URL/code, submit field or not)
 * is exactly the "distinct UI-state derivation" a typo in the `==`/`!=`
 * checks below would otherwise silently get wrong for one pattern with no
 * test catching it.
 *
 * - **a**: [showsUrlAndCode], no [showsValueField] — pattern a resolves on
 *   its own; no value is ever typed back, per resources-and-reauth.md's
 *   taxonomy.
 * - **b/d**: [showsUrlAndCode] + [showsValueField].
 * - **c**: [showsFetchInstructions] instead of [showsUrlAndCode], plus
 *   [showsValueField].
 */
internal data class ResourceReauthDialogSections(
    val showsFetchInstructions: Boolean,
    val showsUrlAndCode: Boolean,
    val showsValueField: Boolean,
)

internal fun resourceReauthDialogSections(pattern: ResourcePattern): ResourceReauthDialogSections = ResourceReauthDialogSections(
    showsFetchInstructions = pattern == ResourcePattern.C,
    showsUrlAndCode = pattern != ResourcePattern.C,
    showsValueField = pattern != ResourcePattern.A,
)

/**
 * One [ResourceReauthPrompt], per [ResourcePattern] — see
 * [resourceReauthDialogSections]'s own doc comment for exactly which
 * sections each pattern shows.
 *
 * The "Open" button always fires a plain `ACTION_VIEW` Intent and lets
 * Android's own resolver do whatever cross-profile handoff it's going to do
 * — per resources-and-reauth.md's "Mobile profile targeting", there is no
 * reliable API for a third-party app to force a link into a specific other
 * profile, so the button is only *labeled* by [ResourceReauthPrompt.mobileProfile]
 * ("Open in Work profile" / "Open in Personal profile" / "Open"). A
 * copy-to-clipboard action is always offered alongside, per that same
 * section's documented fallback.
 */
@Composable
private fun ResourceReauthDialog(prompt: ResourceReauthPrompt, onSubmit: suspend (String) -> Boolean, onDismiss: () -> Unit, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    var value by remember(prompt.resourceId) { mutableStateOf("") }
    var submitting by remember(prompt.resourceId) { mutableStateOf(false) }
    var result by remember(prompt.resourceId) { mutableStateOf<Boolean?>(null) }
    val scope = rememberCoroutineScope()
    val sections = resourceReauthDialogSections(prompt.pattern)

    AlertDialog(
        onDismissRequest = onDismiss,
        modifier = modifier.testTag("resource-reauth-dialog-${prompt.resourceId}"),
        title = { Text(prompt.displayName) },
        text = {
            Column {
                Text(prompt.resourceKind, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)

                if (sections.showsFetchInstructions) {
                    Text(
                        prompt.fetchInstructions ?: "Fetch the required value, then submit it below.",
                        modifier = Modifier.padding(top = 8.dp).testTag("resource-reauth-fetch-instructions"),
                    )
                } else if (sections.showsUrlAndCode) {
                    prompt.verificationUri?.let { uri ->
                        Text(uri, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.padding(top = 8.dp).testTag("resource-reauth-uri"))
                        Row(Modifier.padding(top = 4.dp)) {
                            TextButton(onClick = { openUri(context, uri) }, modifier = Modifier.testTag("resource-reauth-open-button")) {
                                Text(openLabel(prompt.mobileProfile))
                            }
                            TextButton(onClick = { copyToClipboard(context, "Resource verification URL", uri) }, modifier = Modifier.testTag("resource-reauth-copy-uri-button")) {
                                Text("Copy link")
                            }
                        }
                    }
                    prompt.userCode?.let { code ->
                        Text(
                            code,
                            style = MaterialTheme.typography.headlineSmall,
                            modifier = Modifier.padding(top = 12.dp).testTag("resource-reauth-user-code"),
                        )
                        TextButton(onClick = { copyToClipboard(context, "Resource verification code", code) }, modifier = Modifier.testTag("resource-reauth-copy-code-button")) {
                            Text("Copy code")
                        }
                    }
                }

                if (sections.showsValueField) {
                    OutlinedTextField(
                        value = value,
                        onValueChange = { value = it },
                        label = { Text("Value") },
                        modifier = Modifier.padding(top = 12.dp).testTag("resource-reauth-value-field"),
                    )
                    result?.let { verified ->
                        Text(
                            if (verified) "Verified" else "Verification failed — check the value and try again.",
                            color = if (verified) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.error,
                            modifier = Modifier.padding(top = 4.dp).testTag("resource-reauth-result"),
                        )
                    }
                }
            }
        },
        confirmButton = {
            if (sections.showsValueField) {
                TextButton(
                    enabled = !submitting && value.isNotBlank(),
                    onClick = {
                        submitting = true
                        scope.launch {
                            result = onSubmit(value)
                            submitting = false
                        }
                    },
                    modifier = Modifier.testTag("resource-reauth-submit-button"),
                ) { Text(if (submitting) "Submitting…" else "Submit") }
            }
        },
        dismissButton = { TextButton(onClick = onDismiss, modifier = Modifier.testTag("resource-reauth-dismiss-button")) { Text("Close") } },
    )
}

/** An agent-proposed pending Resource, per [PendingResourceProposal]'s own doc comment — Approve persists it (via [ChooshEngine.resourceConfirm]'s `approve = true`), Reject discards it. */
@Composable
private fun PendingResourceProposalDialog(proposal: PendingResourceProposal, onDecide: (Boolean) -> Unit, modifier: Modifier = Modifier) {
    AlertDialog(
        onDismissRequest = {}, // Requires an explicit choice — dismissing without deciding would leave the proposal pending forever with no further prompt.
        modifier = modifier.testTag("resource-proposal-dialog-${proposal.resourceId}"),
        title = { Text("An agent wants to declare a new Resource") },
        text = { Text("resource_id: ${proposal.resourceId}") },
        confirmButton = { TextButton(onClick = { onDecide(true) }, modifier = Modifier.testTag("resource-proposal-approve-button")) { Text("Approve") } },
        dismissButton = { TextButton(onClick = { onDecide(false) }, modifier = Modifier.testTag("resource-proposal-reject-button")) { Text("Reject") } },
    )
}

private fun openLabel(mobileProfile: MobileProfile): String = when (mobileProfile) {
    MobileProfile.WORK -> "Open in Work profile"
    MobileProfile.PERSONAL -> "Open in Personal profile"
    MobileProfile.ASK -> "Open"
}

private fun openUri(context: Context, uri: String) {
    runCatching { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(uri)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)) }
}

private fun copyToClipboard(context: Context, label: String, value: String) {
    (context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager)?.setPrimaryClip(ClipData.newPlainText(label, value))
}
