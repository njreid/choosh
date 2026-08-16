package ai.choosh.nav

import androidx.compose.runtime.mutableStateListOf

/**
 * A minimal, real navigation back stack — the mechanism
 * `docs/specs/android-navigation.md`'s "Back behavior" section needs and
 * `ChooshApp.kt`'s previous hand-rolled `remember { mutableStateOf(...) }`
 * single-current-screen variable didn't have (UX-friction audit finding
 * #2): pushing a screen remembers where you came from, and popping returns
 * to exactly that screen, all the way back down to the root screen the
 * stack was constructed with.
 *
 * Deliberately a small custom class, not `androidx.navigation`/`NavHost`:
 * this app's existing `Screen` sealed interface (data classes carrying
 * live IDs like `workspaceId`/`deviceId`/`path`) is already a perfectly
 * good navigation-state representation, and `gradle/libs.versions.toml`
 * has no `navigation-compose` dependency to begin with — adopting one just
 * to get a back stack would mean inventing route strings/argument
 * (de)serialization for state this codebase already models directly, for
 * no behavioral gain here. A `List`-backed stack plus a real
 * `androidx.activity.compose.BackHandler` (see `ChooshApp.kt`) gets the
 * same "system Back pops one level" behavior with far less new surface
 * area, matching this codebase's existing hand-rolled-navigation style.
 *
 * Backed by a Compose [androidx.compose.runtime.snapshots.SnapshotStateList]
 * (via [mutableStateListOf]), not a plain `MutableList`, so mutating it
 * through [push]/[pop] recomposes whatever reads [current] — the same
 * reason the old `mutableStateOf<Screen>` variable it replaces worked, just
 * generalized from a single slot to a stack. This class itself has no
 * Android/Activity/`BackHandler` dependency — `mutableStateListOf`'s
 * snapshot-state system runs on plain JVM — so it's directly unit-testable
 * (see `ScreenBackStackTest`) independent of any Compose UI or
 * instrumentation harness.
 */
class ScreenBackStack<T>(root: T) {
    private val entries = mutableStateListOf(root)

    /** The screen currently on top of the stack — what should be rendered. */
    val current: T
        get() = entries.last()

    /** Whether [pop] would do anything. `false` only at the root (a single-entry stack). */
    val canPop: Boolean
        get() = entries.size > 1

    /** Pushes [screen] on top; [current] becomes [screen] until it (or something above it) is popped. */
    fun push(screen: T) {
        entries.add(screen)
    }

    /**
     * Pops one level, per `docs/specs/android-navigation.md`'s "Back
     * behavior": "a pinned page ... returns to the explorer", "the
     * explorer ... returns to the Workspace list" — each of those
     * transitions is exactly one [pop] of whatever [push] reached the
     * deeper screen with. Returns `false` (a no-op — never throws or
     * underflows) if already at the root, so callers (a `BackHandler`, an
     * on-screen Back affordance) can decide what root behavior means
     * (e.g. let the platform's default back handling — exit the app —
     * apply) without this class needing to know about Activities.
     */
    fun pop(): Boolean {
        if (entries.size <= 1) return false
        entries.removeAt(entries.lastIndex)
        return true
    }

    /** A stable snapshot of the whole stack, root first — for tests and debugging only. */
    fun snapshot(): List<T> = entries.toList()
}
