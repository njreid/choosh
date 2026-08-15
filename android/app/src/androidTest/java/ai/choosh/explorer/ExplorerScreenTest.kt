package ai.choosh.explorer

import ai.choosh.engine.ChangeKind
import ai.choosh.engine.ChangedPath
import ai.choosh.jj.saveScreenshot
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import org.junit.Rule
import org.junit.Test

/**
 * Real-device Compose rendering for the explorer's changed-files section
 * (`workspace.status`), including a conflicted path shown distinctly.
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
}
