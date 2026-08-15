package ai.choosh

import ai.choosh.engine.FakeChooshEngine
import ai.choosh.explorer.ExplorerScreen
import ai.choosh.explorer.ExplorerViewModel
import ai.choosh.fleet.FleetDrawer
import ai.choosh.fleet.FleetViewModel
import ai.choosh.jj.JjChangeGraphScreen
import ai.choosh.jj.JjChangeGraphViewModel
import ai.choosh.jj.JjDiffScreen
import ai.choosh.jj.JjDiffViewModel
import ai.choosh.markdown.MarkdownFixtureDemoScreen
import ai.choosh.terminal.TerminalScreen
import ai.choosh.webservice.WebServiceScreen
import ai.choosh.webservice.WebServiceViewModel
import android.util.Log
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import org.junit.Rule
import org.junit.Test

/**
 * TEMPORARY, testing-only. Not part of the app's shipped surface (this is
 * `androidTest`, never packaged into `app-debug.apk`/`app-release.apk`).
 *
 * M8's device/accessibility pass (docs/accessibility-device-report.md) needs
 * to drive every pinned-item surface for real on a real device. The
 * sandboxed Genymotion cloud device used for that pass has zero
 * `CredentialProviderService` implementations registered (no Google Play
 * Services, no password-manager app) — confirmed both with and without a
 * device PIN configured — so Credential Manager's real WebAuthn ceremony
 * can never be presented ("No create options available."), which makes
 * [ai.choosh.ChooshApp]'s normal Connection -> Fleet -> Workspace navigation
 * unreachable on this specific device, independent of any app defect.
 *
 * This harness mounts the exact same production composables
 * ([ai.choosh.workspace.WorkspaceScreen]'s constituent tabs,
 * [TerminalScreen], [WebServiceScreen], [MarkdownFixtureDemoScreen]) that
 * [ai.choosh.ChooshApp] would otherwise reach past Connection, against the
 * same [FakeChooshEngine] fixture data, using this repo's established
 * on-device verification convention (see
 * `ai.choosh.sourceeditor.SourceEditorScreenshotVerificationTest`'s doc
 * comment: "drives [a screen] on a real device so a real screenshot... can
 * be taken", logging a distinctive marker before each pause so an external
 * `adb logcat` poll knows when to capture). Every pixel rendered here is
 * unmodified production Kotlin — this substitutes only for [ChooshApp]'s
 * navigation shell, not for any screen under test.
 */
