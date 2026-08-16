package ai.choosh.jj

import ai.choosh.engine.ChangeGraphNode
import ai.choosh.engine.OperationLogEntry
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp

/**
 * One pending destructive action awaiting the user's confirmation — see
 * [JjChangeGraphScreen]'s own doc comment on why this exists at all
 * (UX-friction audit finding #9). `internal`, not `private`: [confirmedAction]
 * below is unit-tested directly from `JjConfirmedActionTest`, the same
 * "pure derivation, unit-tested directly" precedent
 * `ai.choosh.workspace.workspaceScreenTitle`/`deriveWebServiceUiState` use
 * elsewhere in this codebase for logic that would otherwise only be
 * reachable through a full Compose UI test.
 */
internal sealed interface PendingOpConfirmation {
    data object UndoMostRecent : PendingOpConfirmation
    data class Restore(val opId: String, val description: String) : PendingOpConfirmation
}

/**
 * What confirming [pending] actually does — the one place
 * [JjChangeGraphScreen]'s confirm button's `onClick` and a JVM unit test can
 * both exercise the exact same logic (a `remember`-backed Composable's
 * `onClick` body itself has no such seam).
 */
internal fun confirmedAction(pending: PendingOpConfirmation, onUndoMostRecent: () -> Unit, onRestore: (String) -> Unit): () -> Unit =
    when (pending) {
        is PendingOpConfirmation.UndoMostRecent -> onUndoMostRecent
        is PendingOpConfirmation.Restore -> ({ onRestore(pending.opId) })
    }

/**
 * The `JjChangeGraph` item: an interactive DAG view over `workspace.log`,
 * tap-to-inspect a change (a local selection dialog — every field it shows
 * came from the already-fetched node), and one-tap `undo`/`op restore`
 * against `workspace.op.*`. Node positions come from [layoutChangeGraph],
 * a client-side *layout* only — the node/edge data itself is never
 * computed here, per M3's exit criterion.
 *
 * UX-friction audit finding #9: [onUndoMostRecent]/[onRestore] rewrite repo
 * history (a real `jj op undo`/`op restore` against the working copy) — an
 * earlier version of this screen fired them the instant "Undo last op" or a
 * row's "Restore" was tapped, with no confirmation step at all, indistinguishable
 * from every other single-tap navigation action in this app. A
 * [PendingOpConfirmation] held as local UI state plus one [AlertDialog] —
 * the same `AlertDialog` mechanism [ChangeDetailDialog] below already uses,
 * the precedent this fix matches — is the minimal fix: tapping either button
 * now only stages the action; [onUndoMostRecent]/[onRestore] themselves are
 * only ever invoked from the dialog's own confirm button.
 */
@Composable
fun JjChangeGraphScreen(
    state: JjChangeGraphUiState,
    onNodeTap: (String) -> Unit,
    onDismissSelection: () -> Unit,
    onUndoMostRecent: () -> Unit,
    onRestore: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    var pendingConfirmation by remember { mutableStateOf<PendingOpConfirmation?>(null) }

    Column(modifier) {
        Row(Modifier.fillMaxWidth().padding(8.dp), horizontalArrangement = Arrangement.SpaceBetween) {
            Text("Change graph", style = MaterialTheme.typography.titleMedium)
            TextButton(
                onClick = { pendingConfirmation = PendingOpConfirmation.UndoMostRecent },
                enabled = state.operations.isNotEmpty(),
                modifier = Modifier.testTag("undo-most-recent-button"),
            ) { Text("Undo last op") }
        }

        when {
            state.isLoading -> Text("Loading change graph…", modifier = Modifier.padding(16.dp))
            state.error != null -> Text(
                "Couldn't load change graph: ${state.error}",
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(16.dp).testTag("change-graph-error"),
            )
            else -> ChangeGraphAndOperations(
                state = state,
                onNodeTap = onNodeTap,
                onRequestRestore = { op -> pendingConfirmation = PendingOpConfirmation.Restore(op.opId, op.description) },
            )
        }
    }

    val selected = state.nodes.firstOrNull { it.changeId == state.selectedChangeId }
    if (selected != null) {
        ChangeDetailDialog(selected, onDismissSelection)
    }

    when (val pending = pendingConfirmation) {
        null -> Unit
        is PendingOpConfirmation.UndoMostRecent -> OpConfirmationDialog(
            title = "Undo this operation?",
            text = "This reverses the most recently listed operation in this workspace.",
            testTag = "undo-confirm-dialog",
            onConfirm = {
                pendingConfirmation = null
                confirmedAction(pending, onUndoMostRecent, onRestore)()
            },
            onCancel = { pendingConfirmation = null },
        )
        is PendingOpConfirmation.Restore -> OpConfirmationDialog(
            title = "Restore to this operation?",
            text = "This resets the working copy to \"${pending.description}\".",
            testTag = "restore-confirm-dialog",
            onConfirm = {
                pendingConfirmation = null
                confirmedAction(pending, onUndoMostRecent, onRestore)()
            },
            onCancel = { pendingConfirmation = null },
        )
    }
}

