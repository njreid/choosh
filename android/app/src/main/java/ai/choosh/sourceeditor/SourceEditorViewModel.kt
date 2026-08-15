package ai.choosh.sourceeditor

import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.DocumentOpenResult
import ai.choosh.engine.DocumentSaveResult
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import java.util.Base64
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/**
 * Save state per docs/specs/editor-protocol.md's "Save state": "Saving is
 * debounced but state is always explicit, one of: `clean`, `dirty`,
 * `saving`, `conflicted`, `offline`. There is no `staged` state." Every
 * [SourceEditorUiState.Editing] carries exactly one of these five — the UI
 * renders all five explicitly, never inferring/hiding one.
 */
sealed interface SaveState {
    data object Clean : SaveState
    data object Dirty : SaveState
    data object Saving : SaveState

    /**
     * A real conflict: the file changed on disk since [baseRevision] was
     * captured (`WorkspaceFileWriteStale`). The user MUST explicitly
     * resolve this — [SourceEditorViewModel.resolveKeepMine] or
     * [SourceEditorViewModel.resolveTakeTheirs] — never a silent overwrite
     * in either direction, per editor-protocol.md's "Persistence".
     */
    data class Conflicted(val currentRevision: String, val currentContentBase64: String) : SaveState

    /**
     * A transport-level failure (`CallError`, not an application
     * response) — editor-protocol.md's "if `call_rpc` fails ... the
     * document should move to an explicit `offline` state rather than
     * silently losing edits". Local edits are untouched; the next explicit
     * save attempt (another edit's debounce firing, or
     * [SourceEditorViewModel.retrySave]) tries again.
     */
    data class Offline(val message: String) : SaveState
}

sealed interface SourceEditorUiState {
    data object Loading : SourceEditorUiState

    /**
     * `hostd` rejected the read outright — a binary file, an oversized
     * file, or any other `host-rpc.md` application error. `hostd` enforces
     * this threshold host-side per editor-protocol.md's "Limits"; this is
     * just surfacing whatever it returned as a clear "can't edit this
     * here" state, not a client-side duplicate of that check.
     */
    data class Unavailable(val message: String) : SourceEditorUiState

    data class Editing(
        val content: String,
        val saveState: SaveState,
        /**
         * Bumped only when [content] was replaced wholesale (initial load,
         * or [SourceEditorViewModel.resolveTakeTheirs]) — never by a normal
         * keystroke. The Sora host only re-calls `CodeEditor.setText` when
         * this changes, so the editor's own edits are never fought/reset
         * mid-keystroke.
         */
        val contentGeneration: Int = 0,
        /** Set after a failed (non-stale, non-offline) save attempt — informational, not a sixth save state. */
        val lastSaveError: String? = null,
    ) : SourceEditorUiState
}

/**
 * Owns one open document's save-state machine, per
 * docs/specs/editor-protocol.md. Debounces Sora's edit stream
 * ([onContentChanged]) into full-content `workspace.file.write` calls —
 * V1's deliberate scope reduction (see
 * `choosh_protocol::host_rpc::RpcRequest::WorkspaceFileWrite`'s doc
 * comment): never an incremental range-edit list.
 */
