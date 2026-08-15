package ai.choosh.engine

import java.util.Base64
import kotlinx.coroutines.delay

/**
 * In-memory [ChooshEngine] for previews, UI tests, and early UI development
 * against no real backend. The WebAuthn ceremony always "succeeds" here —
 * the real Credential Manager call still happens in
 * [ai.choosh.connection.ConnectionScreen] (this fake only stands in for the
 * server round-trip either side of it), so the passkey UI path is exercised
 * for real even though this pass doesn't wire it to a live `choosh-relayd`.
 *
 * The devhost/Project/Workspace fixture data here deliberately includes a
 * mix of online/offline hosts and at least one attention-needing workspace
 * so the fleet drawer's every row state (per docs/specs/android-navigation.md)
 * is exercisable without a real backend.
 */
class FakeChooshEngine : ChooshEngine {
    private var connected = false

    // Mutable M3 state: `workspaceOpUndo`/`workspaceOpRestore` actually
    // change what a subsequent `workspaceLog`/`workspaceOpLog` call
    // returns, so a ViewModel/UI exercised against this fake genuinely
    // demonstrates the M3 exit criterion ("the change graph updates to
    // reflect [an undo] within one refresh cycle") rather than only
    // returning static fixtures.
    private var opCounter = 3
    private val opLog = mutableListOf(
        OperationLogEntry("op-3", "merge A and B", "2026-08-15T00:00:03Z", "2026-08-15T00:00:03Z", mapOf("user" to "njr@devhost")),
        OperationLogEntry("op-2", "edit from B", "2026-08-15T00:00:02Z", "2026-08-15T00:00:02Z", mapOf("user" to "agent-b@devhost")),
        OperationLogEntry("op-1", "edit from A", "2026-08-15T00:00:01Z", "2026-08-15T00:00:01Z", mapOf("user" to "agent-a@devhost")),
    )
    private var workingCopyDescription = "merge A and B\n"

    override suspend fun webauthnRegisterStart(): String {
        delay(FAKE_LATENCY_MS)
        return """{"challenge":"fake-challenge","rp":{"id":"choosh.local"}}"""
    }

    override suspend fun webauthnRegisterFinish(credentialJson: String): WebauthnResult {
        delay(FAKE_LATENCY_MS)
        return WebauthnResult.Success(sessionCredential = "fake-session-credential")
    }

    override suspend fun webauthnLoginStart(): String {
        delay(FAKE_LATENCY_MS)
        return """{"challenge":"fake-challenge"}"""
    }

    override suspend fun webauthnLoginFinish(credentialJson: String): WebauthnResult {
        delay(FAKE_LATENCY_MS)
        return WebauthnResult.Success(sessionCredential = "fake-session-credential")
    }

    override suspend fun connect(sessionCredential: String): Boolean {
        delay(FAKE_LATENCY_MS)
        connected = true
        return true
    }

    override suspend fun listDevhosts(): List<DevHostPresence> {
        delay(FAKE_LATENCY_MS)
        check(connected) { "listDevhosts() called before connect() succeeded" }
        return FIXTURE_DEVHOSTS
    }

    override suspend fun registerFcmToken(fcmToken: String): Boolean {
        delay(FAKE_LATENCY_MS)
        return connected
    }

    /**
     * A conflicted 2-parent merge exactly per M3's exit criterion, plus a
     * rename+content-change (both `oldPath != newPath`) and a binary file
     * so [ai.choosh.jj.JjDiffScreen]/[ai.choosh.jj.JjChangeGraphScreen] have
     * every shape their real hostd-computed counterpart can produce, even
     * without a real backend.
     */
    override suspend fun workspaceDiff(deviceId: String, workspaceId: String, from: String?, to: String?): List<DiffFileEntry> {
        delay(FAKE_LATENCY_MS)
        return FIXTURE_DIFF
    }

    override suspend fun workspaceLog(deviceId: String, workspaceId: String, revset: String?, limit: Int): List<ChangeGraphNode> {
        delay(FAKE_LATENCY_MS)
        return listOf(
            ChangeGraphNode(
                changeId = "change-merge",
                commitId = "commit-merge",
                description = workingCopyDescription,
                author = "agent-a <agent-a@choosh.ai>",
                parentChangeIds = listOf("change-a", "change-b"),
                isWorkingCopy = true,
                bookmarks = emptyList(),
            ),
            ChangeGraphNode(
                changeId = "change-a",
                commitId = "commit-a",
                description = "edit from A\n",
                author = "agent-a <agent-a@choosh.ai>",
                parentChangeIds = listOf("change-root"),
                isWorkingCopy = false,
                bookmarks = emptyList(),
            ),
            ChangeGraphNode(
                changeId = "change-b",
                commitId = "commit-b",
                description = "edit from B\n",
                author = "agent-b <agent-b@choosh.ai>",
                parentChangeIds = listOf("change-root"),
                isWorkingCopy = false,
                bookmarks = emptyList(),
            ),
            ChangeGraphNode(
                changeId = "change-root",
                commitId = "commit-root",
                description = "init\n",
                author = "njr <njr@choosh.ai>",
                parentChangeIds = emptyList(),
                isWorkingCopy = false,
                bookmarks = listOf("main"),
            ),
        )
    }

