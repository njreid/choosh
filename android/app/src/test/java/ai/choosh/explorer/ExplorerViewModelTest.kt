package ai.choosh.explorer

import ai.choosh.agentevents.AgentStatusTracker
import ai.choosh.engine.AgentEvent
import ai.choosh.engine.AgentKind
import ai.choosh.engine.AgentRunStatus
import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.CreateItemResult
import ai.choosh.engine.FakeChooshEngine
import ai.choosh.engine.ItemSummary
import ai.choosh.engine.ItemType
import ai.choosh.engine.WebServiceStatus
import ai.choosh.engine.WorkspaceStatus
import ai.choosh.engine.WorkspaceTreeListResult
import ai.choosh.fleet.MainDispatcherRule
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/** [ChooshEngine] wrapper that always fails `workspaceStatus`, delegating everything else to [FakeChooshEngine] — lets a test exercise [ExplorerViewModel.refresh]'s `onFailure` path without a real transport failure. */
private class FailingWorkspaceStatusChooshEngine(private val delegate: ChooshEngine = FakeChooshEngine()) : ChooshEngine by delegate {
    override suspend fun workspaceStatus(deviceId: String, workspaceId: String): WorkspaceStatus = error("simulated workspace.status failure")
}

/** [ChooshEngine] wrapper that always fails `item.list`, delegating everything else to [FakeChooshEngine] — exercises [ExplorerViewModel.refreshItems]'s `onFailure` path. */
private class FailingItemListChooshEngine(private val delegate: ChooshEngine = FakeChooshEngine()) : ChooshEngine by delegate {
    override suspend fun itemList(deviceId: String, workspaceId: String): List<ItemSummary> = error("simulated item.list failure")
}

/** [ChooshEngine] wrapper that always returns an empty `item.list`, delegating everything else to [FakeChooshEngine] — exercises the explorer's empty active-agents/dev-services states. */
private class EmptyItemListChooshEngine(private val delegate: ChooshEngine = FakeChooshEngine()) : ChooshEngine by delegate {
    override suspend fun itemList(deviceId: String, workspaceId: String): List<ItemSummary> = emptyList()
}

/** [ChooshEngine] wrapper that always fails `workspace.tree.list`, delegating everything else to [FakeChooshEngine] — exercises [ExplorerViewModel.loadTree]'s `onFailure` path. */
private class FailingTreeListChooshEngine(private val delegate: ChooshEngine = FakeChooshEngine()) : ChooshEngine by delegate {
    override suspend fun workspaceTreeList(deviceId: String, workspaceId: String, pathPrefix: String, cursor: String?): WorkspaceTreeListResult =
        error("simulated workspace.tree.list failure")
}

@OptIn(ExperimentalCoroutinesApi::class)
class ExplorerViewModelTest {

    @get:Rule
    val mainDispatcherRule = MainDispatcherRule()

