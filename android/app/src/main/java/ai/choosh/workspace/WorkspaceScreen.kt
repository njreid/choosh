package ai.choosh.workspace

import ai.choosh.engine.ChooshEngine
import ai.choosh.explorer.ExplorerScreen
import ai.choosh.explorer.ExplorerViewModel
import ai.choosh.jj.JjChangeGraphScreen
import ai.choosh.jj.JjChangeGraphViewModel
import ai.choosh.jj.JjDiffScreen
import ai.choosh.jj.JjDiffViewModel
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Tab
import androidx.compose.material3.TabRow
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewmodel.compose.viewModel

/**
 * A Workspace's page model (docs/specs/android-navigation.md): "the
 * explorer is permanently page zero", plus the `JjDiff`/`JjChangeGraph`
 * pinned items this milestone adds. Full pin persistence/reordering across
 * app restarts is a later increment (per android-navigation.md's "Pin
 * order is insertion order in V1. Reordering MAY be added later.") — this
 * pass renders exactly the three fixed pages as tabs, which already
 * satisfies "Explorer is page zero" plus both new item kinds being
 * reachable.
 */
private enum class WorkspaceTab { EXPLORER, DIFF, GRAPH }

@Composable
fun WorkspaceScreen(
    engine: ChooshEngine,
    workspaceId: String,
    deviceId: String,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
    // Both default to `null` (and render no button) rather than a no-op
    // lambda: `ChooshApp.kt`'s tests/previews that don't wire real
    // `Terminal`/`SourceEditor` navigation shouldn't show a dead button.
    // Demo affordances until the explorer surfaces real `AgentTerminal`/
    // `SourceEditor` pinned items to tap directly (a later increment, per
    // docs/specs/android-navigation.md).
    onOpenTerminal: (() -> Unit)? = null,
    onOpenEditor: ((path: String) -> Unit)? = null,
    // Same "demo affordance, null renders no button" convention as
    // onOpenTerminal/onOpenEditor above — the explorer doesn't yet surface
    // real WebService pinned items or Markdown files to tap directly, per
    // docs/specs/service-tunnels.md/M5-web-and-markdown.md.
    onOpenWebServiceDemo: (() -> Unit)? = null,
    onOpenMarkdownDemo: (() -> Unit)? = null,
) {
    var tab by remember { mutableIntStateOf(0) }

    val explorerViewModel: ExplorerViewModel = viewModel(
        factory = singleInstanceFactory { ExplorerViewModel(engine, deviceId, workspaceId) },
    )
    val diffViewModel: JjDiffViewModel = viewModel(
        factory = singleInstanceFactory { JjDiffViewModel(engine, deviceId, workspaceId) },
    )
    val graphViewModel: JjChangeGraphViewModel = viewModel(
        factory = singleInstanceFactory { JjChangeGraphViewModel(engine, deviceId, workspaceId) },
    )

    Column(modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(8.dp), verticalAlignment = Alignment.CenterVertically) {
            Button(onClick = onBack) { Text("Back") }
            Text(
                "Workspace $workspaceId",
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.padding(start = 12.dp),
            )
        }
        if (onOpenTerminal != null || onOpenEditor != null || onOpenWebServiceDemo != null || onOpenMarkdownDemo != null) {
            Row(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 8.dp)) {
                if (onOpenTerminal != null) {
                    Button(onClick = onOpenTerminal) { Text("Open terminal") }
                }
                if (onOpenEditor != null) {
                    Button(onClick = { onOpenEditor("README.md") }, modifier = Modifier.padding(start = 8.dp)) {
                        Text("Open README.md in editor")
                    }
                }
                if (onOpenWebServiceDemo != null) {
                    Button(onClick = onOpenWebServiceDemo, modifier = Modifier.padding(start = 8.dp)) {
                        Text("Open WebService demo")
                    }
                }
                if (onOpenMarkdownDemo != null) {
                    Button(onClick = onOpenMarkdownDemo, modifier = Modifier.padding(start = 8.dp)) {
                        Text("Open Markdown demo")
                    }
                }
            }
        }
        TabRow(selectedTabIndex = tab) {
            WorkspaceTab.entries.forEachIndexed { index, entry ->
                Tab(
                    selected = tab == index,
                    onClick = { tab = index },
                    text = { Text(entry.name.lowercase().replaceFirstChar { it.uppercase() }) },
                    modifier = Modifier.testTag("workspace-tab-${entry.name.lowercase()}"),
                )
            }
        }

        when (WorkspaceTab.entries[tab]) {
            WorkspaceTab.EXPLORER -> {
                val state by explorerViewModel.state.collectAsState()
                ExplorerScreen(
                    state = state,
                    onFileClick = { tab = WorkspaceTab.DIFF.ordinal },
                    onRefresh = explorerViewModel::refresh,
                    modifier = Modifier.weight(1f),
                )
            }

            WorkspaceTab.DIFF -> {
                val state by diffViewModel.state.collectAsState()
                JjDiffScreen(
                    state = state,
                    onFromChange = diffViewModel::setFrom,
                    onToChange = diffViewModel::setTo,
                    onLoad = diffViewModel::load,
                    modifier = Modifier.weight(1f),
                )
            }

            WorkspaceTab.GRAPH -> {
                val state by graphViewModel.state.collectAsState()
                JjChangeGraphScreen(
                    state = state,
                    onNodeTap = graphViewModel::selectChange,
                    onDismissSelection = graphViewModel::dismissSelection,
                    onUndoMostRecent = graphViewModel::undoMostRecentOperation,
                    onRestore = graphViewModel::restore,
                    modifier = Modifier.weight(1f),
                )
            }
        }
    }
}

/** A minimal single-instance [ViewModelProvider.Factory] — no DI framework, per AGENTS.md. */
private fun <T : ViewModel> singleInstanceFactory(build: () -> T): ViewModelProvider.Factory =
    object : ViewModelProvider.Factory {
        @Suppress("UNCHECKED_CAST")
        override fun <VM : ViewModel> create(modelClass: Class<VM>): VM = build() as VM
    }
