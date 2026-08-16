package ai.choosh.fleet

import ai.choosh.engine.ConnectionState
import ai.choosh.engine.DevHostPresence
import ai.choosh.ui.WindowWidthSizeClass
import ai.choosh.ui.rememberWindowWidthSizeClass
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.AccessTime
import androidx.compose.material.icons.filled.Dns
import androidx.compose.material.icons.filled.ExpandLess
import androidx.compose.material.icons.filled.ExpandMore
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp

/**
 * The fleet drawer: three switchable sort-mode icons at the top (per
 * docs/specs/android-navigation.md) and the resulting row list underneath.
 * Attention flagging renders as a small red dot on every row type — a
 * property of the row, present in every sort mode, never a fourth mode.
 */
@Composable
fun FleetDrawer(
    state: FleetUiState,
    onSortModeSelected: (SortMode) -> Unit,
    onProjectClick: (Project) -> Unit,
    onDevHostClick: (DevHostPresence) -> Unit,
    onWorkspaceClick: (Workspace) -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxWidth()) {
        SortModeSelector(selected = state.sortMode, onSelected = onSortModeSelected)
        when {
            state.isLoading -> Text(
                "Loading fleet…",
                modifier = Modifier.padding(16.dp),
            )
            state.error != null -> Text(
                "Couldn't load the fleet: ${state.error}",
                modifier = Modifier.padding(16.dp),
                color = MaterialTheme.colorScheme.error,
            )
            state.rows.isEmpty() -> Text(
                "No devhosts enrolled yet.",
                modifier = Modifier.padding(16.dp),
            )
            else -> {
                // Adaptive layout, per `docs/accessibility-device-report.md`'s
                // item 3/4 ("Fleet drawer content occupies a small
                // top-left region of a 1600x2560 window with no adaptive
                // use of the extra width or height"). At Medium/Expanded
                // widths, a multi-column grid uses the real window instead
                // of a single-column list pinned to the left.
                val widthClass = rememberWindowWidthSizeClass()
                if (widthClass == WindowWidthSizeClass.COMPACT) {
                    LazyColumn(modifier = Modifier.testTag("fleet-row-list")) {
                        items(state.rows, key = { it.id }) { row ->
                            FleetRowView(row = row, onProjectClick = onProjectClick, onDevHostClick = onDevHostClick, onWorkspaceClick = onWorkspaceClick)
                        }
                    }
                } else {
                    LazyVerticalGrid(columns = GridCells.Adaptive(minSize = 320.dp), modifier = Modifier.testTag("fleet-row-list")) {
                        items(state.rows, key = { it.id }) { row ->
                            FleetRowView(row = row, onProjectClick = onProjectClick, onDevHostClick = onDevHostClick, onWorkspaceClick = onWorkspaceClick)
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun SortModeSelector(selected: SortMode, onSelected: (SortMode) -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(8.dp),
        horizontalArrangement = Arrangement.SpaceEvenly,
    ) {
        SortModeIcon(
            mode = SortMode.PROJECT,
            selected = selected == SortMode.PROJECT,
            icon = Icons.Filled.Folder,
            description = "Sort by project",
            onSelected = onSelected,
        )
        SortModeIcon(
            mode = SortMode.HOST,
            selected = selected == SortMode.HOST,
            icon = Icons.Filled.Dns,
            description = "Sort by devhost",
            onSelected = onSelected,
        )
        SortModeIcon(
            mode = SortMode.RECENT,
            selected = selected == SortMode.RECENT,
            icon = Icons.Filled.AccessTime,
            description = "Sort by recent activity",
            onSelected = onSelected,
        )
    }
}

@Composable
private fun SortModeIcon(
    mode: SortMode,
    selected: Boolean,
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    description: String,
    onSelected: (SortMode) -> Unit,
) {
    IconButton(
        onClick = { onSelected(mode) },
        modifier = Modifier
            .testTag("sort-mode-${mode.name.lowercase()}")
            .semantics { contentDescription = description },
    ) {
        Icon(
            imageVector = icon,
            contentDescription = null,
            tint = if (selected) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun FleetRowView(
    row: FleetRow,
    onProjectClick: (Project) -> Unit,
    onDevHostClick: (DevHostPresence) -> Unit,
    onWorkspaceClick: (Workspace) -> Unit,
) {
    // Project-mode's real expand-to-workspace-list affordance
    // (docs/specs/android-navigation.md: "Tapping a Project row opens its
    // designated primary Workspace directly, skipping an intermediate
    // Workspace list" — that's [onProjectClick]'s job, unchanged; this is
    // the *separate* chevron affordance the UX-friction audit's finding #3
    // flagged as entirely missing: "Project mode renders one row per
    // Project with no expand affordance"). Local, per-row Compose state —
    // resets if this row scrolls out of the `LazyColumn`/`LazyVerticalGrid`
    // far enough to be discarded and recomposed later; a real
    // ViewModel-level "which projects are expanded" set is a further
    // increment this pass didn't need for the affordance to exist at all.
    var expanded by remember(row.id) { mutableStateOf(false) }

    val (label, sublabel, onClick) = when (row) {
        is FleetRow.ProjectRow -> Triple(
            row.project.name,
            "${row.project.workspaces.size} workspace(s)",
        ) { onProjectClick(row.project) }

        is FleetRow.DevHostRow -> Triple(
            row.devHost.alias,
            "${row.devHost.platform} · ${row.devHost.connectionState.name.lowercase()} · ${row.workspaceCount} workspace(s)",
        ) { onDevHostClick(row.devHost) }

        is FleetRow.WorkspaceRow -> Triple(
            row.workspace.name,
            "on ${row.devHostAlias}",
        ) { onWorkspaceClick(row.workspace) }
    }

    Column(Modifier.fillMaxWidth()) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .clickable(onClick = onClick)
                .testTag("fleet-row-${row.id}")
                // `docs/accessibility-device-report.md`'s item 1, gap 1: "by
                // identical code pattern" to `ExplorerScreen`'s
                // `ChangedFileRow`, this row's clickable node otherwise carries
                // an empty accessible label while the real name/sublabel text
                // sits on non-focusable sibling `Text` children. A real,
                // context-specific label naming the row's actual name and
                // sublabel — confirmed via a real on-device `uiautomator dump`
                // that plain `Modifier.semantics(mergeDescendants = true) {}`
                // did not surface the children's text in the exposed
                // `AccessibilityNodeInfo` on this device/Compose version (see
                // `ExplorerScreen.kt`'s identical finding), so this uses the
                // explicit `contentDescription` form instead — the same
                // mechanism already confirmed working for this file's own
                // `SortModeIcon`/`AttentionDot`.
                .semantics {
                    contentDescription = buildString {
                        append("$label, $sublabel")
                        if (row.needsAttention) append(", needs attention")
                    }
                }
                .padding(horizontal = 16.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Column {
                Text(label, style = MaterialTheme.typography.bodyLarge)
                Text(sublabel, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (row.needsAttention) {
                    AttentionDot()
                }
                if (row is FleetRow.DevHostRow && row.devHost.connectionState == ConnectionState.OFFLINE) {
                    Text("offline", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
                if (row is FleetRow.ProjectRow && row.project.workspaces.isNotEmpty()) {
                    IconButton(
                        onClick = { expanded = !expanded },
                        modifier = Modifier
                            .testTag("project-expand-${row.project.projectId}")
                            .semantics { contentDescription = if (expanded) "Collapse ${row.project.name}" else "Expand ${row.project.name}" },
                    ) {
                        Icon(
                            imageVector = if (expanded) Icons.Filled.ExpandLess else Icons.Filled.ExpandMore,
                            contentDescription = null,
                        )
                    }
                }
            }
        }

        if (row is FleetRow.ProjectRow && expanded) {
            Column(Modifier.testTag("project-workspaces-${row.project.projectId}")) {
                row.project.workspaces.forEach { workspace -> NestedWorkspaceRow(workspace, onClick = { onWorkspaceClick(workspace) }) }
            }
        }
    }
}

/** One indented Workspace row under an expanded [FleetRow.ProjectRow] — the Fleet drawer's Project-mode expand-to-workspace-list affordance (UX-friction audit finding #3). Tapping it opens that Workspace directly, same [Workspace] shape/behavior [FleetRow.WorkspaceRow]'s Recent-mode row already uses. */
@Composable
private fun NestedWorkspaceRow(workspace: Workspace, onClick: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .testTag("project-workspace-row-${workspace.workspaceId}")
            .semantics {
                contentDescription = buildString {
                    append(workspace.name)
                    if (workspace.needsAttention) append(", needs attention")
                }
            }
            .padding(start = 32.dp, end = 16.dp, top = 8.dp, bottom = 8.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(workspace.name, style = MaterialTheme.typography.bodyMedium)
        if (workspace.needsAttention) {
            AttentionDot()
        }
    }
}

/** The per-row attention marker required in every sort mode, per android-navigation.md. */
@Composable
private fun AttentionDot() {
    Box(
        modifier = Modifier
            .testTag("attention-dot")
            .semantics { contentDescription = "Needs attention" }
            .size(10.dp)
            .background(color = Color(0xFFD32F2F), shape = CircleShape),
    )
}
