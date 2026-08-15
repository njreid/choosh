package ai.choosh.sourceeditor

import ai.choosh.engine.FakeChooshEngine
import android.util.Log
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.lifecycle.ViewModelStore
import androidx.lifecycle.ViewModelStoreOwner
import androidx.lifecycle.viewmodel.compose.LocalViewModelStoreOwner
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Rule
import org.junit.Test

/**
 * Not a correctness test (the save-state machine's transitions are already
 * covered thoroughly by [SourceEditorViewModelTest] against
 * [FakeChooshEngine]) — this drives [SourceEditorScreen] on a real device
 * so a real screenshot (`adb exec-out screencap -p`) can be taken while it
 * sits in a real, non-Clean save state, per this task's on-device
 * verification requirement. It logs a distinctive marker right before
 * pausing so an external `adb logcat` poll knows exactly when to capture.
 */
@Suppress("DEPRECATION") // classic createComposeRule() — see FleetDrawerTest's doc comment for why the v2 rule doesn't work here.
class SourceEditorScreenshotVerificationTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @Test
    fun rendersEditorAndSaveStateIndicatorForScreenshotCapture() {
        val engine = FakeChooshEngine()
        val owner = object : ViewModelStoreOwner {
            override val viewModelStore = ViewModelStore()
        }
        val viewModel = SourceEditorViewModel(
            engine,
            FakeChooshEngine.FIXTURE_DEVICE_ID,
            FakeChooshEngine.FIXTURE_WORKSPACE_ID,
            "README.md",
            debounceMs = 400L,
        )

        composeTestRule.setContent {
            CompositionLocalProvider(LocalViewModelStoreOwner provides owner) {
                SourceEditorScreen(viewModel = viewModel, path = "README.md", onBack = {})
            }
        }

        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.onAllNodesWithTag("save-state-clean").fetchSemanticsNodes().isNotEmpty()
        }

        // Drive a real edit through the debounce -> saving -> clean cycle, then leave a real
        // concurrent-write conflict on screen so the screenshot shows the conflict-resolution UI
        // (the highest-value thing to see actually rendering, per editor-protocol.md's M4 exit
        // criterion), not just the empty-state Clean indicator.
        composeTestRule.runOnUiThread { viewModel.onContentChanged("# Choosh\n\nedited on a real device.\n") }
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.onAllNodesWithTag("save-state-clean").fetchSemanticsNodes().isNotEmpty()
        }

        engine.simulateConcurrentWrite(
            FakeChooshEngine.FIXTURE_DEVICE_ID,
            FakeChooshEngine.FIXTURE_WORKSPACE_ID,
            "README.md",
            "someone else's content, written concurrently",
        )
        composeTestRule.runOnUiThread {
            viewModel.onContentChanged("# Choosh\n\nedited on a real device, then conflicted.\n")
        }
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.onAllNodesWithTag("conflict-banner").fetchSemanticsNodes().isNotEmpty()
        }
        composeTestRule.waitForIdle()

        Log.i("SourceEditorScreenshotTest", "ready-for-screenshot")
        Thread.sleep(15_000)
    }

    /**
     * Unlike the test above (which drives [SourceEditorViewModel] directly),
     * this one injects real keystrokes into the on-screen Sora `CodeEditor`
     * via `input text` — exercising the actual `ContentChangeEvent`
     * subscription wired up in [SourceEditorScreen]'s `SoraEditorHost`, not
     * just the ViewModel's own API. Confirms the debounce->save cycle is
     * visible on screen from a real edit, not only from a synthetic one.
     */
    @Test
    fun typingIntoTheRealEditorDrivesAVisibleDebouncedSave() {
        val engine = FakeChooshEngine()
        val owner = object : ViewModelStoreOwner {
            override val viewModelStore = ViewModelStore()
        }
        val viewModel = SourceEditorViewModel(
            engine,
            FakeChooshEngine.FIXTURE_DEVICE_ID,
            FakeChooshEngine.FIXTURE_WORKSPACE_ID,
            "README.md",
            debounceMs = 1_500L,
        )

        composeTestRule.setContent {
            CompositionLocalProvider(LocalViewModelStoreOwner provides owner) {
                SourceEditorScreen(viewModel = viewModel, path = "README.md", onBack = {})
            }
        }
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.onAllNodesWithTag("save-state-clean").fetchSemanticsNodes().isNotEmpty()
        }

        composeTestRule.onNodeWithTag("source-editor-view").performClick()
        composeTestRule.waitForIdle()
        InstrumentationRegistry.getInstrumentation().uiAutomation
            .executeShellCommand("input text real_keystrokes_from_adb_shell")
            .close()

        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.onAllNodesWithTag("save-state-dirty").fetchSemanticsNodes().isNotEmpty()
        }
        Log.i("SourceEditorScreenshotTest", "ready-for-screenshot-dirty")
        Thread.sleep(4_000)

        composeTestRule.waitUntil(timeoutMillis = 10_000) {
            composeTestRule.onAllNodesWithTag("save-state-clean").fetchSemanticsNodes().isNotEmpty()
        }
        Log.i("SourceEditorScreenshotTest", "ready-for-screenshot-clean")
        Thread.sleep(10_000)
    }
}
