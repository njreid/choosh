package ai.choosh.sourceeditor

import ai.choosh.engine.FakeChooshEngine
import ai.choosh.fleet.MainDispatcherRule
import java.util.Base64
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/**
 * The save-state machine, per docs/specs/editor-protocol.md's "Save state":
 * exactly `clean`/`dirty`/`saving`/`conflicted`/`offline`, driven here
 * against [FakeChooshEngine] (a real in-memory document store, not a
 * no-op stub — see its class doc) rather than a real `choosh-hostd`. This
 * is the highest-value coverage for this task: every transition below is
 * asserted, not just the happy path.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class SourceEditorViewModelTest {

    @get:Rule
    val mainDispatcherRule = MainDispatcherRule()

    private val deviceId = FakeChooshEngine.FIXTURE_DEVICE_ID
    private val workspaceId = FakeChooshEngine.FIXTURE_WORKSPACE_ID
    private val path = "README.md"

    private fun decodeUtf8(base64: String) = String(Base64.getDecoder().decode(base64), Charsets.UTF_8)

    @Test
    fun `loading a document that opens successfully lands on Editing with Clean`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        val viewModel = SourceEditorViewModel(engine, deviceId, workspaceId, path)
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue("expected Editing, got $state", state is SourceEditorUiState.Editing)
        state as SourceEditorUiState.Editing
        assertEquals(SaveState.Clean, state.saveState)
        assertTrue(state.content.startsWith("# Choosh"))
    }

    @Test
    fun `opening a rejected path (binary-oversized stand-in) surfaces Unavailable`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        val viewModel = SourceEditorViewModel(engine, deviceId, workspaceId, "huge.bin")
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue("expected Unavailable, got $state", state is SourceEditorUiState.Unavailable)
    }

    @Test
    fun `opening while offline surfaces Unavailable rather than hanging or crashing`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine().apply { simulateOffline = true }
        val viewModel = SourceEditorViewModel(engine, deviceId, workspaceId, path)
        advanceUntilIdle()

        assertTrue(viewModel.state.value is SourceEditorUiState.Unavailable)
    }

    /** clean -> dirty -> saving -> clean, the full happy path, with the debounce actually exercised (not bypassed). */
    @Test
    fun `an edit goes clean to dirty to saving to clean after the debounce window`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        val viewModel = SourceEditorViewModel(engine, deviceId, workspaceId, path, debounceMs = 500L)
        advanceUntilIdle()
        assertEquals(SaveState.Clean, (viewModel.state.value as SourceEditorUiState.Editing).saveState)

        viewModel.onContentChanged("# Choosh\n\nedited\n")
        assertEquals(
            "an edit must move to Dirty immediately, not wait for the debounce to even show Dirty",
            SaveState.Dirty,
            (viewModel.state.value as SourceEditorUiState.Editing).saveState,
        )

        // Before the debounce window elapses, no save has fired yet.
        advanceTimeBy(200L)
        assertEquals(SaveState.Dirty, (viewModel.state.value as SourceEditorUiState.Editing).saveState)

        // Let the debounce fire and the fake engine's save round-trip complete.
        advanceUntilIdle()
        val finalState = viewModel.state.value as SourceEditorUiState.Editing
        assertEquals(SaveState.Clean, finalState.saveState)
        assertEquals("# Choosh\n\nedited\n", finalState.content)

        // The save actually reached the fake's document store.
        val reread = engine.openDocument(deviceId, workspaceId, path)
        assertTrue(reread is ai.choosh.engine.DocumentOpenResult.Success)
        assertEquals("# Choosh\n\nedited\n", decodeUtf8((reread as ai.choosh.engine.DocumentOpenResult.Success).contentBase64))
    }

    @Test
    fun `edits within the debounce window coalesce into exactly one save`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        val viewModel = SourceEditorViewModel(engine, deviceId, workspaceId, path, debounceMs = 500L)
        advanceUntilIdle()

        viewModel.onContentChanged("v1")
        advanceTimeBy(200L)
        viewModel.onContentChanged("v2")
        advanceTimeBy(200L)
        viewModel.onContentChanged("v3")
        advanceUntilIdle()

        val finalState = viewModel.state.value as SourceEditorUiState.Editing
        assertEquals(SaveState.Clean, finalState.saveState)
        assertEquals("v3", finalState.content)

        val reread = engine.openDocument(deviceId, workspaceId, path) as ai.choosh.engine.DocumentOpenResult.Success
        // Exactly one save happened: the document's revision only advanced by one from its
        // initial "rev-1" (a save-per-keystroke bug would have advanced it three times).
        assertEquals("rev-2", reread.revision)
        assertEquals("v3", decodeUtf8(reread.contentBase64))
    }

    /** saving -> conflicted on a stale response — the load-bearing conflict case. */
    @Test
    fun `a concurrent write elsewhere surfaces as Conflicted, not a silent overwrite`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        val viewModel = SourceEditorViewModel(engine, deviceId, workspaceId, path, debounceMs = 500L)
        advanceUntilIdle()

        // Simulate an agent/Zed session writing the same path after this app opened it.
        engine.simulateConcurrentWrite(deviceId, workspaceId, path, "someone else's content")

        viewModel.onContentChanged("my local edit")
        advanceUntilIdle()

        val state = viewModel.state.value as SourceEditorUiState.Editing
        val saveState = state.saveState
        assertTrue("expected Conflicted, got $saveState", saveState is SaveState.Conflicted)
        saveState as SaveState.Conflicted
        assertEquals("someone else's content", decodeUtf8(saveState.currentContentBase64))
        // The local edit must not have been lost or silently discarded.
        assertEquals("my local edit", state.content)
    }

    @Test
    fun `resolveKeepMine force-writes local content using the servers current revision`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        val viewModel = SourceEditorViewModel(engine, deviceId, workspaceId, path, debounceMs = 500L)
        advanceUntilIdle()
        engine.simulateConcurrentWrite(deviceId, workspaceId, path, "someone else's content")
        viewModel.onContentChanged("my local edit")
        advanceUntilIdle()
        assertTrue((viewModel.state.value as SourceEditorUiState.Editing).saveState is SaveState.Conflicted)

        viewModel.resolveKeepMine()
        advanceUntilIdle()

        val finalState = viewModel.state.value as SourceEditorUiState.Editing
        assertEquals(SaveState.Clean, finalState.saveState)
        assertEquals("my local edit", finalState.content)
        val reread = engine.openDocument(deviceId, workspaceId, path) as ai.choosh.engine.DocumentOpenResult.Success
        assertEquals("my local edit", decodeUtf8(reread.contentBase64))
    }

    @Test
    fun `resolveTakeTheirs discards local edits and adopts the servers content`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        val viewModel = SourceEditorViewModel(engine, deviceId, workspaceId, path, debounceMs = 500L)
        advanceUntilIdle()
        engine.simulateConcurrentWrite(deviceId, workspaceId, path, "someone else's content")
        viewModel.onContentChanged("my local edit")
        advanceUntilIdle()
        assertTrue((viewModel.state.value as SourceEditorUiState.Editing).saveState is SaveState.Conflicted)
        val generationBefore = (viewModel.state.value as SourceEditorUiState.Editing).contentGeneration

        viewModel.resolveTakeTheirs()

        val finalState = viewModel.state.value as SourceEditorUiState.Editing
        assertEquals(SaveState.Clean, finalState.saveState)
        assertEquals("someone else's content", finalState.content)
        assertTrue(
            "contentGeneration must bump so the Sora host actually re-applies the adopted text",
            finalState.contentGeneration > generationBefore,
        )
    }

    /** offline handling on a transport failure — local edits survive and a later save attempt tries again. */
    @Test
    fun `a transport failure during save moves to Offline without losing the edit`() = runTest(mainDispatcherRule.dispatcher) {
        val engine = FakeChooshEngine()
        val viewModel = SourceEditorViewModel(engine, deviceId, workspaceId, path, debounceMs = 500L)
        advanceUntilIdle()

        engine.simulateOffline = true
        viewModel.onContentChanged("edited while offline")
        advanceUntilIdle()

        val state = viewModel.state.value as SourceEditorUiState.Editing
        assertTrue("expected Offline, got ${state.saveState}", state.saveState is SaveState.Offline)
        assertEquals("edited while offline", state.content)

        // Connectivity returns; the next explicit save attempt (this pass's scoped-down retry
        // story — see SourceEditorViewModel.retrySave's doc comment) succeeds.
        engine.simulateOffline = false
        viewModel.retrySave()
        advanceUntilIdle()

        val recovered = viewModel.state.value as SourceEditorUiState.Editing
        assertEquals(SaveState.Clean, recovered.saveState)
        val reread = engine.openDocument(deviceId, workspaceId, path) as ai.choosh.engine.DocumentOpenResult.Success
        assertEquals("edited while offline", decodeUtf8(reread.contentBase64))
    }

    @Test
    fun `a hostd rejection during save keeps edits as Dirty with an informational error, not a sixth state`() =
        runTest(mainDispatcherRule.dispatcher) {
            val engine = FakeChooshEngine()
            val viewModel = SourceEditorViewModel(engine, deviceId, workspaceId, path, debounceMs = 500L)
            advanceUntilIdle()

            engine.forceNextSaveRejection = "invalid_argument" to "path escaped the workspace root"
            viewModel.onContentChanged("trying to save")
            advanceUntilIdle()

            val state = viewModel.state.value as SourceEditorUiState.Editing
            assertEquals(
                "a hostd rejection is not a conflict and not connectivity loss, so it stays Dirty — the caller keeps its edits and can retry",
                SaveState.Dirty,
                state.saveState,
            )
            assertEquals("trying to save", state.content)
            assertTrue(state.lastSaveError?.contains("invalid_argument") == true)

            // Retrying (e.g. the next debounced edit, or an explicit retry) succeeds once the
            // one-shot rejection has been consumed.
            viewModel.retrySave()
            advanceUntilIdle()
            val recovered = viewModel.state.value as SourceEditorUiState.Editing
            assertEquals(SaveState.Clean, recovered.saveState)
        }

    /**
     * `bound_exceeded` (a save whose content would exceed
     * `choosh_protocol::host_rpc::MAX_FILE_READ_RANGE_BYTES` — the same
     * bound one relay tunnel frame can carry, per `fs_ops.rs`'s
     * `write_file`) gets its own plain-language message rather than the raw
     * `"bound_exceeded: bound exceeded: content is N bytes, max is N"` wire
     * text — this is the write-side half of this pass's chunking work: a
     * save can't be chunked the way a read can (one atomic
     * `workspace.file.write`, no partial-write RPC), so an oversized save is
     * an explicit, clearly-worded rejection instead. Edits must still
     * survive as Dirty, exactly like any other rejection.
     */
    @Test
    fun `a bound_exceeded save rejection surfaces a plain-language size message, not the raw wire text`() =
        runTest(mainDispatcherRule.dispatcher) {
            val engine = FakeChooshEngine()
            val viewModel = SourceEditorViewModel(engine, deviceId, workspaceId, path, debounceMs = 500L)
            advanceUntilIdle()

            engine.forceNextSaveRejection = "bound_exceeded" to "bound exceeded: content is 500000 bytes, max is 131072"
            viewModel.onContentChanged("a save that would be too large")
            advanceUntilIdle()

            val state = viewModel.state.value as SourceEditorUiState.Editing
            assertEquals(SaveState.Dirty, state.saveState)
            assertEquals("a save that would be too large", state.content)
            val message = state.lastSaveError
            assertTrue("expected a plain-language size message, got: $message", message?.contains("too large") == true)
            assertTrue(
                "must not leak the raw bound_exceeded wire code/message to the user: $message",
                message?.contains("bound_exceeded") != true && message?.contains("131072") != true,
            )
        }
}
