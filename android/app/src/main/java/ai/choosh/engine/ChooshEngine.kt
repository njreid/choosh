package ai.choosh.engine

import kotlinx.serialization.Serializable

/**
 * The Kotlin-facing contract for the Rust engine, per DESIGN.md's "Rust owns
 * durable state; views are projections" principle. Every method's wire
 * payload is JSON matching `choosh_protocol::relay`'s shared types
 * (docs/specs/relay-protocol.md, docs/specs/auth-and-enrollment.md) — this
 * interface just gives Kotlin callers a typed surface over that JSON rather
 * than passing raw strings around application code.
 *
 * Two implementations: [NativeChooshEngine] (real, JNI-backed) and
 * [FakeChooshEngine] (in-memory, for previews/tests/early UI development).
 * The composition root in [ai.choosh.ChooshApp] is the single place that
 * chooses which one the rest of the app sees.
 */
interface ChooshEngine {
    /** Starts a WebAuthn passkey registration ceremony. Returns creation-options JSON. */
    suspend fun webauthnRegisterStart(): String

    /** Finishes registration with the Credential Manager response JSON; returns a [WebauthnResult]. */
    suspend fun webauthnRegisterFinish(credentialJson: String): WebauthnResult

    /** Starts a WebAuthn passkey login (assertion) ceremony. Returns request-options JSON. */
    suspend fun webauthnLoginStart(): String

    /** Finishes login with the Credential Manager response JSON; returns a [WebauthnResult]. */
    suspend fun webauthnLoginFinish(credentialJson: String): WebauthnResult

    /** Opens the persistent relay connection using a stored session credential. */
    suspend fun connect(sessionCredential: String): Boolean

    /** Lists every devhost visible to this authenticated connection. */
    suspend fun listDevhosts(): List<DevHostPresence>

    /**
     * Registers this phone's current FCM token with `relayd`, per
     * notifications.md — replaces any previously registered token. `false`
     * if not connected or the call fails; callers should retry after the
     * next successful [connect] rather than treat this as fatal.
     */
    suspend fun registerFcmToken(fcmToken: String): Boolean

    /**
     * `workspace.diff`, per docs/specs/jj-integration.md: structured hunks
     * computed host-side by `jj-lib`, never on-device. `from`/`to` `null`
     * means the RPC's own documented defaults (`"@-"`/`"@"`).
     */
    suspend fun workspaceDiff(deviceId: String, workspaceId: String, from: String? = null, to: String? = null): List<DiffFileEntry>

    /**
     * `workspace.log`: the `JjChangeGraph` item's node/edge data (edges are
     * each node's own [ChangeGraphNode.parentChangeIds]). `revset` `null`
     * means `jj log`'s own default revset.
     */
    suspend fun workspaceLog(deviceId: String, workspaceId: String, revset: String? = null, limit: Int = 50): List<ChangeGraphNode>

    /** `workspace.op.log`: the operation log backing undo/restore, most recent first. */
    suspend fun workspaceOpLog(deviceId: String, workspaceId: String, limit: Int = 50): List<OperationLogEntry>

    /**
     * `workspace.op.undo`: reverses `opId`'s effect. Returns the id of the
     * *new* operation-log entry the undo itself created — never `opId`.
     */
    suspend fun workspaceOpUndo(deviceId: String, workspaceId: String, opId: String): String

    /**
     * `workspace.op.restore`: resets the repo to `opId`'s state. Returns the
     * id of the new operation-log entry the restore itself created.
     */
    suspend fun workspaceOpRestore(deviceId: String, workspaceId: String, opId: String): String

    /** `workspace.status`: the changed-files/conflict summary for the explorer. */
    suspend fun workspaceStatus(deviceId: String, workspaceId: String): WorkspaceStatus

    /**
     * Opens a document per docs/specs/editor-protocol.md's "Opening a
     * document": `workspace.file.read` with no `revision`/`range`, i.e. the
     * live working copy, whole file. `content` inside [DocumentOpenResult.Success]
     * is base64 — callers decode it explicitly as UTF-8 only when handing
     * text to Sora, never implicitly via a platform-default charset (see
     * editor-protocol.md's "Encoding and line endings MUST round-trip
     * byte-identical").
     */
    suspend fun openDocument(deviceId: String, workspaceId: String, path: String): DocumentOpenResult

    /**
     * Saves a document per docs/specs/editor-protocol.md's "Persistence":
     * `workspace.file.write { workspace_id, path, base_revision,
     * content_base64 }`. `contentBase64` MUST be the document's full
     * current content (this milestone's deliberate V1 scope reduction —
     * never an incremental edit list, per
     * `choosh_protocol::host_rpc::RpcRequest::WorkspaceFileWrite`'s doc
     * comment). [DocumentSaveResult.Stale] is a real conflict
     * (editor-protocol.md's `conflicted` state) — callers MUST surface it
     * for explicit user resolution, never silently overwrite in either
     * direction.
     */
    suspend fun saveDocument(
        deviceId: String,
        workspaceId: String,
        path: String,
        baseRevision: String,
        contentBase64: String,
    ): DocumentSaveResult

