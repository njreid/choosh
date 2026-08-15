package ai.choosh.explorer

import ai.choosh.engine.ChangedPath
import ai.choosh.ui.WindowWidthSizeClass
import ai.choosh.ui.rememberWindowWidthSizeClass
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.lazy.items
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
 * The explorer's changed-files section, wired to `workspace.status` per
 * docs/specs/jj-integration.md — the minimal explorer surface this pass
 * needs (per android-navigation.md's page model, the other three sections
 * — active agents, dev services, the searchable project tree — are a
 * different milestone's scope). Tapping a changed file navigates to
 * `JjDiff` (there is no per-file diff RPC — `workspace.diff` already
 * returns every changed file's hunks in one call) via [onFileClick].
 */
@Composable
fun ExplorerScreen(
    state: ExplorerUiState,
    onFileClick: (ChangedPath) -> Unit,
    onRefresh: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier) {
        Row(
            Modifier.fillMaxWidth().padding(8.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("Changed files", style = MaterialTheme.typography.titleMedium)
            TextButton(onClick = onRefresh, modifier = Modifier.testTag("explorer-refresh-button")) { Text("Refresh") }
        }

        when {
            state.isLoading -> Text("Loading…", modifier = Modifier.padding(16.dp))
            state.error != null -> Text(
                "Couldn't load workspace status: ${state.error}",
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(16.dp).testTag("explorer-error"),
            )
            state.changed.isEmpty() -> Text("No changed files.", modifier = Modifier.padding(16.dp).testTag("explorer-empty"))
            else -> {
                // Adaptive layout, per `docs/accessibility-device-report.md`'s
                // item 3/4 ("no `WindowSizeClass`-driven layout choice
                // anywhere in the codebase... no master-detail split, no
                // multi-column grid"): at Medium/Expanded widths (tablet,
                // DeX, external display), a single-column list wastes most
                // of the window — a multi-column grid uses the real extra
                // width instead of stretching the phone layout.
                val widthClass = rememberWindowWidthSizeClass()
                if (widthClass == WindowWidthSizeClass.COMPACT) {
                    LazyColumn(Modifier.testTag("changed-file-list")) {
                        items(state.changed, key = { it.path }) { file ->
                            ChangedFileRow(file, conflicted = file.path in state.conflicted, onClick = { onFileClick(file) })
                        }
                    }
                } else {
                    LazyVerticalGrid(columns = GridCells.Adaptive(minSize = 320.dp), modifier = Modifier.testTag("changed-file-list")) {
                        items(state.changed, key = { it.path }) { file ->
                            ChangedFileRow(file, conflicted = file.path in state.conflicted, onClick = { onFileClick(file) })
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun ChangedFileRow(file: ChangedPath, conflicted: Boolean, onClick: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .testTag("changed-file-${file.path}")
            // `docs/accessibility-device-report.md`'s item 1, gap 1: this
            // row's own clickable node otherwise carries an empty
            // accessible label, with the real filename/kind text living on
            // separate, non-focusable sibling `Text` children TalkBack
            // would never announce for it. A real, context-specific label
            // naming the actual filename and change kind — confirmed via a
            // real on-device `uiautomator dump` (`Modifier.semantics(
            // mergeDescendants = true) {}` alone did not surface the
            // children's text in the exposed `AccessibilityNodeInfo` on
            // this device/Compose version, so this uses the explicit
            // `contentDescription` form instead, the same mechanism
            // already confirmed working for `FleetDrawer.kt`'s
            // `SortModeIcon`/`AttentionDot`).
            .semantics { contentDescription = "${file.path}, ${if (conflicted) "conflicted" else file.kind.name.lowercase()}" }
            .padding(horizontal = 16.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(file.path, color = if (conflicted) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface)
        Text(
            if (conflicted) "conflicted" else file.kind.name.lowercase(),
            style = MaterialTheme.typography.labelSmall,
            color = if (conflicted) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = if (conflicted) Modifier.testTag("conflicted-marker-${file.path}") else Modifier,
        )
    }
}