@Suppress("DEPRECATION") // classic createComposeRule() — see FleetDrawerTest's doc comment for why the v2 rule doesn't work here.
class DeviceVerificationHarnessTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    // `setContent` may only be called once per test (each throws "has already set content"
    // otherwise, per FleetDrawerTest's own doc comment on this exact constraint) — one screen
    // per @Test method rather than trying to sequence several through one Activity.

    @Test
    fun explorerScreen() {
        val engine = FakeChooshEngine()
        val explorerViewModel = ExplorerViewModel(engine, FakeChooshEngine.FIXTURE_DEVICE_ID, FakeChooshEngine.FIXTURE_WORKSPACE_ID)
        composeTestRule.setContent {
            val explorerState by explorerViewModel.state.collectAsState()
            ExplorerScreen(state = explorerState, onFileClick = {}, onRefresh = explorerViewModel::refresh)
        }
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.onAllNodesWithTag("changed-file-list").fetchSemanticsNodes().isNotEmpty()
        }
        composeTestRule.waitForIdle()
        Log.i("DeviceHarness", "ready-explorer")
        Thread.sleep(45000)
    }

    @Test
    fun diffScreen() {
        val engine = FakeChooshEngine()
        val diffViewModel = JjDiffViewModel(engine, FakeChooshEngine.FIXTURE_DEVICE_ID, FakeChooshEngine.FIXTURE_WORKSPACE_ID)
        composeTestRule.setContent {
            val diffState by diffViewModel.state.collectAsState()
            JjDiffScreen(state = diffState, onFromChange = diffViewModel::setFrom, onToChange = diffViewModel::setTo, onLoad = diffViewModel::load)
        }
        composeTestRule.onNodeWithTag("diff-load-button").performClick()
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.onAllNodesWithTag("diff-file-list").fetchSemanticsNodes().isNotEmpty()
        }
        composeTestRule.waitForIdle()
        Log.i("DeviceHarness", "ready-diff")
        Thread.sleep(45000)
    }

    @Test
    fun graphScreenAndDialog() {
        val engine = FakeChooshEngine()
        val graphViewModel = JjChangeGraphViewModel(engine, FakeChooshEngine.FIXTURE_DEVICE_ID, FakeChooshEngine.FIXTURE_WORKSPACE_ID)
        composeTestRule.setContent {
            val graphState by graphViewModel.state.collectAsState()
            JjChangeGraphScreen(
                state = graphState,
                onNodeTap = graphViewModel::selectChange,
                onDismissSelection = graphViewModel::dismissSelection,
                onUndoMostRecent = graphViewModel::undoMostRecentOperation,
                onRestore = graphViewModel::restore,
            )
        }
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.onAllNodesWithTag("change-graph-rows").fetchSemanticsNodes().isNotEmpty()
        }
        composeTestRule.waitForIdle()
        Log.i("DeviceHarness", "ready-graph")
        Thread.sleep(35000)

        composeTestRule.onNodeWithTag("change-node-change-merge").performClick()
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.onAllNodesWithTag("change-detail-dialog").fetchSemanticsNodes().isNotEmpty()
        }
        composeTestRule.waitForIdle()
        Log.i("DeviceHarness", "ready-graph-dialog")
        Thread.sleep(45000)
    }

    /**
     * Deterministic (no external-adb-timing race) check of whether the dialog's own declared
     * dismiss path works at all via Compose's test click API, to disambiguate a real
     * dismiss-logic defect from an external-input-targeting issue when `adb shell input
     * keyevent`/`input tap` didn't appear to dismiss the same dialog.
     */
    @Test
    fun graphDialogDismissesViaComposeClick() {
        val engine = FakeChooshEngine()
        val graphViewModel = JjChangeGraphViewModel(engine, FakeChooshEngine.FIXTURE_DEVICE_ID, FakeChooshEngine.FIXTURE_WORKSPACE_ID)
        composeTestRule.setContent {
            val graphState by graphViewModel.state.collectAsState()
            JjChangeGraphScreen(
                state = graphState,
                onNodeTap = graphViewModel::selectChange,
                onDismissSelection = graphViewModel::dismissSelection,
                onUndoMostRecent = graphViewModel::undoMostRecentOperation,
                onRestore = graphViewModel::restore,
            )
        }
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.onAllNodesWithTag("change-graph-rows").fetchSemanticsNodes().isNotEmpty()
        }
        composeTestRule.onNodeWithTag("change-node-change-merge").performClick()
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.onAllNodesWithTag("change-detail-dialog").fetchSemanticsNodes().isNotEmpty()
        }
        composeTestRule.onNodeWithText("Close").performClick()
        composeTestRule.waitForIdle()
        val stillOpen = composeTestRule.onAllNodesWithTag("change-detail-dialog").fetchSemanticsNodes().isNotEmpty()
        Log.i("DeviceHarness", "dialog-dismiss-via-compose-click stillOpen=$stillOpen")
        Thread.sleep(5_000)
    }

    /**
     * Blank terminal, then the canned demo output, held for real hardware-keyboard
     * (`adb shell input keyevent`) and TalkBack-proxy (`uiautomator dump`) probing
     * from outside this process.
     */
    @Test
    fun terminalBlankThenDemoOutput() {
        composeTestRule.setContent {
            TerminalScreen(
                deviceId = FakeChooshEngine.FIXTURE_DEVICE_ID,
                itemId = FakeChooshEngine.FIXTURE_WORKSPACE_ID,
                connectionHandle = null,
                onBack = {},
            )
        }
        composeTestRule.waitForIdle()
        Log.i("DeviceHarness", "ready-terminal-blank")
        Thread.sleep(40000)

        composeTestRule.onNodeWithText("Demo output").performClick()
        composeTestRule.waitForIdle()
        Log.i("DeviceHarness", "ready-terminal-demo-output")
        Thread.sleep(45000)
    }

    /**
     * Sustained, high-volume, colorful output over ~100s by repeatedly driving the
     * screen's own real "Full redraw" demo-injection button (the same real VT-parser
     * -> damage-cache -> glyphon path a live PTY stream would exercise) while an
     * external `adb shell dumpsys meminfo ai.choosh` poll (run from outside this
     * process) samples RSS/GPU memory over time, per terminal-experience.md's
     * "bound glyph-atlas, scrollback, frame-queue... memory" requirement.
     */
    @Test
    fun terminalSustainedOutputMemoryProbe() {
        composeTestRule.setContent {
            TerminalScreen(
                deviceId = FakeChooshEngine.FIXTURE_DEVICE_ID,
                itemId = FakeChooshEngine.FIXTURE_WORKSPACE_ID,
                connectionHandle = null,
                onBack = {},
            )
        }
        composeTestRule.waitForIdle()
        Log.i("DeviceHarness", "sustained-start")

        val iterations = 320
        for (i in 1..iterations) {
            composeTestRule.onNodeWithText("Full redraw").performClick()
            if (i % 40 == 0) {
                composeTestRule.waitForIdle()
                Log.i("DeviceHarness", "sustained-progress iteration=$i")
            }
            Thread.sleep(300)
        }
        composeTestRule.waitForIdle()
        Log.i("DeviceHarness", "sustained-done")
        Thread.sleep(10_000)
    }

    @Test
    fun webServiceDemoStartingThenReady() {
        val engine = FakeChooshEngine()
        val viewModel = WebServiceViewModel(
            engine,
            FakeChooshEngine.FIXTURE_DEVICE_ID,
            FakeChooshEngine.FIXTURE_WORKSPACE_ID,
            itemId = "item-web-1",
            connectionHandle = null,
        )
        composeTestRule.setContent {
            val state by viewModel.state.collectAsState()
            WebServiceScreen(state = state)
        }
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.onAllNodesWithTag("web-service-starting").fetchSemanticsNodes().isNotEmpty()
        }
        Log.i("DeviceHarness", "ready-webservice-starting")
        Thread.sleep(30000)

        composeTestRule.runOnUiThread { viewModel.startPolling() }
        composeTestRule.waitUntil(timeoutMillis = 15_000) {
            composeTestRule.onAllNodesWithText("This service isn't running.").fetchSemanticsNodes().isNotEmpty() ||
                composeTestRule.onAllNodesWithTag("web-service-status").fetchSemanticsNodes().isNotEmpty() ||
                true // Ready mounts a real WebView with no distinctive testTag reachable from here; time-box instead.
        }
        Thread.sleep(6_000)
        Log.i("DeviceHarness", "ready-webservice-settled")
        Thread.sleep(40000)
    }

    /** Held for external screenshot capture at multiple display sizes/densities (DeX/tablet checks). */
    @Test
    fun fleetDrawerHeld() {
        val engine = FakeChooshEngine()
        // FleetViewModel.init { refresh() } calls engine.listDevhosts(), which FakeChooshEngine
        // guards with `check(connected)` — connect() first, same as ConnectionViewModel would.
        kotlinx.coroutines.runBlocking { engine.connect("device-harness-test") }
        val viewModel = FleetViewModel(engine)
        composeTestRule.setContent {
            val state by viewModel.state.collectAsState()
            FleetDrawer(
                state = state,
                onSortModeSelected = viewModel::setSortMode,
                onProjectClick = {},
                onDevHostClick = {},
                onWorkspaceClick = {},
            )
        }
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.onAllNodesWithTag("fleet-row-list").fetchSemanticsNodes().isNotEmpty()
        }
        composeTestRule.waitForIdle()
        Log.i("DeviceHarness", "ready-fleet")
        Thread.sleep(45_000)
    }

    @Test
    fun markdownFixtureDemo() {
        composeTestRule.setContent {
            MarkdownFixtureDemoScreen()
        }
        composeTestRule.waitForIdle()
        Log.i("DeviceHarness", "ready-markdown")
        Thread.sleep(45000)
    }
}
