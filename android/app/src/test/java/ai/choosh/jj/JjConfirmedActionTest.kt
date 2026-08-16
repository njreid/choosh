package ai.choosh.jj

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * UX-friction audit finding #9: [JjChangeGraphScreen]'s "Undo last op" and
 * each `OperationRow`'s "Restore" button used to fire [confirmedAction]'s
 * two real, history-rewriting actions ([ai.choosh.jj.JjChangeGraphViewModel.undoMostRecentOperation]/
 * [ai.choosh.jj.JjChangeGraphViewModel.restore]) the instant they were
 * tapped, with no confirmation step. [confirmedAction] is what a confirm
 * dialog's own "Confirm" button now invokes — pinned down here directly
 * since a `remember`-backed Composable's `onClick` body has no other JVM
 * unit test seam.
 */
class JjConfirmedActionTest {
    @Test
    fun `confirming an UndoMostRecent pending action invokes onUndoMostRecent, never onRestore`() {
        var undoCalls = 0
        var restoreCalledWith: String? = null

        confirmedAction(
            PendingOpConfirmation.UndoMostRecent,
            onUndoMostRecent = { undoCalls += 1 },
            onRestore = { restoreCalledWith = it },
        )()

        assertEquals(1, undoCalls)
        assertNull("undo must never invoke onRestore", restoreCalledWith)
    }

    @Test
    fun `confirming a Restore pending action invokes onRestore with exactly that operation's id, never onUndoMostRecent`() {
        var undoCalls = 0
        var restoreCalledWith: String? = null

        confirmedAction(
            PendingOpConfirmation.Restore(opId = "op-7", description = "edit from B"),
            onUndoMostRecent = { undoCalls += 1 },
            onRestore = { restoreCalledWith = it },
        )()

        assertEquals("op-7", restoreCalledWith)
        assertEquals("restore must never invoke onUndoMostRecent", 0, undoCalls)
    }
}