    override suspend fun workspaceOpLog(deviceId: String, workspaceId: String, limit: Int): List<OperationLogEntry> {
        delay(FAKE_LATENCY_MS)
        return opLog.toList()
    }

    override suspend fun workspaceOpUndo(deviceId: String, workspaceId: String, opId: String): String {
        delay(FAKE_LATENCY_MS)
        opCounter += 1
        val newOpId = "op-$opCounter"
        opLog.add(0, OperationLogEntry(newOpId, "undo $opId", "2026-08-15T00:01:0${opCounter}Z", "2026-08-15T00:01:0${opCounter}Z", mapOf("user" to "njr@devhost")))
        workingCopyDescription = "merge A and B (undone: $opId)\n"
        return newOpId
    }

    override suspend fun workspaceOpRestore(deviceId: String, workspaceId: String, opId: String): String {
        delay(FAKE_LATENCY_MS)
        opCounter += 1
        val newOpId = "op-$opCounter"
        opLog.add(0, OperationLogEntry(newOpId, "restore to $opId", "2026-08-15T00:01:0${opCounter}Z", "2026-08-15T00:01:0${opCounter}Z", mapOf("user" to "njr@devhost")))
        workingCopyDescription = "merge A and B\n"
        return newOpId
    }

    override suspend fun workspaceStatus(deviceId: String, workspaceId: String): WorkspaceStatus {
        delay(FAKE_LATENCY_MS)
        return WorkspaceStatus(
            changed = listOf(
                ChangedPath("app/src/Old.kt", ChangeKind.MODIFIED),
                ChangedPath("README.md", ChangeKind.ADDED),
                ChangedPath("a.txt", ChangeKind.MODIFIED),
            ),
            conflicted = listOf("a.txt"),
        )
    }

    /**
     * A realistic in-memory document store, keyed by
     * `deviceId|workspaceId|path` — round-trips real revisions and real
     * `Stale`/`Rejected` outcomes rather than a no-op stub, so
     * [ai.choosh.sourceeditor.SourceEditorViewModel] tests (and Compose
     * previews) can exercise every one of editor-protocol.md's five save
     * states against this fake without a real backend.
     */
    private val documents = mutableMapOf(
        documentKey(FIXTURE_DEVICE_ID, FIXTURE_WORKSPACE_ID, "README.md") to
            FakeDocument(contentBase64 = base64Of("# Choosh\n\nA fleet of workspaces, always reachable.\n"), revision = 1),
    )

    /** Set by a test to exercise [DocumentOpenResult.Offline]/[DocumentSaveResult.Offline] without touching [connected]. */
    var simulateOffline: Boolean = false

    /**
     * One-shot: when non-null, the next [saveDocument] call returns
     * [DocumentSaveResult.Rejected] with this `(code, message)` instead of
     * actually applying the write, then clears itself. Lets a test exercise
     * a `hostd`-side save rejection (a `host-rpc.md` application error, not
     * a conflict and not connectivity loss) on a document that opened
     * successfully — the [isRejectedPath] convention can't do this alone,
     * since it applies identically to open and save.
     */
    var forceNextSaveRejection: Pair<String, String>? = null

    override suspend fun openDocument(deviceId: String, workspaceId: String, path: String): DocumentOpenResult {
        delay(FAKE_LATENCY_MS)
        if (simulateOffline) return DocumentOpenResult.Offline("fake transport offline")
        if (isRejectedPath(path)) return DocumentOpenResult.Rejected("bound_exceeded", "this file can't be edited here")
        val key = documentKey(deviceId, workspaceId, path)
        val doc = documents.getOrPut(key) { FakeDocument(contentBase64 = base64Of(""), revision = 1) }
        return DocumentOpenResult.Success(
            contentBase64 = doc.contentBase64,
            revision = "rev-${doc.revision}",
            totalSize = Base64.getDecoder().decode(doc.contentBase64).size.toLong(),
        )
    }

