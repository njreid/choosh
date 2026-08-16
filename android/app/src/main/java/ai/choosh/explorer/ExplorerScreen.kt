package ai.choosh.explorer

import ai.choosh.engine.AgentKind
import ai.choosh.engine.AgentRunStatus
import ai.choosh.engine.ChangedPath
import ai.choosh.engine.TreeEntry
import ai.choosh.engine.TreeEntryKind
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.HorizontalDivider
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
 * The explorer's four page-zero sections, per
 * docs/specs/android-navigation.md's Page model — active agents, registered
 * development services, changed files (`workspace.status`), and the
 * searchable project tree (`workspace.tree.list`). One scrollable column
 * (not four independently-scrolling Lazy containers — see this file's own
 * history for why: nesting more than one `LazyColumn` inside a shared
 * scroll parent needs explicit bounded heights per section this pass didn't
 * need to solve given the four sections' typically small row counts).
 * Tapping an agent/service/tree-file row resolves through
 * [ExplorerViewModel]'s real `item.list`/`workspace.tree.list` data to a
 * genuine pinnable Item — never hardcoded demo content.
 */
@Composable
fun ExplorerScreen(
    state: ExplorerUiState,
    onFileClick: (ChangedPath) -> Unit,
    onRefresh: () -> Unit,
    onAgentClick: (AgentRow) -> Unit = {},
    onCreateAgent: (AgentKind, String) -> Unit = { _, _ -> },
    onServiceClick: (ServiceRow) -> Unit = {},
    onCreateService: (name: String, command: List<String>, port: Int) -> Unit = { _, _, _ -> },
    onTreeEntryClick: (TreeEntry) -> Unit = {},
    onTreeSearchResultClick: (String) -> Unit = {},
    onTreeSearchQueryChanged: (String) -> Unit = {},
    onTreeNavigateUp: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    Column(modifier.verticalScroll(rememberScrollState()).testTag("explorer-screen")) {
        state.itemActionError?.let { message ->
            Text(
                "Couldn't create item: $message",
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(16.dp).testTag("explorer-item-action-error"),
            )
        }

        ActiveAgentsSection(state.agents, state.agentsError, onAgentClick, onCreateAgent)
        HorizontalDivider()
        DevServicesSection(state.services, state.servicesError, onServiceClick, onCreateService)
        HorizontalDivider()
        ChangedFilesSection(state, onFileClick, onRefresh)
        HorizontalDivider()
        ProjectTreeSection(state.tree, onTreeEntryClick, onTreeSearchResultClick, onTreeSearchQueryChanged, onTreeNavigateUp)
    }
}

// --- Active agents ---------------------------------------------------------

@Composable
private fun ActiveAgentsSection(agents: List<AgentRow>, error: String?, onAgentClick: (AgentRow) -> Unit, onCreateAgent: (AgentKind, String) -> Unit) {
    Column(Modifier.fillMaxWidth().testTag("agent-section")) {
        SectionHeader("Active agents")
        when {
            error != null -> Text("Couldn't load agents: $error", color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(16.dp).testTag("agent-error"))
            agents.isEmpty() -> Text("No active agents.", modifier = Modifier.padding(horizontal = 16.dp).testTag("agent-empty"))
            else -> Column(Modifier.testTag("agent-list")) {
                agents.forEach { agent -> AgentRowView(agent, onClick = { onAgentClick(agent) }) }
            }
        }
        NewAgentForm(onCreateAgent)
    }
}

@Composable
private fun AgentRowView(agent: AgentRow, onClick: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .testTag("agent-row-${agent.itemId}")
            .semantics { contentDescription = "${agent.name}, ${agent.status.name.lowercase()}" }
            .padding(horizontal = 16.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(agent.name)
        Text(agent.status.name.lowercase(), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
private fun NewAgentForm(onCreateAgent: (AgentKind, String) -> Unit) {
    var name by remember { mutableStateOf("") }
    Column(Modifier.padding(horizontal = 16.dp, vertical = 4.dp)) {
        OutlinedTextField(
            value = name,
            onValueChange = { name = it },
            label = { Text("New agent name") },
            modifier = Modifier.fillMaxWidth().testTag("new-agent-name-field"),
        )
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceEvenly) {
            AgentKind.entries.forEach { kind ->
                TextButton(
                    onClick = { onCreateAgent(kind, name.ifBlank { defaultAgentName(kind) }) },
                    modifier = Modifier.testTag("new-agent-${kind.name.lowercase()}-button"),
                ) { Text(kind.name.lowercase().replaceFirstChar { it.uppercase() }) }
            }
        }
    }
}

private fun defaultAgentName(kind: AgentKind): String = "${kind.name.lowercase()}-${System.currentTimeMillis() % 100000}"

// --- Registered development services ---------------------------------------

@Composable
private fun DevServicesSection(services: List<ServiceRow>, error: String?, onServiceClick: (ServiceRow) -> Unit, onCreateService: (String, List<String>, Int) -> Unit) {
    Column(Modifier.fillMaxWidth().testTag("service-section")) {
        SectionHeader("Registered development services")
        when {
            error != null -> Text("Couldn't load services: $error", color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(16.dp).testTag("service-error"))
            services.isEmpty() -> Text("No registered services.", modifier = Modifier.padding(horizontal = 16.dp).testTag("service-empty"))
            else -> Column(Modifier.testTag("service-list")) {
                services.forEach { service -> ServiceRowView(service, onClick = { onServiceClick(service) }) }
            }
        }
        NewServiceForm(onCreateService)
    }
}

@Composable
private fun ServiceRowView(service: ServiceRow, onClick: () -> Unit) {
    val portLabel = service.port?.let { ":$it" }.orEmpty()
    Row(
        Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .testTag("service-row-${service.itemId}")
            .semantics { contentDescription = "${service.name}$portLabel, ${service.status.name.lowercase()}" }
            .padding(horizontal = 16.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text("${service.name}$portLabel")
        Text(service.status.name.lowercase(), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
private fun NewServiceForm(onCreateService: (String, List<String>, Int) -> Unit) {
    var name by remember { mutableStateOf("") }
    var command by remember { mutableStateOf("") }
    var port by remember { mutableStateOf("") }
    Column(Modifier.padding(horizontal = 16.dp, vertical = 4.dp)) {
        OutlinedTextField(value = name, onValueChange = { name = it }, label = { Text("Service name") }, modifier = Modifier.fillMaxWidth().testTag("new-service-name-field"))
        OutlinedTextField(
            value = command,
            onValueChange = { command = it },
            // host-rpc.md's "Command construction": a fixed argv, never a
            // shell string `hostd` interpolates — this field is split on
            // whitespace client-side into separate argv entries before
            // ever reaching item.create, the caller's own explicit choice
            // of how to tokenize free-form input, not hostd inferring it.
            label = { Text("Command (space-separated argv)") },
            modifier = Modifier.fillMaxWidth().testTag("new-service-command-field"),
        )
        OutlinedTextField(value = port, onValueChange = { port = it }, label = { Text("Port") }, modifier = Modifier.fillMaxWidth().testTag("new-service-port-field"))
        TextButton(
            onClick = {
                val argv = command.trim().split(Regex("\\s+")).filter { it.isNotBlank() }
                onCreateService(name, argv, port.toIntOrNull() ?: 0)
            },
            modifier = Modifier.testTag("new-service-create-button"),
        ) { Text("Create service") }
    }
}

// --- Changed files -----------------------------------------------------------

@Composable
private fun ChangedFilesSection(state: ExplorerUiState, onFileClick: (ChangedPath) -> Unit, onRefresh: () -> Unit) {
    Column(Modifier.fillMaxWidth()) {
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
            else -> Column(Modifier.testTag("changed-file-list")) {
                state.changed.forEach { file -> ChangedFileRow(file, conflicted = file.path in state.conflicted, onClick = { onFileClick(file) }) }
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

// --- Searchable project tree -------------------------------------------------

@Composable
private fun ProjectTreeSection(
    tree: TreeUiState,
    onTreeEntryClick: (TreeEntry) -> Unit,
    onTreeSearchResultClick: (String) -> Unit,
    onTreeSearchQueryChanged: (String) -> Unit,
    onTreeNavigateUp: () -> Unit,
) {
    Column(Modifier.fillMaxWidth().testTag("tree-section")) {
        SectionHeader("Project tree")
        OutlinedTextField(
            value = tree.query,
            onValueChange = onTreeSearchQueryChanged,
            label = { Text("Search files") },
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp).testTag("tree-search-field"),
        )

        val searching = tree.query.isNotBlank()
        if (!searching) {
            Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                if (tree.pathPrefix.isNotEmpty()) {
                    TextButton(onClick = onTreeNavigateUp, modifier = Modifier.testTag("tree-up-button")) { Text("Up") }
                }
                Text(
                    if (tree.pathPrefix.isEmpty()) "/" else "/${tree.pathPrefix}",
                    style = MaterialTheme.typography.labelMedium,
                    modifier = Modifier.padding(start = 8.dp).testTag("tree-breadcrumb"),
                )
            }
        }

        when {
            searching -> if (tree.searchResults.isEmpty()) {
                Text("No matches.", modifier = Modifier.padding(16.dp).testTag("tree-search-empty"))
            } else {
                Column(Modifier.testTag("tree-search-results")) {
                    tree.searchResults.forEach { path ->
                        Text(
                            path,
                            modifier = Modifier
                                .fillMaxWidth()
                                .clickable { onTreeSearchResultClick(path) }
                                .testTag("tree-search-result-$path")
                                .padding(horizontal = 16.dp, vertical = 12.dp),
                        )
                    }
                }
            }
            tree.isLoading -> Text("Loading…", modifier = Modifier.padding(16.dp).testTag("tree-loading"))
            tree.error != null -> Text("Couldn't load file tree: ${tree.error}", color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(16.dp).testTag("tree-error"))
            tree.entries.isEmpty() -> Text("This directory is empty.", modifier = Modifier.padding(16.dp).testTag("tree-empty"))
            else -> Column(Modifier.testTag("tree-entry-list")) {
                tree.entries.forEach { entry -> TreeEntryRow(entry, onClick = { onTreeEntryClick(entry) }) }
            }
        }
    }
}

@Composable
private fun TreeEntryRow(entry: TreeEntry, onClick: () -> Unit) {
    val icon = if (entry.kind == TreeEntryKind.DIRECTORY) "📁" else "📄"
    Row(
        Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .testTag("tree-entry-${entry.name}")
            .semantics { contentDescription = "$icon ${entry.name}${if (entry.conflicted) ", conflicted" else ""}" }
            .padding(horizontal = 16.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text("$icon ${entry.name}", color = if (entry.conflicted) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface)
    }
}

@Composable
private fun SectionHeader(title: String) {
    Row(Modifier.fillMaxWidth().padding(8.dp), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
        Text(title, style = MaterialTheme.typography.titleMedium)
    }
}
