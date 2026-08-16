package ai.choosh.explorer

import ai.choosh.agentevents.AgentStatusTracker
import ai.choosh.engine.AgentKind
import ai.choosh.engine.AgentRunStatus
import ai.choosh.engine.ChangedPath
import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.CreateItemResult
import ai.choosh.engine.ItemType
import ai.choosh.engine.TreeEntry
import ai.choosh.engine.TreeEntryKind
import ai.choosh.engine.WebServiceStatus
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/** One `AgentTerminal` row in the explorer's "active agents" section. [status] is the live `agent_status` value when known (see [AgentStatusTracker]), else a coarse fallback derived from `item.list`'s own running/stopped signal. */
data class AgentRow(val itemId: String, val name: String, val status: AgentRunStatus)

/** One `WebService` row in the explorer's "registered development services" section. */
data class ServiceRow(val itemId: String, val name: String, val port: Int?, val status: WebServiceStatus)

/**
 * One directory level of the explorer's searchable project tree
 * (`workspace.tree.list`, docs/specs/android-navigation.md's Page model
 * section 4). [pathPrefix] empty means the workspace root. [query]/
 * [searchResults] are [ExplorerViewModel]'s client-side filter over every
 * directory level fetched so far (android-navigation.md's "Search" section:
 * "Explorer search matches cached/streamed root-confined paths ...
 * Host-assisted content search is a later capability") — [searchResults]
 * are full root-relative paths, not bare names.
 */
data class TreeUiState(
    val pathPrefix: String = "",
    val entries: List<TreeEntry> = emptyList(),
    val isLoading: Boolean = false,
    val error: String? = null,
    val query: String = "",
    val searchResults: List<String> = emptyList(),
)

data class ExplorerUiState(
    val changed: List<ChangedPath> = emptyList(),
    val conflicted: List<String> = emptyList(),
    val isLoading: Boolean = true,
    val error: String? = null,
    val agents: List<AgentRow> = emptyList(),
    val agentsError: String? = null,
    val services: List<ServiceRow> = emptyList(),
    val servicesError: String? = null,
    val tree: TreeUiState = TreeUiState(),
    /** Set by [ExplorerViewModel.createAgent]/[ExplorerViewModel.createService] on a `hostd`-side rejection — surfaced distinctly from [agentsError]/[servicesError], which are read-path (`item.list`) failures, not write-path (`item.create`) ones. */
    val itemActionError: String? = null,
)

/**
 * Fired once per tapped Explorer row that names a real pinnable Item or
 * file, per docs/specs/android-navigation.md's "Tapping a row toggles its
 * pinned state" — the composition root turns this into a navigation to the
 * corresponding pinned-item screen (`AgentTerminal`->Terminal,
 * `WebService`->WebService, a tree file->SourceEditor or MarkdownPreview
 * depending on extension, per that doc's "Pinned kinds" table). Mirrors
 * [ai.choosh.fleet.FleetNavigationEvent]'s existing "ViewModel returns an
 * event, composition root maps it to a screen" shape.
 */
sealed interface ExplorerNavigationEvent {
    data class OpenTerminal(val deviceId: String, val itemId: String) : ExplorerNavigationEvent
    data class OpenWebService(val deviceId: String, val workspaceId: String, val itemId: String) : ExplorerNavigationEvent
    data class OpenSourceEditor(val deviceId: String, val workspaceId: String, val path: String) : ExplorerNavigationEvent
    data class OpenMarkdownPreview(val deviceId: String, val workspaceId: String, val path: String) : ExplorerNavigationEvent
}

