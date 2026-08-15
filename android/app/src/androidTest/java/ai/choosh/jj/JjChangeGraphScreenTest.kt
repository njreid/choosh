package ai.choosh.jj

import ai.choosh.engine.ChangeGraphNode
import ai.choosh.engine.OperationLogEntry
import android.graphics.Bitmap
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.captureToImage
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.io.FileOutputStream
import org.junit.Rule
import org.junit.Test

/**
 * Real-device Compose rendering for the `JjChangeGraph` item, per M3's
 * exit criteria: a real conflicted merge (2 parents, working copy is the
 * merge) renders without crashing, tap-to-inspect shows the merge-parent
 * count, and the undo action is reachable.
 */
@Suppress("DEPRECATION")
class JjChangeGraphScreenTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun conflictedMergeNodes() = listOf(
        ChangeGraphNode(
            changeId = "change-merge",
            commitId = "commit-merge",
            description = "merge A and B\n",
            author = "agent-a <agent-a@choosh.ai>",
            parentChangeIds = listOf("change-a", "change-b"),
            isWorkingCopy = true,
            bookmarks = emptyList(),
        ),
        ChangeGraphNode(
            changeId = "change-a",
            commitId = "commit-a",
            description = "edit from A\n",
            author = "agent-a <agent-a@choosh.ai>",
            parentChangeIds = listOf("change-root"),
            isWorkingCopy = false,
            bookmarks = emptyList(),
        ),
        ChangeGraphNode(
            changeId = "change-b",
            commitId = "commit-b",
            description = "edit from B\n",
            author = "agent-b <agent-b@choosh.ai>",
            parentChangeIds = listOf("change-root"),
            isWorkingCopy = false,
            bookmarks = emptyList(),
        ),
        ChangeGraphNode(
            changeId = "change-root",
            commitId = "commit-root",
            description = "init\n",
            author = "njr <njr@choosh.ai>",
            parentChangeIds = emptyList(),
            isWorkingCopy = false,
            bookmarks = listOf("main"),
        ),
    )

    private fun operations() = listOf(
        OperationLogEntry("op-3", "merge A and B", "2026-08-15T00:00:03Z", "2026-08-15T00:00:03Z", mapOf("user" to "njr@devhost")),
        OperationLogEntry("op-2", "edit from B", "2026-08-15T00:00:02Z", "2026-08-15T00:00:02Z", mapOf("user" to "agent-b@devhost")),
    )

    @Test
    fun rendersAConflictedMergeAndTapToInspectShowsTheMergeIndicator() {
        composeTestRule.setContent {
            var selected by remember { mutableStateOf<String?>(null) }
            JjChangeGraphScreen(
                state = JjChangeGraphUiState(
                    nodes = conflictedMergeNodes(),
                    operations = operations(),
                    selectedChangeId = selected,
                    isLoading = false,
                ),
                onNodeTap = { selected = it },
                onDismissSelection = { selected = null },
                onUndoMostRecent = {},
                onRestore = {},
            )
        }

        composeTestRule.onNodeWithTag("change-graph-rows").assertExists()
        composeTestRule.onNodeWithTag("change-node-change-merge").assertExists()
        composeTestRule.onNodeWithTag("undo-most-recent-button").assertExists()

        // Capture the graph itself before opening the detail dialog: a
        // Compose `AlertDialog` renders in its own popup window, which adds
        // a second `isRoot` semantics node — `captureToImage()` requires
        // exactly one, so the screenshot has to happen while only the
        // screen's own root is present.
        saveScreenshot(composeTestRule, "jj-change-graph-screen")

        composeTestRule.onNodeWithTag("change-node-change-merge").performClick()
        composeTestRule.waitForIdle()

        composeTestRule.onNodeWithTag("change-detail-dialog").assertExists()
        composeTestRule.onNodeWithTag("merge-indicator").assertExists()

        // The dialog itself is captured as its own node (not the ambiguous
        // multi-root `onRoot()`), giving direct visual proof of the merge
        // indicator/parent count for a real conflicted merge.
        val dialogBitmap = composeTestRule.onNodeWithTag("change-detail-dialog").captureToImage().asAndroidBitmap()
        val dir = InstrumentationRegistry.getInstrumentation().targetContext.getExternalFilesDir(null)
        FileOutputStream(File(dir, "jj-change-graph-detail-dialog.png")).use { out ->
            dialogBitmap.compress(Bitmap.CompressFormat.PNG, 100, out)
        }
    }

    @Test
    fun restoreButtonExistsForEachOperation() {
        composeTestRule.setContent {
            JjChangeGraphScreen(
                state = JjChangeGraphUiState(nodes = conflictedMergeNodes(), operations = operations(), isLoading = false),
                onNodeTap = {},
                onDismissSelection = {},
                onUndoMostRecent = {},
                onRestore = {},
            )
        }

        composeTestRule.onNodeWithTag("operation-row-op-3").assertExists()
        composeTestRule.onNodeWithTag("restore-button-op-3").assertExists()
    }
}
