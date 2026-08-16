package ai.choosh.fleet

import ai.choosh.engine.WorkspaceSummary
import androidx.compose.foundation.clickable
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
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp

/**
 * The real per-devhost Workspace list, per docs/specs/android-navigation.md's
 * "Workspace entry": `Fleet drawer -> Workspace list -> Workspace`. Replaces
 * `ChooshApp.kt`'s old `Screen.DevHostPlaceholder`.
 */
@Composable
fun DevHostWorkspacesScreen(
    deviceId: String,
    state: DevHostWorkspacesUiState,
    onWorkspaceClick: (WorkspaceSummary) -> Unit,
    onRefresh: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(8.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.SpaceBetween) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Button(onClick = onBack) { Text("Back") }
                Text("DevHost $deviceId", style = MaterialTheme.typography.titleMedium, modifier = Modifier.padding(start = 12.dp))
            }
            TextButton(onClick = onRefresh, modifier = Modifier.testTag("devhost-workspaces-refresh-button")) { Text("Refresh") }
        }

        when {
            state.isLoading -> Text("Loading…", modifier = Modifier.padding(16.dp))
            state.error != null -> Text(
                "Couldn't load workspaces: ${state.error}",
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(16.dp).testTag("devhost-workspaces-error"),
            )
            state.workspaces.isEmpty() -> Text(
                "No workspaces registered on this devhost yet.",
                modifier = Modifier.padding(16.dp).testTag("devhost-workspaces-empty"),
            )
            else -> LazyColumn(Modifier.testTag("devhost-workspace-list")) {
                items(state.workspaces, key = { it.workspaceId }) { workspace ->
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .clickable { onWorkspaceClick(workspace) }
                            .testTag("devhost-workspace-row-${workspace.workspaceId}")
                            .semantics { contentDescription = workspace.workspaceName }
                            .padding(horizontal = 16.dp, vertical = 12.dp),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(workspace.workspaceName)
                        Text(workspace.projectId, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
            }
        }
    }
}