/**
 * Backs every one of the explorer's four page-zero sections
 * (docs/specs/android-navigation.md's "Page model": active agents,
 * registered development services, changed files, searchable project
 * tree) plus real item-pinning:
 *
 *  - **Active agents** / **registered development services**: `item.list`,
 *    split by [ai.choosh.engine.ItemType]. A tapped row resolves to
 *    [ExplorerNavigationEvent.OpenTerminal]/[ExplorerNavigationEvent.OpenWebService]
 *    against that item's real `itemId` — never a hardcoded demo item.
 *    [createAgent]/[createService] call `item.create` for the
 *    "start a new one" affordance (there is no `item.create`-less way to
 *    reach a workspace's first agent/service — per host-rpc.md, an
 *    `AgentTerminal`/`WebService` item only exists once `item.create`
 *    registers it).
 *  - **Changed files**: `workspace.status`, unchanged from the pre-existing
 *    single-section implementation.
 *  - **Searchable project tree**: `workspace.tree.list`, one directory
 *    level per [loadTree] call (host-rpc.md's Bounds: "Directory traversal
 *    depth per tree.list call: one level; recursion is client-driven").
 *    [onTreeSearchQueryChanged] filters over every level fetched so far,
 *    not a fresh recursive fetch, per android-navigation.md's "Search"
 *    section. A tapped file resolves to
 *    [ExplorerNavigationEvent.OpenSourceEditor] or
 *    [ExplorerNavigationEvent.OpenMarkdownPreview] by extension (`.md` ->
 *    Markdown, per the "Pinned kinds" table's `MarkdownPreview`/
 *    `SourceEditor` split) — a tapped directory drills into it instead via
 *    [loadTree], never a navigation event.
 *
 * [statusTracker], like [ai.choosh.fleet.FleetViewModel]'s
 * `attentionTracker` parameter, is `null` by default (every pre-existing
 * call site/test keeps working unchanged) — when supplied, it's the only
 * source of an `AgentTerminal` row's real `busy`/`waiting`/`starting`/
 * `failed` distinction, since `item.list` itself only ever reports
 * `running`/`stopped` for that item type (see [AgentStatusTracker]'s own
 * doc comment).
 */
