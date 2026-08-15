package ai.choosh.jj

import ai.choosh.engine.ChangeKind
import ai.choosh.engine.DiffFileEntry
import ai.choosh.engine.DiffHunk
import ai.choosh.engine.DiffSegment
import ai.choosh.engine.DiffSegmentKind
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Diff-rendering logic against fixed [DiffFileEntry] fixtures, per M3's
 * exit criteria: a rename (both `oldPath`/`newPath` visible), a binary
 * file (metadata only, never garbled hunks), and a real conflicted-merge
 * diff's segments (jj's own conflict-marker text passes through as
 * ordinary content — this layer never special-cases or strips it).
 */
class DiffRenderingTest {

    @Test
    fun `unified lines carry a hunk header followed by prefixed content lines`() {
        val hunk = DiffHunk(
            oldStart = 1,
            oldLines = 2,
            newStart = 1,
            newLines = 3,
            segments = listOf(
                DiffSegment(DiffSegmentKind.CONTEXT, "line1"),
                DiffSegment(DiffSegmentKind.REMOVED, "line2"),
                DiffSegment(DiffSegmentKind.ADDED, "line-two-changed"),
                DiffSegment(DiffSegmentKind.ADDED, "line-three"),
            ),
        )

        val lines = unifiedDiffLines(listOf(hunk))

        assertEquals(DiffLine.Header("@@ -1,2 +1,3 @@"), lines[0])
        assertEquals(DiffLine.Content(' ', "line1", DiffSegmentKind.CONTEXT), lines[1])
        assertEquals(DiffLine.Content('-', "line2", DiffSegmentKind.REMOVED), lines[2])
        assertEquals(DiffLine.Content('+', "line-two-changed", DiffSegmentKind.ADDED), lines[3])
        assertEquals(DiffLine.Content('+', "line-three", DiffSegmentKind.ADDED), lines[4])
    }

    @Test
    fun `multiple hunks each get their own header`() {
        val hunkA = DiffHunk(1, 1, 1, 1, listOf(DiffSegment(DiffSegmentKind.REMOVED, "a")))
        val hunkB = DiffHunk(10, 1, 10, 1, listOf(DiffSegment(DiffSegmentKind.ADDED, "b")))

        val lines = unifiedDiffLines(listOf(hunkA, hunkB))

        assertEquals(4, lines.size)
        assertTrue(lines[0] is DiffLine.Header)
        assertTrue(lines[2] is DiffLine.Header)
    }

    @Test
    fun `a rename shows both old and new paths`() {
        val entry = DiffFileEntry.Hunks(oldPath = "docs/README.old.md", newPath = "docs/README.md", hunks = emptyList())
        assertEquals("docs/README.old.md → docs/README.md", diffFileHeader(entry))
    }

    @Test
    fun `a pure rename with identical content has an empty hunk list`() {
        // jj only pairs a rename when byte-for-byte content is unchanged — verified
        // against real jj 0.44.0 output (choosh-hostd's jj_ops.rs tests).
        val entry = DiffFileEntry.Hunks(oldPath = "src.txt", newPath = "renamed.txt", hunks = emptyList())
        assertEquals("src.txt → renamed.txt", diffFileHeader(entry))
        assertTrue(unifiedDiffLines(entry.hunks).isEmpty())
    }

    @Test
    fun `a same-path modify shows only the one path, not an arrow`() {
        val entry = DiffFileEntry.Hunks(oldPath = "a.txt", newPath = "a.txt", hunks = emptyList())
        assertEquals("a.txt", diffFileHeader(entry))
    }

    @Test
    fun `an add has only a new path`() {
        val entry = DiffFileEntry.Hunks(oldPath = null, newPath = "added.txt", hunks = emptyList())
        assertEquals("added.txt", diffFileHeader(entry))
    }

    @Test
    fun `a delete has only an old path`() {
        val entry = DiffFileEntry.Hunks(oldPath = "removed.txt", newPath = null, hunks = emptyList())
        assertEquals("removed.txt", diffFileHeader(entry))
    }

    @Test
    fun `binary metadata never produces unified diff lines`() {
        // Binary/oversized files are metadata-only per jj-integration.md — this
        // type doesn't even have a `hunks` field, so there's structurally no way
        // to call unifiedDiffLines on one; this test documents that boundary.
        val entry = DiffFileEntry.Binary(path = "assets/logo.png", status = ChangeKind.MODIFIED, byteSize = 48_213)
        assertEquals("binary:assets/logo.png", diffFileKey(entry))
    }

    @Test
    fun `diff file keys are stable and distinguish a rename from either of its endpoints`() {
        val rename = DiffFileEntry.Hunks(oldPath = "old.txt", newPath = "new.txt", hunks = emptyList())
        val modifyOld = DiffFileEntry.Hunks(oldPath = "old.txt", newPath = "old.txt", hunks = emptyList())
        val modifyNew = DiffFileEntry.Hunks(oldPath = "new.txt", newPath = "new.txt", hunks = emptyList())

        val keys = setOf(diffFileKey(rename), diffFileKey(modifyOld), diffFileKey(modifyNew))
        assertEquals(3, keys.size)
    }

    @Test
    fun `a real conflicted merge's marker text passes through as ordinary content`() {
        // Mirrors the exact assertion choosh-hostd's own
        // log_and_diff_handle_a_real_conflicted_merge_from_two_workspaces test
        // makes against real jj 0.44.0 output: jj's conflict-marker text is
        // ordinary +/-/space diff content, never special-cased or stripped here.
        val hunk = DiffHunk(
            oldStart = 1,
            oldLines = 1,
            newStart = 1,
            newLines = 7,
            segments = listOf(
                DiffSegment(DiffSegmentKind.ADDED, "<<<<<<< Conflict 1 of 1"),
                DiffSegment(DiffSegmentKind.ADDED, "%%%%%%% Changes from base to side #1"),
                DiffSegment(DiffSegmentKind.ADDED, "-hello"),
                DiffSegment(DiffSegmentKind.ADDED, "+hello"),
                DiffSegment(DiffSegmentKind.ADDED, "+from A"),
                DiffSegment(DiffSegmentKind.ADDED, "+++++++ Contents of side #2"),
                DiffSegment(DiffSegmentKind.ADDED, "hello"),
                DiffSegment(DiffSegmentKind.ADDED, "from B"),
                DiffSegment(DiffSegmentKind.ADDED, ">>>>>>> Conflict 1 of 1 ends"),
            ),
        )
        val entry = DiffFileEntry.Hunks(oldPath = null, newPath = "a.txt", hunks = listOf(hunk))

        val lines = unifiedDiffLines(entry.hunks)
        val allText = lines.joinToString("\n") { line -> if (line is DiffLine.Content) line.text else "" }

        assertTrue("expected A's content to survive verbatim", allText.contains("from A"))
        assertTrue("expected B's content to survive verbatim", allText.contains("from B"))
        assertTrue("expected jj's own conflict marker text, not stripped", allText.contains("Conflict"))
    }
}