    override suspend fun saveDocument(
        deviceId: String,
        workspaceId: String,
        path: String,
        baseRevision: String,
        contentBase64: String,
    ): DocumentSaveResult {
        delay(FAKE_LATENCY_MS)
        if (simulateOffline) return DocumentSaveResult.Offline("fake transport offline")
        if (isRejectedPath(path)) return DocumentSaveResult.Rejected("bound_exceeded", "this file can't be edited here")
        forceNextSaveRejection?.let { (code, message) ->
            forceNextSaveRejection = null
            return DocumentSaveResult.Rejected(code, message)
        }
        val key = documentKey(deviceId, workspaceId, path)
        val doc = documents.getOrPut(key) { FakeDocument(contentBase64 = base64Of(""), revision = 1) }
        if ("rev-${doc.revision}" != baseRevision) {
            return DocumentSaveResult.Stale(currentRevision = "rev-${doc.revision}", currentContentBase64 = doc.contentBase64)
        }
        doc.revision += 1
        doc.contentBase64 = contentBase64
        return DocumentSaveResult.Success(revision = "rev-${doc.revision}")
    }

    /**
     * Deliberately provokes a concurrent-writer conflict on the next save
     * against `path`, per editor-protocol.md's "Concurrent writers" note —
     * lets a test/preview simulate an agent or laptop Zed session writing
     * the same path between this app's open and its next save, without a
     * real second writer.
     */
    fun simulateConcurrentWrite(deviceId: String, workspaceId: String, path: String, newContent: String) {
        val key = documentKey(deviceId, workspaceId, path)
        val doc = documents.getOrPut(key) { FakeDocument(contentBase64 = base64Of(""), revision = 1) }
        doc.revision += 1
        doc.contentBase64 = base64Of(newContent)
    }

    override fun close() {
        connected = false
    }

    private data class FakeDocument(var contentBase64: String, var revision: Int)

    companion object {
        private const val FAKE_LATENCY_MS = 120L
        const val FIXTURE_DEVICE_ID = "dev-mbp-home"
        const val FIXTURE_WORKSPACE_ID = "ws-choosh-app"

        private fun documentKey(deviceId: String, workspaceId: String, path: String) = "$deviceId|$workspaceId|$path"
        private fun base64Of(text: String): String = Base64.getEncoder().encodeToString(text.toByteArray(Charsets.UTF_8))

        /** Paths under this convention always come back `Rejected`, standing in for `hostd`'s binary/oversized rejection. */
        private fun isRejectedPath(path: String) = path.endsWith(".bin")

        /**
         * A modify, a rename+content-change (`oldPath != newPath`, real
         * hunks — not a pure rename, which jj only pairs when content is
         * byte-identical), and a binary file, per M3's exit criterion that
         * a rename and a binary file both render correctly.
         */
        val FIXTURE_DIFF: List<DiffFileEntry> = listOf(
            DiffFileEntry.Hunks(
                oldPath = "app/src/Old.kt",
                newPath = "app/src/Old.kt",
                hunks = listOf(
                    DiffHunk(
                        oldStart = 1,
                        oldLines = 3,
                        newStart = 1,
                        newLines = 4,
                        segments = listOf(
                            DiffSegment(DiffSegmentKind.CONTEXT, "fun main() {"),
                            DiffSegment(DiffSegmentKind.REMOVED, "    println(\"old\")"),
                            DiffSegment(DiffSegmentKind.ADDED, "    println(\"new\")"),
                            DiffSegment(DiffSegmentKind.ADDED, "    println(\"and more\")"),
                            DiffSegment(DiffSegmentKind.CONTEXT, "}"),
                        ),
                    ),
                ),
            ),
            DiffFileEntry.Hunks(
                oldPath = "docs/README.old.md",
                newPath = "docs/README.md",
                hunks = listOf(
                    DiffHunk(
                        oldStart = 1,
                        oldLines = 1,
                        newStart = 1,
                        newLines = 2,
                        segments = listOf(
                            DiffSegment(DiffSegmentKind.REMOVED, "# Old title"),
                            DiffSegment(DiffSegmentKind.ADDED, "# New title"),
                            DiffSegment(DiffSegmentKind.ADDED, "Renamed and reworded."),
                        ),
                    ),
                ),
            ),
            DiffFileEntry.Binary(path = "assets/logo.png", status = ChangeKind.MODIFIED, byteSize = 48_213),
        )

        val FIXTURE_DEVHOSTS = listOf(
            DevHostPresence(
                deviceId = "dev-mbp-home",
                alias = "mbp-home",
                platform = "macos",
                accountLabel = null,
                connectionState = ConnectionState.ONLINE,
                lastSeen = "2026-08-14T20:00:00Z",
            ),
            DevHostPresence(
                deviceId = "dev-build-box-large",
                alias = "build-box-large",
                platform = "linux",
                accountLabel = "aws:123456789012",
                connectionState = ConnectionState.ONLINE,
                lastSeen = "2026-08-14T19:58:00Z",
            ),
            DevHostPresence(
                deviceId = "dev-old-cloud-box",
                alias = "old-cloud-box",
                platform = "linux",
                accountLabel = "aws:987654321098",
                connectionState = ConnectionState.OFFLINE,
                lastSeen = "2026-08-10T12:00:00Z",
            ),
        )
    }
}
