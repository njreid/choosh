package ai.choosh.sourceeditor

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import io.github.rosemoe.sora.event.ContentChangeEvent
import io.github.rosemoe.sora.widget.CodeEditor
import java.util.concurrent.atomic.AtomicBoolean

/**
 * The `SourceEditor` pinned item, per docs/specs/editor-protocol.md and
 * docs/specs/android-navigation.md's pinned-kinds table: Sora embedded via
 * `AndroidView`, a save-state indicator always showing one of the five
 * explicit states, and conflict-resolution UI when `conflicted` — never a
 * silent overwrite.
 */
@Composable
fun SourceEditorScreen(viewModel: SourceEditorViewModel, path: String, onBack: () -> Unit, modifier: Modifier = Modifier) {
    val state by viewModel.state.collectAsState()

    Column(modifier = modifier.fillMaxSize()) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton(onClick = onBack) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
            }
            Text(path, style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
            if (state is SourceEditorUiState.Editing) {
                SaveStateIndicator((state as SourceEditorUiState.Editing).saveState)
            }
        }

        when (val current = state) {
            is SourceEditorUiState.Loading -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                CircularProgressIndicator(modifier = Modifier.testTag("source-editor-loading"))
            }

            is SourceEditorUiState.Unavailable -> Box(
                Modifier.fillMaxSize().padding(24.dp),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    "This file can't be edited here.\n${current.message}",
                    style = MaterialTheme.typography.bodyLarge,
                    modifier = Modifier.testTag("source-editor-unavailable"),
                )
            }

            is SourceEditorUiState.Editing -> {
                val saveState = current.saveState
                if (saveState is SaveState.Conflicted) {
                    ConflictBanner(onKeepMine = viewModel::resolveKeepMine, onTakeTheirs = viewModel::resolveTakeTheirs)
                }
                if (saveState is SaveState.Offline) {
                    OfflineBanner(message = saveState.message, onRetry = viewModel::retrySave)
                }
                if (current.lastSaveError != null) {
                    ErrorBanner(message = current.lastSaveError, onRetry = viewModel::retrySave)
                }
                SoraEditorHost(
                    content = current.content,
                    contentGeneration = current.contentGeneration,
                    onContentChanged = viewModel::onContentChanged,
                    modifier = Modifier.fillMaxSize().testTag("source-editor-view"),
                )
            }
        }
    }
}

/**
 * Always shows exactly one of editor-protocol.md's five save states —
 * never inferred/hidden. `testTag`s carry the state name so UI tests (and
 * a screenshot) can assert which one is showing.
 */
@Composable
private fun SaveStateIndicator(saveState: SaveState) {
    val (label, color) = when (saveState) {
        is SaveState.Clean -> "Saved" to MaterialTheme.colorScheme.primary
        is SaveState.Dirty -> "Unsaved" to MaterialTheme.colorScheme.onSurfaceVariant
        is SaveState.Saving -> "Saving…" to MaterialTheme.colorScheme.primary
        is SaveState.Conflicted -> "Conflict" to MaterialTheme.colorScheme.error
        is SaveState.Offline -> "Offline" to MaterialTheme.colorScheme.error
    }
    Row(
        modifier = Modifier
            .testTag("save-state-${saveStateTag(saveState)}")
            .semantics { contentDescription = "Save state: $label" }
            .padding(horizontal = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(modifier = Modifier.size(8.dp).background(color = color, shape = CircleShape))
        Text(label, style = MaterialTheme.typography.labelMedium, color = color, modifier = Modifier.padding(start = 6.dp))
    }
}

private fun saveStateTag(saveState: SaveState): String = when (saveState) {
    is SaveState.Clean -> "clean"
    is SaveState.Dirty -> "dirty"
    is SaveState.Saving -> "saving"
    is SaveState.Conflicted -> "conflicted"
    is SaveState.Offline -> "offline"
}

/**
 * Required by editor-protocol.md's "Persistence": a stale write MUST NOT
 * be silently resolved in either direction — the user picks.
 */
@Composable
private fun ConflictBanner(onKeepMine: () -> Unit, onTakeTheirs: () -> Unit) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.errorContainer)
            .padding(12.dp)
            .testTag("conflict-banner"),
    ) {
        Text(
            "This file changed elsewhere since you opened it. Choose which version to keep.",
            color = MaterialTheme.colorScheme.onErrorContainer,
            style = MaterialTheme.typography.bodyMedium,
        )
        Row(modifier = Modifier.padding(top = 8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(onClick = onKeepMine, modifier = Modifier.testTag("conflict-keep-mine")) { Text("Keep mine") }
            OutlinedButton(onClick = onTakeTheirs, modifier = Modifier.testTag("conflict-take-theirs")) { Text("Take theirs") }
        }
    }
}

