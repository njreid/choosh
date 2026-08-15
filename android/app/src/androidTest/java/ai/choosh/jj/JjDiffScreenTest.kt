package ai.choosh.jj

import ai.choosh.engine.ChangeKind
import ai.choosh.engine.DiffFileEntry
import ai.choosh.engine.DiffHunk
import ai.choosh.engine.DiffSegment
import ai.choosh.engine.DiffSegmentKind
import android.graphics.Bitmap
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.captureToImage
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.io.FileOutputStream
import org.junit.Rule
import org.junit.Test

/**
 * Real-device Compose rendering for the `JjDiff` item, per M3's exit
 * criteria: a rename (both old/new path visible), a binary file (metadata,
 * never garbled hunks), and a real conflicted-merge diff's marker text
 * surviving verbatim. Mirrors [ai.choosh.fleet.FleetDrawerTest]'s classic
 * `createComposeRule()` pattern (the `v2` rule never establishes a compose
 * hierarchy against a real Genymotion device, see that file's doc comment).
 */
@Suppress("DEPRECATION")
class JjDiffScreenTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun fixtureState() = JjDiffUiState(
        from = "change-a",
        to = "change-merge",
        files = listOf(
            // A real conflicted-merge diff, mirroring choosh-hostd's own
            // log_and_diff_handle_a_real_conflicted_merge_from_two_workspaces
            // test assertions against real jj 0.44.0 output.
            DiffFileEntry.Hunks(
                oldPath = "a.txt",
                newPath = "a.txt",
                hunks = listOf(
                    DiffHunk(
                        oldStart = 1,
                        oldLines = 1,
                        newStart = 1,
                        newLines = 7,
                        segments = listOf(
                            DiffSegment(DiffSegmentKind.ADDED, "<<<<<<< Conflict 1 of 1"),
                            DiffSegment(DiffSegmentKind.ADDED, "%%%%%%% Changes from base to side #1"),
                            DiffSegment(DiffSegmentKind.REMOVED, "hello"),
                            DiffSegment(DiffSegmentKind.ADDED, "hello"),
                            DiffSegment(DiffSegmentKind.ADDED, "from A"),
                            DiffSegment(DiffSegmentKind.ADDED, "+++++++ Contents of side #2"),
                            DiffSegment(DiffSegmentKind.ADDED, "hello"),
                        ),
                    ),
                ),
            ),
            DiffFileEntry.Hunks(oldPath = "docs/README.old.md", newPath = "docs/README.md", hunks = emptyList()),
            DiffFileEntry.Binary(path = "assets/logo.png", status = ChangeKind.MODIFIED, byteSize = 48_213),
        ),
    )

    @Test
    fun rendersARealConflictedMergeARenameAndABinaryFileWithoutCrashing() {
        composeTestRule.setContent {
            JjDiffScreen(state = fixtureState(), onFromChange = {}, onToChange = {}, onLoad = {})
        }

        composeTestRule.onNodeWithTag("diff-file-list").assertExists()
        // Each unified-diff content line renders as one Text with its
        // leading +/-/space prefix baked in (e.g. "+from A"), so this must
        // match as a substring, not the raw segment text alone.
        composeTestRule.onNodeWithText("from A", substring = true).assertExists()
        composeTestRule.onNodeWithTag("diff-pure-rename").assertExists()
        composeTestRule.onNodeWithTag("diff-binary-metadata").assertExists()
        composeTestRule.onNodeWithText("docs/README.old.md → docs/README.md").assertExists()

        saveScreenshot(composeTestRule, "jj-diff-screen")
    }
}

/**
 * Captures the currently-composed root as a PNG under the app's external
 * files dir (readable via `adb pull` without root on a debuggable install)
 * — the concrete visual-verification artifact for this pass's real-device
 * check, not a throwaway debugging aid left in place accidentally.
 */
internal fun saveScreenshot(rule: androidx.compose.ui.test.junit4.ComposeContentTestRule, name: String) {
    val bitmap = rule.onRoot().captureToImage().asAndroidBitmap()
    val dir = InstrumentationRegistry.getInstrumentation().targetContext.getExternalFilesDir(null)
    val file = File(dir, "$name.png")
    FileOutputStream(file).use { out -> bitmap.compress(Bitmap.CompressFormat.PNG, 100, out) }
}
