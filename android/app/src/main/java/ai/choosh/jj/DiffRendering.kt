package ai.choosh.jj

import ai.choosh.engine.DiffFileEntry
import ai.choosh.engine.DiffHunk
import ai.choosh.engine.DiffSegmentKind

/**
 * One renderable line of a unified diff: either a `@@ ... @@` hunk header
 * or one content line carrying its original context/added/removed kind
 * (for coloring) and the leading `' '`/`'+'`/`'-'` prefix character a
 * unified diff conventionally shows.
 */
sealed interface DiffLine {
    data class Header(val text: String) : DiffLine
    data class Content(val prefix: Char, val text: String, val kind: DiffSegmentKind) : DiffLine
}

/**
 * Flattens one file's [DiffHunk]s into unified-diff-style renderable
 * lines. Pure presentation over already-host-computed hunks — per M3's
 * hard "no diff computation on Android" requirement, this never derives a
 * hunk or a segment itself, only formats the ones `workspace.diff` already
 * returned verbatim.
 */
fun unifiedDiffLines(hunks: List<DiffHunk>): List<DiffLine> = hunks.flatMap { hunk ->
    val header = DiffLine.Header("@@ -${hunk.oldStart},${hunk.oldLines} +${hunk.newStart},${hunk.newLines} @@")
    val content = hunk.segments.map { segment ->
        val prefix = when (segment.kind) {
            DiffSegmentKind.CONTEXT -> ' '
            DiffSegmentKind.ADDED -> '+'
            DiffSegmentKind.REMOVED -> '-'
        }
        DiffLine.Content(prefix, segment.text, segment.kind)
    }
    listOf(header) + content
}

/**
 * The header line shown above one [DiffFileEntry.Hunks] entry's content:
 * `"old → new"` for a rename (`oldPath != newPath`), or the single shared
 * path otherwise. Never renders a path a caller didn't supply — an add has
 * no `oldPath`, a delete has no `newPath`, per `jj-integration.md`.
 */
fun diffFileHeader(entry: DiffFileEntry.Hunks): String = when {
    entry.oldPath != null && entry.newPath != null && entry.oldPath != entry.newPath -> "${entry.oldPath} → ${entry.newPath}"
    entry.newPath != null -> entry.newPath
    entry.oldPath != null -> entry.oldPath
    else -> "(unknown path)"
}

/** A stable per-file key for list/testTag identity, unique even across a rename. */
fun diffFileKey(entry: DiffFileEntry): String = when (entry) {
    is DiffFileEntry.Hunks -> "hunks:${entry.oldPath.orEmpty()}:${entry.newPath.orEmpty()}"
    is DiffFileEntry.Binary -> "binary:${entry.path}"
}
