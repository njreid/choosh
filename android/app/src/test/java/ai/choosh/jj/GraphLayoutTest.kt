package ai.choosh.jj

import ai.choosh.engine.ChangeGraphNode
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * [layoutChangeGraph] against fixed [ChangeGraphNode] lists, per M3's exit
 * criterion that the change graph "reflects concurrent edits from two `jj
 * workspace`s against the same repo correctly, including a conflicted
 * merge" — this is the layout half of that (the node/edge *data* itself is
 * exercised end-to-end against a real repo in
 * choosh-hostd::jj_ops::tests::log_and_diff_handle_a_real_conflicted_merge_from_two_workspaces).
 */
class GraphLayoutTest {

    private fun node(
        changeId: String,
        parentChangeIds: List<String> = emptyList(),
        isWorkingCopy: Boolean = false,
    ) = ChangeGraphNode(
        changeId = changeId,
        commitId = "commit-$changeId",
        description = "$changeId\n",
        author = "a <a@b.c>",
        parentChangeIds = parentChangeIds,
        isWorkingCopy = isWorkingCopy,
        bookmarks = emptyList(),
    )

    @Test
    fun `empty input lays out to nothing`() {
        assertEquals(emptyList<GraphPosition>(), layoutChangeGraph(emptyList()))
    }

    @Test
    fun `a single root node lands at row zero`() {
        val positions = layoutChangeGraph(listOf(node("root", isWorkingCopy = true)))
        assertEquals(listOf(GraphPosition("root", row = 0, column = 0)), positions)
    }

    @Test
    fun `a linear chain lays out newest-first, one row per generation`() {
        val nodes = listOf(
            node("root"),
            node("mid", parentChangeIds = listOf("root")),
            node("head", parentChangeIds = listOf("mid"), isWorkingCopy = true),
        )

        val positions = layoutChangeGraph(nodes).associateBy { it.changeId }

        assertEquals(0, positions.getValue("head").row)
        assertEquals(1, positions.getValue("mid").row)
        assertEquals(2, positions.getValue("root").row)
    }

    @Test
    fun `a conflicted 2-parent merge lands its parents on the same row, one generation before the merge`() {
        // Mirrors choosh-hostd's real conflicted-merge test scenario: two
        // `jj workspace add` working copies (change-a, change-b) both
        // descending from a shared root, merged into a 2-parent node.
        val nodes = listOf(
            node("root"),
            node("change-a", parentChangeIds = listOf("root")),
            node("change-b", parentChangeIds = listOf("root")),
            node("change-merge", parentChangeIds = listOf("change-a", "change-b"), isWorkingCopy = true),
        )

        val positions = layoutChangeGraph(nodes).associateBy { it.changeId }

        assertEquals("the merge should be the newest (row 0) node", 0, positions.getValue("change-merge").row)
        assertEquals(1, positions.getValue("change-a").row)
        assertEquals(1, positions.getValue("change-b").row)
        assertEquals(2, positions.getValue("root").row)
        // Both parents share a row but occupy distinct columns, deterministically
        // ordered by change_id — proving the layout doesn't collapse them onto
        // each other or silently drop one.
        assertTrue(positions.getValue("change-a").column != positions.getValue("change-b").column)
    }

    @Test
    fun `column order within a row is deterministic by change id`() {
        val nodes = listOf(node("root"), node("zeta", parentChangeIds = listOf("root")), node("alpha", parentChangeIds = listOf("root")))
        val positions = layoutChangeGraph(nodes).associateBy { it.changeId }
        assertTrue(positions.getValue("alpha").column < positions.getValue("zeta").column)
    }

    @Test
    fun `a node whose parent is outside the returned set is treated as its own root`() {
        // The revset boundary (workspace.log's `limit`) can hand back a node
        // whose parent isn't itself in the response — this must not crash or
        // require every parent to be resolvable.
        val nodes = listOf(node("orphan", parentChangeIds = listOf("not-in-this-response"), isWorkingCopy = true))
        val positions = layoutChangeGraph(nodes)
        assertEquals(listOf(GraphPosition("orphan", row = 0, column = 0)), positions)
    }

    @Test
    fun `layout never crashes on repeated calls with the same node set`() {
        // A cheap regression guard for the M3 exit criterion "no crash" — repeated
        // layout calls (as a refresh cycle after undo/restore would trigger) must
        // keep producing the same deterministic result.
        val nodes = listOf(
            node("root"),
            node("change-a", parentChangeIds = listOf("root")),
            node("change-b", parentChangeIds = listOf("root")),
            node("change-merge", parentChangeIds = listOf("change-a", "change-b"), isWorkingCopy = true),
        )
        val first = layoutChangeGraph(nodes)
        val second = layoutChangeGraph(nodes)
        assertEquals(first, second)
    }
}