/** Shared Cancel/Confirm shape for [PendingOpConfirmation.UndoMostRecent]/[PendingOpConfirmation.Restore] — see [JjChangeGraphScreen]'s doc comment. */
@Composable
private fun OpConfirmationDialog(title: String, text: String, testTag: String, onConfirm: () -> Unit, onCancel: () -> Unit) {
    AlertDialog(
        onDismissRequest = onCancel,
        modifier = Modifier.testTag(testTag),
        title = { Text(title) },
        text = { Text(text) },
        confirmButton = { TextButton(onClick = onConfirm, modifier = Modifier.testTag("$testTag-confirm")) { Text("Confirm") } },
        dismissButton = { TextButton(onClick = onCancel, modifier = Modifier.testTag("$testTag-cancel")) { Text("Cancel") } },
    )
}

@Composable
private fun ChangeGraphAndOperations(state: JjChangeGraphUiState, onNodeTap: (String) -> Unit, onRequestRestore: (OperationLogEntry) -> Unit) {
    val positions = layoutChangeGraph(state.nodes).associateBy { it.changeId }
    val rows = state.nodes.groupBy { positions[it.changeId]?.row ?: 0 }.toSortedMap()

    LazyColumn(Modifier.testTag("change-graph-rows")) {
        rows.forEach { (rowIndex, rowNodes) ->
            item(key = "row-$rowIndex") {
                val scrollState = rememberScrollState()
                Row(
                    Modifier.fillMaxWidth().horizontalScroll(scrollState).padding(4.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    rowNodes.sortedBy { positions[it.changeId]?.column ?: 0 }.forEach { node ->
                        ChangeNodeChip(node, onClick = { onNodeTap(node.changeId) })
                    }
                }
            }
        }
        item(key = "operations-header") {
            Text("Operations", style = MaterialTheme.typography.titleSmall, modifier = Modifier.padding(16.dp, 24.dp, 16.dp, 8.dp))
        }
        items(state.operations, key = { it.opId }) { op -> OperationRow(op, onRestore = { onRequestRestore(op) }) }
    }
}

@Composable
private fun ChangeNodeChip(node: ChangeGraphNode, onClick: () -> Unit) {
    val background = when {
        node.isWorkingCopy -> Color(0xFF1565C0)
        node.parentChangeIds.size >= 2 -> Color(0xFF6A1B9A) // a merge, per the M3 conflicted-merge exit criterion
        else -> Color(0xFF424242)
    }
    Column(
        Modifier
            .clickable(onClick = onClick)
            .testTag("change-node-${node.changeId}")
            // `docs/accessibility-device-report.md`'s item 1, gap 1 named
            // `ChangeNodeChip` directly. A meaningful, context-specific
            // label — the change id, whether it's a merge, and its
            // description — rather than relying on `mergeDescendants` to
            // read out the truncated 8-char id and possibly-blank
            // description text this chip visually shows.
            .semantics {
                contentDescription = buildString {
                    append("Change ${node.changeId.take(8)}")
                    if (node.parentChangeIds.size >= 2) append(", merge of ${node.parentChangeIds.size} parents")
                    if (node.isWorkingCopy) append(", working copy")
                    val description = node.description.lineSequence().firstOrNull().orEmpty()
                    append(if (description.isBlank()) ", no description" else ", $description")
                }
            }
            .background(background, RoundedCornerShape(8.dp))
            .widthIn(min = 96.dp, max = 220.dp)
            .padding(8.dp),
    ) {
        Text(node.changeId.take(8), color = Color.White, style = MaterialTheme.typography.labelMedium)
        Text(
            node.description.lineSequence().firstOrNull().orEmpty().ifBlank { "(no description)" },
            color = Color.White,
            style = MaterialTheme.typography.bodySmall,
            maxLines = 2,
        )
    }
}

@Composable
private fun ChangeDetailDialog(node: ChangeGraphNode, onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        modifier = Modifier.testTag("change-detail-dialog"),
        title = { Text(node.changeId) },
        text = {
            Column {
                Text(node.description.ifBlank { "(no description)" })
                Text("Author: ${node.author}")
                Text("Parents: ${if (node.parentChangeIds.isEmpty()) "none" else node.parentChangeIds.joinToString()}")
                if (node.parentChangeIds.size >= 2) {
                    Text(
                        "Merge commit (${node.parentChangeIds.size} parents)",
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.testTag("merge-indicator"),
                    )
                }
                if (node.bookmarks.isNotEmpty()) Text("Bookmarks: ${node.bookmarks.joinToString()}")
            }
        },
        confirmButton = { TextButton(onClick = onDismiss) { Text("Close") } },
    )
}

@Composable
private fun OperationRow(op: OperationLogEntry, onRestore: () -> Unit) {
    Card(modifier = Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 4.dp).testTag("operation-row-${op.opId}")) {
        Row(
            Modifier.fillMaxWidth().padding(12.dp, 8.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Column {
                Text(op.description, style = MaterialTheme.typography.bodyMedium)
                Text(op.opId, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            TextButton(
                onClick = onRestore,
                modifier = Modifier
                    .testTag("restore-button-${op.opId}")
                    // `docs/accessibility-device-report.md`'s item 1, gap 1
                    // named this Restore button directly: bare "Restore" is
                    // ambiguous when TalkBack focus can land on it without
                    // the sibling operation description ever being
                    // announced — the label now names which operation.
                    .semantics { contentDescription = "Restore to operation: ${op.description}" },
            ) { Text("Restore") }
        }
    }
}
