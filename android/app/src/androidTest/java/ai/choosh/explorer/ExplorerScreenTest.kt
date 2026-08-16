package ai.choosh.explorer

import ai.choosh.engine.AgentRunStatus
import ai.choosh.engine.ChangeKind
import ai.choosh.engine.ChangedPath
import ai.choosh.engine.TreeEntry
import ai.choosh.engine.TreeEntryKind
import ai.choosh.engine.WebServiceStatus
import ai.choosh.jj.saveScreenshot
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import org.junit.Rule
import org.junit.Test

/**
 * Real-device Compose rendering for all four of the explorer's page-zero
 * sections (docs/specs/android-navigation.md's Page model: active agents,
 * registered development services, changed files, searchable project
 * tree) — the UX-friction audit's finding #4 ("The Explorer only
 * implements 1 of its 4 spec'd sections") closed.
 */
@Suppress("DEPRECATION")
class ExplorerScreenTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @Test
    fun rendersChangedFilesWithAConflictedPathMarkedDistinctly() {
        val state = ExplorerUiState(
            changed = listOf(
                ChangedPath("app/src/Old.kt", ChangeKind.MODIFIED),
                ChangedPath("README.md", ChangeKind.ADDED),
                ChangedPath("a.txt", ChangeKind.MODIFIED),
            ),
            conflicted = listOf("a.txt"),
            isLoading = false,
        )

        composeTestRule.setContent { ExplorerScreen(state = state, onFileClick = {}, onRefresh = {}) }

        composeTestRule.onNodeWithTag("changed-file-list").assertExists()
        composeTestRule.onNodeWithTag("changed-file-a.txt").assertExists()
        // The marker Text's testTag gets merged into its parent Row's
        // semantics node (Compose's default merging for a simple Row of
        // Text children) — the same reason FleetDrawerTest's attention-dot
        // check needs it too.
        composeTestRule.onNodeWithTag("conflicted-marker-a.txt", useUnmergedTree = true).assertExists()

        saveScreenshot(composeTestRule, "explorer-screen")
    }

    @Test
    fun rendersAllFourSectionsAndRealItemPinningTapsFireTheRightCallback() {
        val state = ExplorerUiState(
            changed = listOf(ChangedPath("README.md", ChangeKind.ADDED)),
            conflicted = emptyList(),
            isLoading = false,
            agents = listOf(AgentRow("item-agent-1", "agent-a", AgentRunStatus.BUSY)),
            services = listOf(ServiceRow("item-web-1", "web", 3000, WebServiceStatus.RUNNING)),
            tree = TreeUiState(
                pathPrefix = "",
                entries = listOf(
                    TreeEntry("README.md", TreeEntryKind.FILE, conflicted = false),
                    TreeEntry("docs", TreeEntryKind.DIRECTORY, conflicted = false),
                ),
            ),
        )
        var agentClicked: AgentRow? = null
        var serviceClicked: ServiceRow? = null
        var treeEntryClicked: TreeEntry? = null

        composeTestRule.setContent {
            ExplorerScreen(
                state = state,
                onFileClick = {},
                onRefresh = {},
                onAgentClick = { agentClicked = it },
                onServiceClick = { serviceClicked = it },
                onTreeEntryClick = { treeEntryClicked = it },
            )
        }

        // All four sections are genuinely present, not a placeholder.
        composeTestRule.onNodeWithTag("agent-section").assertExists()
        composeTestRule.onNodeWithTag("service-section").assertExists()
        composeTestRule.onNodeWithTag("changed-file-list").assertExists()
        composeTestRule.onNodeWithTag("tree-section").assertExists()

        composeTestRule.onNodeWithTag("agent-row-item-agent-1").assertExists()
        composeTestRule.onNodeWithTag("agent-row-item-agent-1").performClick()
        assert(agentClicked?.itemId == "item-agent-1") { "tapping the real agent row must resolve to its real item, got $agentClicked" }

        composeTestRule.onNodeWithTag("service-row-item-web-1").assertExists()
        composeTestRule.onNodeWithTag("service-row-item-web-1").performClick()
        assert(serviceClicked?.itemId == "item-web-1") { "tapping the real service row must resolve to its real item, got $serviceClicked" }

        composeTestRule.onNodeWithTag("tree-entry-README.md").assertExists()
        composeTestRule.onNodeWithTag("tree-entry-README.md").performClick()
        assert(treeEntryClicked?.name == "README.md") { "tapping the real tree file must resolve to its real entry, got $treeEntryClicked" }

        saveScreenshot(composeTestRule, "explorer-screen-all-sections")
    }

    @Test
    fun emptyAgentAndServiceSectionsRenderCleanEmptyStatesNotErrors() {
        val state = ExplorerUiState(isLoading = false, agents = emptyList(), services = emptyList())

        composeTestRule.setContent { ExplorerScreen(state = state, onFileClick = {}, onRefresh = {}) }

        composeTestRule.onNodeWithTag("agent-empty").assertExists()
        composeTestRule.onNodeWithTag("service-empty").assertExists()
    }
}
