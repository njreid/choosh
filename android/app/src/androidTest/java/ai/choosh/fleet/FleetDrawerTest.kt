package ai.choosh.fleet

import ai.choosh.engine.ConnectionState
import ai.choosh.engine.DevHostPresence
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import org.junit.Rule
import org.junit.Test

/**
 * Compose UI tests for [FleetDrawer], per docs/specs/android-navigation.md's
 * "Fleet drawer" section: three switchable sort modes, attention flagging as
 * a per-row property present in every mode, and Project-tap opening the
 * primary workspace directly. Uses [FleetDrawer]'s existing production
 * `testTag`s (`sort-mode-<mode>`, `fleet-row-<id>`, `attention-dot`,
 * `fleet-row-list`) — no test-only tags were added, they were already there.
 *
 * Uses the classic (non-`v2`) `createComposeRule()`: a prior pass switched
 * to `androidx.compose.ui.test.junit4.v2.createComposeRule` to silence a
 * deprecation warning, but that rule never established a compose hierarchy
 * on a real device (every test failed with "No compose hierarchies found in
 * the app" against a real Genymotion-cloud Android 16 device) — it is not a
 * drop-in replacement despite the deprecation notice implying otherwise.
 * The classic rule auto-launches an empty host `ComponentActivity`; suppress
 * the deprecation locally rather than reintroduce a broken rule to satisfy
 * this project's `warningsAsErrors` lint setting.
 */
@Suppress("DEPRECATION")
class FleetDrawerTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun devHost(id: String, alias: String) = DevHostPresence(
        deviceId = id,
        alias = alias,
        platform = "linux",
        accountLabel = null,
        connectionState = ConnectionState.ONLINE,
        lastSeen = "2026-08-14T20:00:00Z",
    )

    private fun stateWithFixtures(sortMode: SortMode): FleetUiState {
        val hosts = listOf(devHost("dev-a", "host-a"))
        val projects = listOf(
            Project(
                projectId = "proj-1",
                name = "flagged-project",
                primaryWorkspaceId = "ws-hot",
                workspaces = listOf(
                    Workspace("ws-calm", "calm-workspace", "dev-a", needsAttention = false, lastActiveAt = "2026-08-14T10:00:00Z"),
                    Workspace("ws-hot", "hot-workspace", "dev-a", needsAttention = true, lastActiveAt = "2026-08-14T11:00:00Z"),
                ),
            ),
        )
        return FleetUiState(sortMode = sortMode, rows = FleetViewModel.rowsFor(sortMode, hosts, projects), isLoading = false)
    }

    @Test
    fun projectModeRendersOneRowPerProject() {
        composeTestRule.setContent {
            FleetDrawer(
                state = stateWithFixtures(SortMode.PROJECT),
                onSortModeSelected = {},
                onProjectClick = {},
                onDevHostClick = {},
                onWorkspaceClick = {},
            )
        }

        composeTestRule.onNodeWithText("flagged-project").assertExists()
    }

    @Test
    fun switchingSortModeChangesWhatIsDisplayed() {
        // `state` must be genuine Compose state (`remember { mutableStateOf(...) }`),
        // not a plain `var`: reassigning a plain `var` captured in the content
        // lambda never triggers recomposition, since Compose's snapshot system
        // has nothing to observe on a plain variable — the UI would silently
        // never update no matter how long `waitForIdle()` waits.
        composeTestRule.setContent {
            var state by remember { mutableStateOf(stateWithFixtures(SortMode.PROJECT)) }
            FleetDrawer(
                state = state,
                onSortModeSelected = { mode -> state = stateWithFixtures(mode) },
                onProjectClick = {},
                onDevHostClick = {},
                onWorkspaceClick = {},
            )
        }

        // PROJECT mode: the project row is visible, not the individual workspace rows.
        composeTestRule.onNodeWithText("flagged-project").assertExists()

        composeTestRule.onNodeWithTag("sort-mode-recent").performClick()
        composeTestRule.waitForIdle()

        // RECENT mode: flat workspace rows, no project-level row.
        composeTestRule.onNodeWithText("hot-workspace").assertExists()
        composeTestRule.onNodeWithText("calm-workspace").assertExists()
    }

    @Test
    fun attentionDotRendersOnlyOnTheFlaggedRowInEveryMode() {
        // `setContent` may only be called once per test (a second call throws
        // "has already set content") — drive the mode change through
        // recomposable state instead of re-invoking `setContent` per mode.
        var currentMode by mutableStateOf(SortMode.PROJECT)
        composeTestRule.setContent {
            FleetDrawer(
                state = stateWithFixtures(currentMode),
                onSortModeSelected = {},
                onProjectClick = {},
                onDevHostClick = {},
                onWorkspaceClick = {},
            )
        }

        for (mode in SortMode.entries) {
            currentMode = mode
            composeTestRule.waitForIdle()

            // Every mode in this fixture has exactly one attention-needing row
            // (the project/devhost/hot-workspace), never zero, never more than one.
            composeTestRule.onNodeWithTag("attention-dot", useUnmergedTree = true).assertExists()
        }
    }

    @Test
    fun tappingAProjectRowInvokesOnProjectClickWithThatProject() {
        var clicked: Project? = null
        composeTestRule.setContent {
            FleetDrawer(
                state = stateWithFixtures(SortMode.PROJECT),
                onSortModeSelected = {},
                onProjectClick = { clicked = it },
                onDevHostClick = {},
                onWorkspaceClick = {},
            )
        }

        composeTestRule.onNodeWithTag("fleet-row-project:proj-1").performClick()

        assert(clicked?.primaryWorkspaceId == "ws-hot") {
            "expected the tapped project's primaryWorkspaceId to be ws-hot, got ${clicked?.primaryWorkspaceId}"
        }
    }

    @Test
    fun loadingStateRendersLoadingText() {
        composeTestRule.setContent {
            FleetDrawer(
                state = FleetUiState(isLoading = true),
                onSortModeSelected = {},
                onProjectClick = {},
                onDevHostClick = {},
                onWorkspaceClick = {},
            )
        }

        composeTestRule.onNodeWithText("Loading fleet…").assertExists()
    }

    @Test
    fun errorStateRendersErrorMessageInsteadOfRows() {
        composeTestRule.setContent {
            FleetDrawer(
                state = FleetUiState(isLoading = false, error = "network unreachable"),
                onSortModeSelected = {},
                onProjectClick = {},
                onDevHostClick = {},
                onWorkspaceClick = {},
            )
        }

        composeTestRule.onNodeWithText("Couldn't load the fleet: network unreachable").assertExists()
    }
}