    @Test
    fun `refresh loads the changed-files summary including a conflicted path`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(!state.isLoading)
        assertNull(state.error)
        assertTrue(state.changed.isNotEmpty())
        assertEquals(1, state.conflicted.size)
        assertTrue(state.changed.any { it.path == state.conflicted.first() })
    }

    @Test
    fun `a workspace status failure surfaces as an error state, not a crash`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FailingWorkspaceStatusChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(!state.isLoading)
        assertTrue(state.error?.contains("simulated workspace.status failure") == true)
        assertTrue("no stale changed-files data should be shown alongside an error", state.changed.isEmpty())
    }

    // --- active agents / registered development services (item.list) ------

    @Test
    fun `refreshItems loads active agents and dev services split by item type, excluding Shell`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()

        val state = viewModel.state.value
        assertNull(state.agentsError)
        assertNull(state.servicesError)
        assertEquals(listOf("agent-a"), state.agents.map { it.name })
        assertEquals(listOf("web"), state.services.map { it.name })
    }

    @Test
    fun `an empty item_list is a clean empty state for both sections, not an error`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(EmptyItemListChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue(state.agents.isEmpty())
        assertTrue(state.services.isEmpty())
        assertNull(state.agentsError)
        assertNull(state.servicesError)
    }

    @Test
    fun `an item_list failure surfaces as an error state for both sections, not a crash`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FailingItemListChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()

        val state = viewModel.state.value
        assertNotNull(state.agentsError)
        assertNotNull(state.servicesError)
        assertTrue(state.agents.isEmpty())
        assertTrue(state.services.isEmpty())
    }

    @Test
    fun `onAgentTapped resolves to a real OpenTerminal event for that item's id`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()
        val agent = viewModel.state.value.agents.single()

        assertEquals(ExplorerNavigationEvent.OpenTerminal("dev-1", agent.itemId), viewModel.onAgentTapped(agent))
    }

    @Test
    fun `onServiceTapped resolves to a real OpenWebService event for that item's id`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()
        val service = viewModel.state.value.services.single()

        assertEquals(ExplorerNavigationEvent.OpenWebService("dev-1", "ws-1", service.itemId), viewModel.onServiceTapped(service))
    }

    @Test
    fun `createAgent registers a new item and it appears after the automatic refresh`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()
        assertEquals(1, viewModel.state.value.agents.size)

        viewModel.createAgent(AgentKind.CLAUDE, "agent-new")
        advanceUntilIdle()

        val state = viewModel.state.value
        assertNull(state.itemActionError)
        assertEquals(2, state.agents.size)
        assertTrue(state.agents.any { it.name == "agent-new" })
    }

    @Test
    fun `createAgent with a duplicate name is rejected, not silently applied`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()

        viewModel.createAgent(AgentKind.CLAUDE, "agent-a") // "agent-a" already exists (the fixture item)
        advanceUntilIdle()

        val state = viewModel.state.value
        assertNotNull("a duplicate item name must surface an error, not silently apply", state.itemActionError)
        assertEquals("a rejected create must not add a phantom row", 1, state.agents.size)
    }

    @Test
    fun `createService registers a new WebService item and it appears after the automatic refresh`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()
        assertEquals(1, viewModel.state.value.services.size)

        viewModel.createService("web-2", listOf("npm", "run", "dev"), 4000)
        advanceUntilIdle()

        val state = viewModel.state.value
        assertNull(state.itemActionError)
        assertEquals(2, state.services.size)
        assertTrue(state.services.any { it.name == "web-2" && it.port == 4000 })
    }

    @Test
    fun `createService missing a command or port is rejected by the fake, mirroring hostd's invalid_argument`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()

        viewModel.createService("no-command", emptyList(), 0)
        advanceUntilIdle()

        assertNotNull(viewModel.state.value.itemActionError)
    }

    @Test
    fun `a live agent status from the status tracker wins over item_list's own coarse status`() = runTest(mainDispatcherRule.dispatcher) {
        val statusTracker = AgentStatusTracker()
        val viewModel = ExplorerViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1", statusTracker = statusTracker)
        advanceUntilIdle()
        // item.list alone can't distinguish an AgentTerminal's real state (see AgentStatusTracker's
        // doc comment) — the fake's fixture item is always wire-status RUNNING, which maps to UNKNOWN.
        assertEquals(AgentRunStatus.UNKNOWN, viewModel.state.value.agents.single().status)

        statusTracker.apply(AgentEvent.AgentStatusChanged("ws-1", "item-agent-1", AgentRunStatus.BUSY))
        advanceUntilIdle()

        assertEquals(AgentRunStatus.BUSY, viewModel.state.value.agents.single().status)
    }

    // --- searchable project tree (workspace.tree.list) ----------------------

    @Test
    fun `the project tree loads the workspace root on init`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()

        val tree = viewModel.state.value.tree
        assertNull(tree.error)
        assertEquals("", tree.pathPrefix)
        assertEquals(setOf("README.md", "app.kt", "docs"), tree.entries.map { it.name }.toSet())
    }

    @Test
    fun `tapping a directory entry drills into it rather than emitting a navigation event`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()
        val docsEntry = viewModel.state.value.tree.entries.single { it.name == "docs" }

        val event = viewModel.onTreeEntryTapped(docsEntry)
        advanceUntilIdle()

        assertNull("a directory tap must not itself navigate", event)
        val tree = viewModel.state.value.tree
        assertEquals("docs", tree.pathPrefix)
        assertEquals(listOf("guide.md"), tree.entries.map { it.name })
    }

    @Test
    fun `tapping a markdown file resolves to OpenMarkdownPreview, a non-markdown file to OpenSourceEditor`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()
        val appKt = viewModel.state.value.tree.entries.single { it.name == "app.kt" }
        val readmeMd = viewModel.state.value.tree.entries.single { it.name == "README.md" }

        assertEquals(ExplorerNavigationEvent.OpenSourceEditor("dev-1", "ws-1", "app.kt"), viewModel.onTreeEntryTapped(appKt))
        assertEquals(ExplorerNavigationEvent.OpenMarkdownPreview("dev-1", "ws-1", "README.md"), viewModel.onTreeEntryTapped(readmeMd))
    }

    @Test
    fun `onTreeNavigateUp returns to the parent directory`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()
        viewModel.loadTree("docs")
        advanceUntilIdle()
        assertEquals("docs", viewModel.state.value.tree.pathPrefix)

        viewModel.onTreeNavigateUp()
        advanceUntilIdle()

        assertEquals("", viewModel.state.value.tree.pathPrefix)
    }

    @Test
    fun `onTreeNavigateUp at the root is a no-op`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()

        viewModel.onTreeNavigateUp()
        advanceUntilIdle()

        assertEquals("", viewModel.state.value.tree.pathPrefix)
    }

    @Test
    fun `search matches files across every directory level fetched so far`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()
        viewModel.loadTree("docs") // populate the cache for docs/guide.md
        advanceUntilIdle()

        viewModel.onTreeSearchQueryChanged("guide")

        assertEquals(listOf("docs/guide.md"), viewModel.state.value.tree.searchResults)
    }

    @Test
    fun `a blank search query clears results without touching the cache`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()
        viewModel.onTreeSearchQueryChanged("readme")
        assertTrue(viewModel.state.value.tree.searchResults.isNotEmpty())

        viewModel.onTreeSearchQueryChanged("")

        assertTrue(viewModel.state.value.tree.searchResults.isEmpty())
    }

    @Test
    fun `onTreeSearchResultTapped resolves the same file-vs-markdown split as a direct tap`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FakeChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()

        assertEquals(ExplorerNavigationEvent.OpenMarkdownPreview("dev-1", "ws-1", "README.md"), viewModel.onTreeSearchResultTapped("README.md"))
        assertEquals(ExplorerNavigationEvent.OpenSourceEditor("dev-1", "ws-1", "app.kt"), viewModel.onTreeSearchResultTapped("app.kt"))
    }

    @Test
    fun `a workspace_tree_list failure surfaces as an error state, not a crash`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ExplorerViewModel(FailingTreeListChooshEngine(), deviceId = "dev-1", workspaceId = "ws-1")
        advanceUntilIdle()

        val tree = viewModel.state.value.tree
        assertNotNull(tree.error)
        assertTrue(tree.entries.isEmpty())
    }
}
