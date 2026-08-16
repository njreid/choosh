package ai.choosh.resources

import ai.choosh.engine.MobileProfile
import ai.choosh.engine.Resource
import ai.choosh.engine.ResourcePattern
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp

/**
 * The real per-devhost Resources list, per docs/specs/resources-and-reauth.md —
 * `display_name`/`resource_kind`/pattern (or "connection" for a non-reauth
 * Resource)/`last_used_at`/`last_verified_at`. Reached from
 * [ai.choosh.fleet.DevHostWorkspacesScreen], the same "a new per-devhost
 * thing you can navigate to" spot that screen itself was added at
 * (docs/specs/android-navigation.md's "Workspace entry" precedent) — a
 * Resource is devhost-scoped, same as a Workspace list, so this reuses that
 * screen's own top-row navigation affordance rather than inventing a
 * separate fleet-drawer entry point.
 *
 * Includes a minimal "declare a Resource directly" form (display name, kind
 * as free text, pattern picker, reauth command, mobile profile picker) that
 * exercises `resource.propose`/`resource.confirm` end to end — see
 * [ResourcesViewModel]'s own doc comment for why this never auto-confirms.
 */
@Composable
fun ResourcesScreen(
    deviceId: String,
    state: ResourcesUiState,
    onRefresh: () -> Unit,
    onBack: () -> Unit,
    onPropose: (displayName: String, resourceKind: String, pattern: ResourcePattern?, reauthCommand: String?, mobileProfile: MobileProfile) -> Unit,
    onConfirmSelfProposal: (resourceId: String, approve: Boolean) -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(8.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.SpaceBetween) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Button(onClick = onBack) { Text("Back") }
                Text("Resources on $deviceId", style = MaterialTheme.typography.titleMedium, modifier = Modifier.padding(start = 12.dp))
            }
            TextButton(onClick = onRefresh, modifier = Modifier.testTag("resources-refresh-button")) { Text("Refresh") }
        }

        when {
            state.isLoading -> Text("Loading…", modifier = Modifier.padding(16.dp))
            state.error != null -> Text(
                "Couldn't load resources: ${state.error}",
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(16.dp).testTag("resources-error"),
            )
            else -> LazyColumn(Modifier.weight(1f).testTag("resource-list")) {
                if (state.resources.isEmpty()) {
                    item { Text("No resources declared on this devhost yet.", modifier = Modifier.padding(16.dp).testTag("resources-empty")) }
                }
                items(state.resources, key = { it.resourceId }) { resource -> ResourceRow(resource) }
                if (state.pendingSelfProposals.isNotEmpty()) {
                    item { Text("Pending your approval", style = MaterialTheme.typography.titleSmall, modifier = Modifier.padding(16.dp, 12.dp, 16.dp, 4.dp)) }
                    items(state.pendingSelfProposals, key = { it.resourceId }) { pending ->
                        PendingSelfProposalRow(pending, onConfirm = { approve -> onConfirmSelfProposal(pending.resourceId, approve) })
                    }
                }
            }
        }

        if (state.proposeError != null) {
            Text(
                "Couldn't propose resource: ${state.proposeError}",
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(16.dp, 4.dp).testTag("resource-propose-error"),
            )
        }

        NewResourceForm(inFlight = state.proposeInFlight, onPropose = onPropose)
    }
}

@Composable
private fun ResourceRow(resource: Resource) {
    val patternLabel = resource.pattern?.name?.let { "pattern $it" } ?: "connection"
    Column(
        Modifier
            .fillMaxWidth()
            .testTag("resource-row-${resource.resourceId}")
            .semantics { contentDescription = "${resource.displayName}, ${resource.resourceKind}, $patternLabel" }
            .padding(horizontal = 16.dp, vertical = 12.dp),
    ) {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            Text(resource.displayName)
            Text(patternLabel, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        Text(resource.resourceKind, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(
            "last used: ${resource.lastUsedAt ?: "never"} · last verified: ${resource.lastVerifiedAt ?: "never"}",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun PendingSelfProposalRow(pending: PendingSelfProposal, onConfirm: (Boolean) -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .testTag("resource-pending-row-${pending.resourceId}")
            .padding(horizontal = 16.dp, vertical = 8.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(pending.displayName)
        Row {
            TextButton(onClick = { onConfirm(true) }, modifier = Modifier.testTag("resource-pending-approve-${pending.resourceId}")) { Text("Approve") }
            TextButton(onClick = { onConfirm(false) }, modifier = Modifier.testTag("resource-pending-reject-${pending.resourceId}")) { Text("Reject") }
        }
    }
}

private val PATTERN_OPTIONS: List<ResourcePattern?> = listOf(null, ResourcePattern.A, ResourcePattern.B, ResourcePattern.C, ResourcePattern.D)

@Composable
private fun NewResourceForm(inFlight: Boolean, onPropose: (String, String, ResourcePattern?, String?, MobileProfile) -> Unit) {
    var displayName by remember { mutableStateOf("") }
    var resourceKind by remember { mutableStateOf("") }
    var pattern by remember { mutableStateOf<ResourcePattern?>(null) }
    var reauthCommand by remember { mutableStateOf("") }
    var mobileProfile by remember { mutableStateOf(MobileProfile.ASK) }

    Column(Modifier.padding(horizontal = 16.dp, vertical = 4.dp)) {
        Text("Declare a resource", style = MaterialTheme.typography.titleSmall)
        OutlinedTextField(
            value = displayName,
            onValueChange = { displayName = it },
            label = { Text("Display name") },
            modifier = Modifier.fillMaxWidth().testTag("new-resource-name-field"),
        )
        OutlinedTextField(
            value = resourceKind,
            onValueChange = { resourceKind = it },
            label = { Text("Kind (e.g. aws-sso, custom)") },
            modifier = Modifier.fillMaxWidth().testTag("new-resource-kind-field"),
        )
        Text("Pattern (blank = a plain connection, not a re-auth flow)", style = MaterialTheme.typography.labelSmall)
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceEvenly) {
            PATTERN_OPTIONS.forEach { option ->
                TextButton(
                    onClick = { pattern = option },
                    modifier = Modifier.testTag("new-resource-pattern-${option?.name?.lowercase() ?: "none"}"),
                ) { Text(if (option == null) "none" else option.name) }
            }
        }
        OutlinedTextField(
            value = reauthCommand,
            onValueChange = { reauthCommand = it },
            label = { Text("Reauth command (or description)") },
            modifier = Modifier.fillMaxWidth().testTag("new-resource-reauth-command-field"),
        )
        Text("Mobile profile", style = MaterialTheme.typography.labelSmall)
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceEvenly) {
            MobileProfile.entries.forEach { profile ->
                TextButton(
                    onClick = { mobileProfile = profile },
                    modifier = Modifier.testTag("new-resource-profile-${profile.name.lowercase()}"),
                ) { Text(profile.name.lowercase().replaceFirstChar { it.uppercase() }) }
            }
        }
        TextButton(
            enabled = !inFlight,
            onClick = { onPropose(displayName, resourceKind, pattern, reauthCommand.ifBlank { null }, mobileProfile) },
            modifier = Modifier.testTag("new-resource-propose-button"),
        ) { Text(if (inFlight) "Proposing…" else "Propose resource") }
    }
}