@Composable
private fun OfflineBanner(message: String, onRetry: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.errorContainer)
            .padding(12.dp)
            .testTag("offline-banner"),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Text("Offline: edits are kept locally. $message", color = MaterialTheme.colorScheme.onErrorContainer)
        OutlinedButton(onClick = onRetry, modifier = Modifier.testTag("offline-retry")) { Text("Retry") }
    }
}

@Composable
private fun ErrorBanner(message: String, onRetry: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.errorContainer)
            .padding(12.dp)
            .testTag("save-error-banner"),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Text("Save failed: $message", color = MaterialTheme.colorScheme.onErrorContainer)
        OutlinedButton(onClick = onRetry, modifier = Modifier.testTag("save-error-retry")) { Text("Retry") }
    }
}

/**
 * Hosts Sora's `CodeEditor` via `AndroidView`, per docs/specs/android-navigation.md's
 * "Heavy views are retained" note — one `CodeEditor` instance for this
 * screen's lifetime, released on disposal.
 *
 * **Suppression, not translation.** V1 only needs to know "the user edited
 * something," not the precise UTF-16 range Sora reports (that range-edit
 * machinery is this project's superseded SORA-EDIT-SPIKE.md design — V1's
 * wire protocol only ever sends full content, per
 * `choosh_protocol::host_rpc::RpcRequest::WorkspaceFileWrite`'s doc
 * comment). So this host just grabs the full current text on every
 * `ContentChangeEvent` and forwards it to [onContentChanged] — except
 * while `suppressChangeEvents` is set, which is true only for the
 * duration of this composable's own programmatic `setText` calls (initial
 * load, or a generation bump from `resolveTakeTheirs`), so a
 * self-triggered projection is never mistaken for a user edit. Sora
 * dispatches `ContentChangeEvent` synchronously within `setText`
 * (docs/SORA-EDIT-SPIKE.md's verified `ContentChangeEvent` contract), so a
 * plain boolean flag set immediately around the call is sufficient — no
 * generation/token counter needed here the way an incremental range-edit
 * design would require.
 */
@Composable
private fun SoraEditorHost(
    content: String,
    contentGeneration: Int,
    onContentChanged: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val onContentChangedState = rememberUpdatedState(onContentChanged)
    val editorRef = remember { mutableStateOf<CodeEditor?>(null) }
    val lastAppliedGeneration = remember { mutableStateOf(-1) }
    val suppressChangeEvents = remember { AtomicBoolean(false) }

    DisposableEffect(Unit) {
        onDispose { editorRef.value?.release() }
    }

    AndroidView(
        modifier = modifier,
        factory = { context ->
            CodeEditor(context).also { editor ->
                editorRef.value = editor
                suppressChangeEvents.set(true)
                editor.setText(content)
                suppressChangeEvents.set(false)
                lastAppliedGeneration.value = contentGeneration
                editor.subscribeEvent(ContentChangeEvent::class.java) { _, _ ->
                    if (!suppressChangeEvents.get()) {
                        onContentChangedState.value.invoke(editor.text.toString())
                    }
                }
            }
        },
        update = { editor ->
            if (lastAppliedGeneration.value != contentGeneration) {
                suppressChangeEvents.set(true)
                editor.setText(content)
                suppressChangeEvents.set(false)
                lastAppliedGeneration.value = contentGeneration
            }
        },
    )
}