class SourceEditorViewModel(
    private val engine: ChooshEngine,
    private val deviceId: String,
    private val workspaceId: String,
    private val path: String,
    /**
     * Delay between the last edit and firing `workspace.file.write`. 800ms:
     * long enough that an ordinary typing burst coalesces into one write
     * instead of one per keystroke, short enough that a save follows a
     * pause quickly — within editor-protocol.md's suggested 500ms-2s
     * window (this pass's judgment call, not a wire contract).
     */
    private val debounceMs: Long = 800L,
) : ViewModel() {
    private val _state = MutableStateFlow<SourceEditorUiState>(SourceEditorUiState.Loading)
    val state: StateFlow<SourceEditorUiState> = _state.asStateFlow()

    private var baseRevision: String? = null
    private var pendingContentBase64: String? = null
    private var debounceJob: Job? = null

    /** True if an edit arrived while a save was already in flight — triggers exactly one more save once it completes. */
    private var dirtyDuringSave = false

    init {
        load()
    }

    fun load() {
        debounceJob?.cancel()
        dirtyDuringSave = false
        _state.value = SourceEditorUiState.Loading
        viewModelScope.launch {
            when (val result = engine.openDocument(deviceId, workspaceId, path)) {
                is DocumentOpenResult.Success -> {
                    baseRevision = result.revision
                    pendingContentBase64 = result.contentBase64
                    _state.value = SourceEditorUiState.Editing(
                        content = decodeUtf8(result.contentBase64),
                        saveState = SaveState.Clean,
                        contentGeneration = 0,
                    )
                }
                is DocumentOpenResult.Rejected -> _state.value = SourceEditorUiState.Unavailable(result.message)
                is DocumentOpenResult.Offline -> _state.value = SourceEditorUiState.Unavailable("Offline: ${result.message}")
            }
        }
    }

    /** Called on every Sora content-change event; the actual save fires after [debounceMs] of no further edits. */
    fun onContentChanged(newContent: String) {
        val editing = _state.value as? SourceEditorUiState.Editing ?: return
        pendingContentBase64 = encodeUtf8(newContent)

        when (editing.saveState) {
            is SaveState.Saving -> {
                // A save is already in flight against the previous content — don't fire a second
                // concurrent write; saveNow() will notice dirtyDuringSave once this one resolves.
                dirtyDuringSave = true
                _state.value = editing.copy(content = newContent)
            }
            is SaveState.Conflicted -> {
                // Never auto-save over an unresolved conflict — keep tracking what the user is
                // typing so resolveKeepMine() saves their latest text, but wait for an explicit
                // resolution before any workspace.file.write fires.
                _state.value = editing.copy(content = newContent)
            }
            else -> {
                _state.value = editing.copy(content = newContent, saveState = SaveState.Dirty, lastSaveError = null)
                scheduleDebouncedSave()
            }
        }
    }

    private fun scheduleDebouncedSave() {
        debounceJob?.cancel()
        debounceJob = viewModelScope.launch {
            delay(debounceMs)
            saveNow()
        }
    }

    private suspend fun saveNow() {
        val editing = _state.value as? SourceEditorUiState.Editing ?: return
        val revision = baseRevision ?: return
        val content = pendingContentBase64 ?: return
        dirtyDuringSave = false
        _state.value = editing.copy(saveState = SaveState.Saving)

        when (val result = engine.saveDocument(deviceId, workspaceId, path, revision, content)) {
            is DocumentSaveResult.Success -> {
                baseRevision = result.revision
                val current = _state.value as? SourceEditorUiState.Editing ?: return
                if (dirtyDuringSave) {
                    dirtyDuringSave = false
                    _state.value = current.copy(saveState = SaveState.Dirty)
                    scheduleDebouncedSave()
                } else {
                    _state.value = current.copy(saveState = SaveState.Clean, lastSaveError = null)
                }
            }
            is DocumentSaveResult.Stale -> {
                val current = _state.value as? SourceEditorUiState.Editing ?: return
                _state.value = current.copy(saveState = SaveState.Conflicted(result.currentRevision, result.currentContentBase64))
            }
            is DocumentSaveResult.Offline -> {
                val current = _state.value as? SourceEditorUiState.Editing ?: return
                _state.value = current.copy(saveState = SaveState.Offline(result.message))
            }
            is DocumentSaveResult.Rejected -> {
                // Not a conflict and not connectivity loss — keep the edits (Dirty, not Clean) and
                // surface why, rather than inventing a sixth save state for an application error.
                //
                // "bound_exceeded" (choosh_protocol::host_rpc::MAX_FILE_READ_RANGE_BYTES, also
                // workspace.file.write's own max content size — see fs_ops.rs's write_file) gets
                // a dedicated, plain-language message rather than the raw "code: message" wire
                // text: that bound exists purely so one request fits inside one relay tunnel
                // frame, a detail meaningless to a user staring at a failed save. Sora keeps the
                // edits locally either way (Dirty, not Clean/lost) — this is a UX call, not a data-
                // loss one: a save this large simply cannot go over the phone-to-relay tunnel in
                // one shot, and there is no chunking to fall back to for a single atomic write
                // (unlike the read side's workspace.file.read, which can and does chunk — see
                // Engine::read_whole_file in the Rust bridge).
                val current = _state.value as? SourceEditorUiState.Editing ?: return
                val message = if (result.code == "bound_exceeded") {
                    "This file is too large to save over the phone connection (over $MAX_SAVE_SIZE_DESCRIPTION). " +
                        "Your edits are kept here — try trimming the file, or save this change from a laptop instead."
                } else {
                    "${result.code}: ${result.message}"
                }
                _state.value = current.copy(saveState = SaveState.Dirty, lastSaveError = message)
            }
        }
    }

    /**
     * Manual retry from [SaveState.Offline] (or a [SaveState.Dirty] left by
     * a failed save) — this pass's scoped-down reconnect story: retry only
     * on an explicit save attempt (this, or the next debounced edit), not a
     * background auto-retry loop. See this class's doc comment.
     */
    fun retrySave() {
        val editing = _state.value as? SourceEditorUiState.Editing ?: return
        if (editing.saveState !is SaveState.Offline && editing.saveState !is SaveState.Dirty) return
        debounceJob?.cancel()
        _state.value = editing.copy(saveState = SaveState.Dirty)
        viewModelScope.launch { saveNow() }
    }

    /** Conflict resolution: force-write the user's local content, using the server's current revision as the new base. */
    fun resolveKeepMine() {
        val editing = _state.value as? SourceEditorUiState.Editing ?: return
        val conflicted = editing.saveState as? SaveState.Conflicted ?: return
        baseRevision = conflicted.currentRevision
        debounceJob?.cancel()
        _state.value = editing.copy(saveState = SaveState.Dirty)
        viewModelScope.launch { saveNow() }
    }

    /** Conflict resolution: discard local edits, adopt the server's current content. */
    fun resolveTakeTheirs() {
        val editing = _state.value as? SourceEditorUiState.Editing ?: return
        val conflicted = editing.saveState as? SaveState.Conflicted ?: return
        debounceJob?.cancel()
        baseRevision = conflicted.currentRevision
        pendingContentBase64 = conflicted.currentContentBase64
        _state.value = editing.copy(
            content = decodeUtf8(conflicted.currentContentBase64),
            saveState = SaveState.Clean,
            contentGeneration = editing.contentGeneration + 1,
        )
    }

    override fun onCleared() {
        debounceJob?.cancel()
    }

    companion object {
        /**
         * Human-readable stand-in for
         * `choosh_protocol::host_rpc::MAX_FILE_READ_RANGE_BYTES` (128 KiB) in
         * the "bound_exceeded" save-rejection message above. Deliberately a
         * plain string, not threaded down from the Rust constant itself —
         * this crosses the JNI boundary as an untyped `(code, message)` pair
         * (see [ai.choosh.NativeChooshEngine]'s `decodeDocumentSaveResult`),
         * so there is no real value the wire already carries here worth
         * parsing back out of `result.message`; a fixed, occasionally-
         * reviewed description is simpler and no less correct than false
         * precision extracted from a string meant for logs, not arithmetic.
         */
        private const val MAX_SAVE_SIZE_DESCRIPTION = "128 KB"

        /**
         * Explicit UTF-8, never a platform-default charset — per
         * editor-protocol.md's "Encoding and line endings MUST round-trip
         * byte-identical", content crosses this boundary as opaque UTF-8
         * bytes, never normalized.
         */
        fun decodeUtf8(base64: String): String = String(Base64.getDecoder().decode(base64), Charsets.UTF_8)

        fun encodeUtf8(text: String): String = Base64.getEncoder().encodeToString(text.toByteArray(Charsets.UTF_8))
    }
}
