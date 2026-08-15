package ai.choosh.jj

import ai.choosh.engine.DiffFileEntry
import ai.choosh.engine.DiffSegmentKind
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp

/**
 * The `JjDiff` item: native unified-diff rendering from [DiffFileEntry]
 * structured hunks — every hunk/segment here was computed by `jj-lib`
 * host-side (`workspace.diff`) and rendered verbatim, never recomputed on
 * device, per M3's exit criterion. `from`/`to` text fields are the "some
 * way to specify from/to" minimal revision picker the milestone allows
 * for this pass (a polished revision browser is out of scope).
 */
@Composable
fun JjDiffScreen(
    state: JjDiffUiState,
    onFromChange: (String) -> Unit,
    onToChange: (String) -> Unit,
    onLoad: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier) {
        Row(Modifier.fillMaxWidth().padding(8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedTextField(
                value = state.from,
                onValueChange = onFromChange,
                label = { Text("From (default @-)") },
                singleLine = true,
                modifier = Modifier.weight(1f).testTag("diff-from-field"),
            )
            OutlinedTextField(
                value = state.to,
                onValueChange = onToChange,
                label = { Text("To (default @)") },
                singleLine = true,
                modifier = Modifier.weight(1f).testTag("diff-to-field"),
            )
        }
        Button(
            onClick = onLoad,
            modifier = Modifier
                .padding(horizontal = 8.dp)
                .testTag("diff-load-button")
                // `docs/accessibility-device-report.md`'s item 1, gap 1
                // flagged this button by name ("the diff's
                // OutlinedTextField-adjacent Load diff button pattern") —
                // a context-specific label naming the actual revisions
                // about to be loaded, not just "Load diff".
                .semantics {
                    contentDescription = "Load diff from ${state.from.ifBlank { "default @-" }} to ${state.to.ifBlank { "default @" }}"
                },
        ) { Text("Load diff") }

        when {
            state.isLoading -> Text("Loading diff…", modifier = Modifier.padding(16.dp))
            state.error != null -> Text(
                "Couldn't load diff: ${state.error}",
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(16.dp).testTag("diff-error"),
            )
            state.files.isEmpty() -> Text("No changes.", modifier = Modifier.padding(16.dp).testTag("diff-empty"))
            else -> LazyColumn(Modifier.testTag("diff-file-list")) {
                items(state.files, key = { diffFileKey(it) }) { file -> DiffFileCard(file) }
            }
        }
    }
}

@Composable
private fun DiffFileCard(file: DiffFileEntry) {
    Card(modifier = Modifier.fillMaxWidth().padding(8.dp).testTag("diff-file-${diffFileKey(file)}")) {
        Column(Modifier.padding(8.dp)) {
            when (file) {
                is DiffFileEntry.Binary -> BinaryFileMetadata(file)
                is DiffFileEntry.Hunks -> HunksFileContent(file)
            }
        }
    }
}

/** Binary files are shown as metadata only — never a garbled diff attempt, per M3's exit criterion. */
@Composable
private fun BinaryFileMetadata(file: DiffFileEntry.Binary) {
    Text(file.path, style = MaterialTheme.typography.titleSmall)
    Text(
        "Binary file · ${file.status.name.lowercase()} · ${file.byteSize} bytes",
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.testTag("diff-binary-metadata"),
    )
}

@Composable
private fun HunksFileContent(file: DiffFileEntry.Hunks) {
    Text(diffFileHeader(file), style = MaterialTheme.typography.titleSmall)
    if (file.hunks.isEmpty()) {
        Text(
            "Renamed, no content change",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag("diff-pure-rename"),
        )
        return
    }
    unifiedDiffLines(file.hunks).forEach { line ->
        when (line) {
            is DiffLine.Header -> Text(
                line.text,
                fontFamily = FontFamily.Monospace,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            is DiffLine.Content -> Text(
                "${line.prefix}${line.text}",
                fontFamily = FontFamily.Monospace,
                style = MaterialTheme.typography.bodySmall,
                color = when (line.kind) {
                    DiffSegmentKind.ADDED -> Color(0xFF2E7D32)
                    DiffSegmentKind.REMOVED -> Color(0xFFC62828)
                    DiffSegmentKind.CONTEXT -> MaterialTheme.colorScheme.onSurface
                },
            )
        }
    }
}