    /** Closes the relay connection. Idempotent. */
    fun close()
}

/** Outcome of [ChooshEngine.openDocument]. */
sealed interface DocumentOpenResult {
    data class Success(val contentBase64: String, val revision: String, val totalSize: Long) : DocumentOpenResult

    /**
     * `hostd` rejected the read — per editor-protocol.md's "Limits", a
     * binary or oversized file, or any other `host-rpc.md` application
     * error (`not_found`, etc.). The caller's job is to show a clear
     * "this file can't be edited here" state, not to duplicate `hostd`'s
     * binary/size check client-side.
     */
    data class Rejected(val code: String, val message: String) : DocumentOpenResult

    /** A transport-level failure (not connected, or the call itself failed) — editor-protocol.md's `offline` state. */
    data class Offline(val message: String) : DocumentOpenResult
}

/** Outcome of [ChooshEngine.saveDocument]. */
sealed interface DocumentSaveResult {
    data class Success(val revision: String) : DocumentSaveResult

    /**
     * The file changed on disk since the caller's `baseRevision` was
     * captured — editor-protocol.md's `conflicted` state. Carries the
     * current server-side revision/content so the caller can offer
     * "keep mine" (retry with [currentRevision] as the new `baseRevision`)
     * or "take theirs" (adopt [currentContentBase64]) — never a silent
     * overwrite in either direction.
     */
    data class Stale(val currentRevision: String, val currentContentBase64: String) : DocumentSaveResult
    data class Rejected(val code: String, val message: String) : DocumentSaveResult
    data class Offline(val message: String) : DocumentSaveResult
}

/** Mirrors `choosh_protocol::host_rpc::ChangeKind`. */
enum class ChangeKind { ADDED, MODIFIED, DELETED }

/** Mirrors `choosh_protocol::host_rpc::DiffSegmentKind`. */
enum class DiffSegmentKind { CONTEXT, ADDED, REMOVED }

data class DiffSegment(val kind: DiffSegmentKind, val text: String)

data class DiffHunk(
    val oldStart: Long,
    val oldLines: Long,
    val newStart: Long,
    val newLines: Long,
    val segments: List<DiffSegment>,
)

/**
 * Mirrors `choosh_protocol::host_rpc::DiffFileEntry`: either a hunked text
 * file (a rename has `oldPath != newPath` with possibly-empty `hunks` for a
 * pure rename) or [Binary] metadata — per jj-integration.md, a binary or
 * oversized file is NEVER rendered as garbled hunks.
 */
sealed interface DiffFileEntry {
    data class Hunks(val oldPath: String?, val newPath: String?, val hunks: List<DiffHunk>) : DiffFileEntry
    data class Binary(val path: String, val status: ChangeKind, val byteSize: Long) : DiffFileEntry
}

/** One `workspace.log` change-graph node/edge; mirrors `ChangeGraphNode`. */
data class ChangeGraphNode(
    val changeId: String,
    val commitId: String,
    val description: String,
    val author: String,
    val parentChangeIds: List<String>,
    val isWorkingCopy: Boolean,
    val bookmarks: List<String>,
)

/** One `workspace.op.log` entry; mirrors `OperationLogEntry`. */
data class OperationLogEntry(
    val opId: String,
    val description: String,
    val startTime: String,
    val endTime: String,
    val tags: Map<String, String>,
)

data class ChangedPath(val path: String, val kind: ChangeKind)

/** `workspace.status`'s `{changed, conflicted}` result shape. */
data class WorkspaceStatus(val changed: List<ChangedPath>, val conflicted: List<String>)

/**
 * `relayd`'s WebAuthn HTTP endpoints return either a success payload or a
 * typed failure — this mirrors that rather than throwing for an expected
 * rejection (a stale/invalid ceremony response is not exceptional, callers
 * are expected to handle it as UI state).
 */
sealed interface WebauthnResult {
    @Serializable
    data class Success(val sessionCredential: String) : WebauthnResult

    @Serializable
    data class Failure(val code: String, val message: String) : WebauthnResult
}

/** Mirrors `choosh_protocol::relay::DevHostPresence` exactly (see relay-protocol.md). */
@Serializable
data class DevHostPresence(
    val deviceId: String,
    val alias: String,
    val platform: String,
    val accountLabel: String?,
    val connectionState: ConnectionState,
    val lastSeen: String,
)

enum class ConnectionState { ONLINE, OFFLINE }
