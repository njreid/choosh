package ai.choosh.ui

import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.platform.LocalConfiguration

/**
 * A minimal width breakpoint, used to give Explorer/Fleet/Workspace real
 * adaptive behavior at tablet/DeX-sized displays instead of stretching a
 * phone layout — `docs/accessibility-device-report.md`'s item 3/4 finding
 * ("no `WindowSizeClass`-driven layout choice anywhere in the codebase...
 * no master-detail split, no multi-column grid").
 *
 * **Why a hand-rolled breakpoint instead of
 * `androidx.compose.material3.windowsizeclass`**: that artifact is not a
 * current Gradle dependency (`gradle/libs.versions.toml` has no
 * `material3-window-size-class` entry), and the only thing this pass needs
 * from it is exactly the two width thresholds Material's own spec already
 * documents (600dp, 840dp) — adding a whole new dependency (with its own
 * version to track and its `ExperimentalMaterial3WindowSizeClassApi` opt-in,
 * as of the Compose BOM pinned here) for a two-branch `when` used in three
 * screens is more machinery than this scope warrants. The breakpoints
 * themselves are the same ones that API would report
 * ([COMPACT_MAX_WIDTH_DP]/[MEDIUM_MAX_WIDTH_DP] match Material's own
 * `WindowWidthSizeClass` thresholds), so swapping in the real dependency
 * later — if a fourth/fifth screen needs the same logic, or the richer
 * height-class dimension — is a drop-in replacement, not a redesign.
 */
enum class WindowWidthSizeClass {
    COMPACT,
    MEDIUM,
    EXPANDED,
}

private const val COMPACT_MAX_WIDTH_DP = 600
private const val MEDIUM_MAX_WIDTH_DP = 840

/** The current window's width breakpoint, recomputed on every configuration change (rotation, resize, DeX/external display). */
@Composable
fun rememberWindowWidthSizeClass(): WindowWidthSizeClass {
    val widthDp = LocalConfiguration.current.screenWidthDp
    return remember(widthDp) {
        when {
            widthDp < COMPACT_MAX_WIDTH_DP -> WindowWidthSizeClass.COMPACT
            widthDp < MEDIUM_MAX_WIDTH_DP -> WindowWidthSizeClass.MEDIUM
            else -> WindowWidthSizeClass.EXPANDED
        }
    }
}