class ExplorerViewModel(
    private val engine: ChooshEngine,
    private val deviceId: String,
    private val workspaceId: String,
    private val statusTracker: AgentStatusTracker? = null,
) : ViewModel() {
    private val _state = MutableStateFlow(ExplorerUiState())
    val state: StateFlow<ExplorerUiState> = _state.asStateFlow()

    /** `pathPrefix -> entries` for every directory level [loadTree] has fetched, oldest levels never evicted — the cache [onTreeSearchQueryChanged] filters over. */
    private val treeCache = mutableMapOf<String, List<TreeEntry>>()

    init {
        refresh()
        refreshItems()
        loadTree("")
        statusTracker?.let { tracker ->
            viewModelScope.launch { tracker.statusByItemId.collect { recomputeAgentStatuses(it) } }
        }
    }

    fun refresh() {
        viewModelScope.launch {
            _state.value = _state.value.copy(isLoading = true, error = null)
            runCatching { engine.workspaceStatus(deviceId, workspaceId) }
                .onSuccess { status ->
                    _state.value = _state.value.copy(changed = status.changed, conflicted = status.conflicted, isLoading = false, error = null)
                }
                .onFailure { failure ->
                    _state.value = _state.value.copy(isLoading = false, error = failure.message ?: "failed to load workspace status")
                }
        }
    }

    /** `item.list`, split into [ExplorerUiState.agents]/[ExplorerUiState.services] — the explorer's other two RPC-backed sections. Public (not folded into [refresh]) so [ExplorerScreen]'s own refresh action and [createAgent]/[createService] can re-poll it independently of the changed-files section's cadence. */
    fun refreshItems() {
        viewModelScope.launch {
            runCatching { engine.itemList(deviceId, workspaceId) }
                .onSuccess { items ->
                    val live = statusTracker?.statusByItemId?.value.orEmpty()
                    val agents = items.filter { it.itemType == ItemType.AGENT_TERMINAL }.map { AgentRow(it.itemId, it.name, effectiveAgentStatus(it.status, live[it.itemId])) }
                    val services = items.filter { it.itemType == ItemType.WEB_SERVICE }.map { ServiceRow(it.itemId, it.name, it.port, it.status) }
                    _state.value = _state.value.copy(agents = agents, services = services, agentsError = null, servicesError = null)
                }
                .onFailure { failure ->
                    val message = failure.message ?: "failed to load items"
                    _state.value = _state.value.copy(agentsError = message, servicesError = message)
                }
        }
    }

    /** `item.create { item_type: AgentTerminal, agent }`, then [refreshItems] on success so the new row appears without a separate manual refresh. */
    fun createAgent(agent: AgentKind, name: String) {
        viewModelScope.launch {
            _state.value = _state.value.copy(itemActionError = null)
            when (val result = createItemOrFailure { engine.createItem(deviceId, workspaceId, ItemType.AGENT_TERMINAL, name, agent = agent) }) {
                is CreateItemResult.Success -> refreshItems()
                is CreateItemResult.Failure -> _state.value = _state.value.copy(itemActionError = result.message)
            }
        }
    }

    /** `item.create { item_type: WebService, command, port }` — `command` MUST already be a fixed argv (host-rpc.md's "Command construction"; see [ChooshEngine.createItem]'s doc comment), never a raw shell string this method would split itself. Then [refreshItems] on success, mirroring [createAgent]. */
    fun createService(name: String, command: List<String>, port: Int) {
        viewModelScope.launch {
            _state.value = _state.value.copy(itemActionError = null)
            when (val result = createItemOrFailure { engine.createItem(deviceId, workspaceId, ItemType.WEB_SERVICE, name, command = command, port = port) }) {
                is CreateItemResult.Success -> refreshItems()
                is CreateItemResult.Failure -> _state.value = _state.value.copy(itemActionError = result.message)
            }
        }
    }

    private suspend fun createItemOrFailure(call: suspend () -> CreateItemResult): CreateItemResult =
        runCatching { call() }.getOrElse { failure -> CreateItemResult.Failure(failure.message ?: "item.create failed") }

    /** Resolves a tapped [AgentRow] to the real [ExplorerNavigationEvent] that opens its `AgentTerminal` — never a hardcoded item id. */
    fun onAgentTapped(row: AgentRow): ExplorerNavigationEvent.OpenTerminal = ExplorerNavigationEvent.OpenTerminal(deviceId, row.itemId)

    /** Resolves a tapped [ServiceRow] to the real [ExplorerNavigationEvent] that opens its `WebService`. */
    fun onServiceTapped(row: ServiceRow): ExplorerNavigationEvent.OpenWebService = ExplorerNavigationEvent.OpenWebService(deviceId, workspaceId, row.itemId)

    /**
     * A tapped tree row: a directory drills [loadTree] into it (no
     * navigation event, `null`); a file resolves to
     * [ExplorerNavigationEvent.OpenMarkdownPreview] (a `.md`/`.markdown`
     * path, case-insensitive) or [ExplorerNavigationEvent.OpenSourceEditor]
     * otherwise, per android-navigation.md's "Pinned kinds" table.
     */
    fun onTreeEntryTapped(entry: TreeEntry): ExplorerNavigationEvent? {
        val fullPath = joinTreePath(_state.value.tree.pathPrefix, entry.name)
        return when (entry.kind) {
            TreeEntryKind.DIRECTORY -> {
                loadTree(fullPath)
                null
            }
            TreeEntryKind.FILE, TreeEntryKind.UNKNOWN -> if (isMarkdownPath(fullPath)) {
                ExplorerNavigationEvent.OpenMarkdownPreview(deviceId, workspaceId, fullPath)
            } else {
                ExplorerNavigationEvent.OpenSourceEditor(deviceId, workspaceId, fullPath)
            }
        }
    }

    /** Same file/directory resolution as [onTreeEntryTapped], for a row surfaced via [TreeUiState.searchResults] (a bare path string, not a [TreeEntry] — a search result crosses directory levels this ViewModel hasn't necessarily loaded [TreeEntry.kind] for at the *current* level, but every result string is itself the full path of a previously-fetched entry, so its kind was already known at fetch time; this app's V1 search only ever surfaces files, never directories, keeping this resolution unambiguous). */
    fun onTreeSearchResultTapped(path: String): ExplorerNavigationEvent =
        if (isMarkdownPath(path)) ExplorerNavigationEvent.OpenMarkdownPreview(deviceId, workspaceId, path) else ExplorerNavigationEvent.OpenSourceEditor(deviceId, workspaceId, path)

    /** Fetches one directory level (`pathPrefix` empty means the workspace root) and caches it for [onTreeSearchQueryChanged]. Public so [ExplorerScreen]'s breadcrumb/refresh affordances can call it directly. */
    fun loadTree(pathPrefix: String) {
        viewModelScope.launch {
            _state.value = _state.value.copy(tree = _state.value.tree.copy(pathPrefix = pathPrefix, isLoading = true, error = null))
            runCatching { engine.workspaceTreeList(deviceId, workspaceId, pathPrefix) }
                .onSuccess { result ->
                    treeCache[pathPrefix] = result.entries
                    _state.value = _state.value.copy(tree = _state.value.tree.copy(entries = result.entries, isLoading = false, error = null))
                }
                .onFailure { failure ->
                    _state.value = _state.value.copy(tree = _state.value.tree.copy(isLoading = false, error = failure.message ?: "failed to load file tree"))
                }
        }
    }

    /** Drills back up to the current directory's parent (root's parent is itself a no-op — there's nowhere above the workspace root). */
    fun onTreeNavigateUp() {
        val current = _state.value.tree.pathPrefix
        if (current.isEmpty()) return
        loadTree(current.substringBeforeLast('/', missingDelimiterValue = ""))
    }

    /** Filters [treeCache] (every directory level fetched so far) by substring match on each entry's own name, case-insensitively — per android-navigation.md's "Search" section. Blank `query` clears the results without touching the cache. */
    fun onTreeSearchQueryChanged(query: String) {
        val results = if (query.isBlank()) {
            emptyList()
        } else {
            treeCache.flatMap { (prefix, entries) ->
                entries.filter { it.kind == TreeEntryKind.FILE && it.name.contains(query, ignoreCase = true) }.map { joinTreePath(prefix, it.name) }
            }.sorted()
        }
        _state.value = _state.value.copy(tree = _state.value.tree.copy(query = query, searchResults = results))
    }

    private fun recomputeAgentStatuses(live: Map<String, AgentRunStatus>) {
        val current = _state.value.agents
        if (current.isEmpty()) return
        _state.value = _state.value.copy(agents = current.map { row -> live[row.itemId]?.let { row.copy(status = it) } ?: row })
    }
}

/** `live` (from [AgentStatusTracker], when known) wins outright — it's a genuine `agent_status` push, strictly more informative than `item.list`'s own coarse `running`/`stopped`. Absent that, `item.list`'s `stopped` maps to [AgentRunStatus.STOPPED] (the one value both shapes agree on); `running` maps to [AgentRunStatus.UNKNOWN] rather than a guessed busy/waiting, since (per [AgentStatusTracker]'s doc comment) `item.list` genuinely cannot distinguish those for an `AgentTerminal`. */
private fun effectiveAgentStatus(itemStatus: WebServiceStatus, live: AgentRunStatus?): AgentRunStatus {
    if (live != null) return live
    return if (itemStatus == WebServiceStatus.STOPPED) AgentRunStatus.STOPPED else AgentRunStatus.UNKNOWN
}

private fun isMarkdownPath(path: String): Boolean {
    val lower = path.lowercase()
    return lower.endsWith(".md") || lower.endsWith(".markdown")
}

private fun joinTreePath(pathPrefix: String, name: String): String = if (pathPrefix.isEmpty()) name else "$pathPrefix/$name"
