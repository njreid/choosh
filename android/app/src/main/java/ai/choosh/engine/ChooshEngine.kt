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
     * `request-enrollment-token`, per docs/specs/auth-and-enrollment.md's
     * "Enrollment tokens" section: mints a single-use, 15-minute-lifetime
     * token that lets one fresh `devhost` Identity enroll (the resulting
     * token is what an operator pastes into
     * `curl ... | sudo sh -s -- --token=<token>` on the target machine, per
     * docs/specs/host-deployment.md's "Bootstrap install"). Only the
     * `devhost` identity class is exposed here — enrolling a `laptop-proxy`
     * is a `choosh-hostd proxy enroll` CLI flow, not a phone-UI affordance,
     * per that same spec's "Laptop-proxy enrollment" section. Never throws:
     * like [WebauthnResult], an expected rejection (not connected, or a
     * `relayd`-side failure) is returned as [EnrollmentTokenResult.Failure]
     * for the caller to render as UI state.
     */
    suspend fun requestEnrollmentToken(): EnrollmentTokenResult

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

    /**
     * `item.list`: every registered item in `workspaceId`, backing the
     * `WebService`/Markdown pinned-item screens' status polling
     * (docs/specs/service-tunnels.md's "Lifecycle"/"Readiness").
     */
    suspend fun itemList(deviceId: String, workspaceId: String): List<ItemSummary>

    /**
     * `project.list`: every Project `deviceId` (a single devhost's own
     * registry) knows about, per docs/specs/host-rpc.md — the fleet
     * drawer's Project-mode data source. Like every other RPC here, this
     * is scoped to one devhost's tunnel; per host-rpc.md's `workspace.list`
     * precedent ("this is called once per devhost after list-devhosts"),
     * callers assembling the whole fleet's Project list call this once per
     * reachable devhost and merge by `projectId`. `active` is computed
     * entirely by `hostd` (`android-navigation.md`'s Host sort-mode
     * definition) — this client never infers or overrides it.
     */
    suspend fun projectList(deviceId: String): List<ProjectSummary>

    /**
     * `project.set_primary_workspace`: changes which Workspace
     * [projectList] reports as [ProjectSummary.primaryWorkspaceId] for
     * `projectId`, and which one the fleet drawer opens when that Project
     * row is tapped. `workspaceId` MUST already belong to `projectId` —
     * `hostd` rejects a mismatched pair (see docs/specs/host-rpc.md)
     * rather than silently applying it, surfaced here as a thrown
     * exception (this call's only failure mode besides "not connected"),
     * mirroring [saveDocument]'s [DocumentSaveResult.Rejected] posture of
     * never silently swallowing a `hostd`-side rejection.
     */
    suspend fun setPrimaryWorkspace(deviceId: String, projectId: String, workspaceId: String)

    /**
     * Drains every live `agent-event` push (docs/specs/agent-events.md)
     * received on the persistent connection since the last call, oldest
     * first. Always succeeds — an empty list, never a thrown exception,
     * including when nothing has ever connected — callers are expected to
     * poll this on an interval; see [ai.choosh.agentevents.AgentEventSubscription],
     * which owns exactly that loop plus the per-workspace sequence-cursor
     * bookkeeping this method's caller must not duplicate.
     */
    suspend fun pollAgentEvents(): List<AgentEventPush>

    /**
     * `agent-events.md`'s "Delivery and replay" resume request, ridden over
     * its own `"agent-events"`-purpose relay tunnel (see
     * `rust/choosh-protocol/src/relay.rs`'s module doc comment): replays
     * every retained agent event for `workspaceId` strictly after
     * `afterSequence`, oldest first, or reports
     * [AgentEventsResumeOutcome.SnapshotRequired] if `afterSequence` is
     * older than the owning devhost's retained window. `afterSequence` of
     * `null` means "no prior acknowledged sequence for this workspace" —
     * e.g. a first-ever subscribe — which is answered with every
     * currently-retained event, never `SnapshotRequired` (there's no gap to
     * detect when the caller was never caught up to begin with).
     *
     * Throws on failure (not connected, or a transport-level error) —
     * callers (again, normally only [ai.choosh.agentevents.AgentEventSubscription])
     * are expected to catch and retry on their own schedule, the same
     * "throw on transport failure" convention [workspaceStatus]/[workspaceDiff]
     * already use.
     */
    suspend fun agentEventsResume(deviceId: String, workspaceId: String, afterSequence: Long?): AgentEventsResumeOutcome

    /** Closes the relay connection. Idempotent. */
    fun close()
}

// --- docs/specs/agent-events.md domain types --------------------------

/** Mirrors `choosh_protocol::relay::WireInputReason`. */
enum class InputReason { APPROVAL, PERMISSION, QUESTION, ELICITATION, NEXT_PROMPT, UNKNOWN }

/** Mirrors `choosh_protocol::relay::WireAgentStatus`. */
enum class AgentRunStatus { STARTING, BUSY, WAITING, STOPPED, FAILED, UNKNOWN }

/**
 * Mirrors `choosh_protocol::relay::WireAgentEvent` — docs/specs/agent-events.md's
 * normalized event set, one variant per `kind` on the wire.
 * [Unknown] is the forward-compatibility fallback for a `kind` this client
 * build doesn't recognize yet (a future normalized event landing
 * server-side before this client is updated) — decoded, never a crash or a
 * dropped/unparseable push, matching this codebase's existing
 * unrecognized-value convention (e.g. [ai.choosh.engine.WebServiceStatus.UNKNOWN]).
 */
sealed interface AgentEvent {
    data class InputRequired(val workspaceId: String, val itemId: String, val reason: InputReason) : AgentEvent
    data class TurnCompleted(val workspaceId: String, val itemId: String) : AgentEvent
    data class FilesChanged(val workspaceId: String, val itemId: String, val paths: List<String>) : AgentEvent
    data class AgentStatusChanged(val workspaceId: String, val itemId: String, val status: AgentRunStatus) : AgentEvent
    data class EditorAttached(val workspaceId: String) : AgentEvent
    data class EditorDetached(val workspaceId: String) : AgentEvent

    /** Carries no `workspaceId`/`itemId` — see `WireAgentEvent::AuthRequired`'s own doc comment. */
    data class AuthRequired(val provider: String, val userCode: String, val verificationUri: String) : AgentEvent

    data object Unknown : AgentEvent
}

/**
 * The workspace this event is attributable to, mirroring
 * `WireAgentEvent::workspace_id`'s doc comment — `null` only for
 * [AgentEvent.AuthRequired] (deliberately unscoped) and [AgentEvent.Unknown]
 * (unrecognized, so no field to trust).
 */
val AgentEvent.workspaceIdOrNull: String?
    get() = when (this) {
        is AgentEvent.InputRequired -> workspaceId
        is AgentEvent.TurnCompleted -> workspaceId
        is AgentEvent.FilesChanged -> workspaceId
        is AgentEvent.AgentStatusChanged -> workspaceId
        is AgentEvent.EditorAttached -> workspaceId
        is AgentEvent.EditorDetached -> workspaceId
        is AgentEvent.AuthRequired, AgentEvent.Unknown -> null
    }

/** One live agent-event push, per [ChooshEngine.pollAgentEvents]. */
data class AgentEventPush(val fromDeviceId: String, val event: AgentEvent, val sequence: Long?)

/** One replayed event, per [ChooshEngine.agentEventsResume]'s [AgentEventsResumeOutcome.Replayed]. */
data class SequencedAgentEvent(val sequence: Long, val event: AgentEvent)

/** Outcome of [ChooshEngine.agentEventsResume], per `agent-events.md`'s "Delivery and replay". */
sealed interface AgentEventsResumeOutcome {
    /** Oldest first, strictly increasing, no gaps, no duplicates — mirrors `AgentEventsResumeResponse::Replayed`. */
    data class Replayed(val events: List<SequencedAgentEvent>, val latestSequence: Long) : AgentEventsResumeOutcome

    /**
     * `afterSequence` is older than the owning devhost's retained window —
     * per `agent-events.md`, the caller MUST refresh full workspace/item
     * state via `workspace.status`/`workspace.list` rather than treat this
     * as an error or a silent no-op.
     */
    data object SnapshotRequired : AgentEventsResumeOutcome
}

/**
 * Mirrors `choosh_protocol::host_rpc::ProjectSummary` exactly: `{
 * project_id, name, primary_workspace_id, active }`. `active` is
 * `hostd`-computed (`docs/specs/host-rpc.md`'s `project.list`,
 * `docs/specs/android-navigation.md`'s Host sort-mode definition) — never
 * derived client-side from this type's own fields.
 */
data class ProjectSummary(
    val projectId: String,
    val name: String,
    val primaryWorkspaceId: String?,
    val active: Boolean,
)

/**
 * Mirrors `choosh_protocol::host_rpc::ItemType`. `WEB_SERVICE` is the only
 * variant this milestone's screens act on; the others are listed for
 * completeness/forward-compatibility with whatever `item.list` returns.
 */
enum class ItemType { AGENT_TERMINAL, SHELL, WEB_SERVICE, UNKNOWN }

/**
 * Mirrors `docs/specs/service-tunnels.md`'s "Lifecycle" statuses:
 * `starting`, `running`, `stopped`, `failed`, `unknown`. As of this pass,
 * `choosh_protocol::host_rpc::ItemStatus` only defines `Running`/`Stopped`
 * on the wire (the concurrently-landing backend work owns adding the rest) —
 * [ItemSummary.status] decodes whatever string arrives, mapping any value
 * this client doesn't yet recognize to [UNKNOWN] rather than crashing or
 * silently defaulting to a specific known state, so this UI is already
 * forward-compatible with `starting`/`failed` landing later without a
 * client-side change.
 */
enum class WebServiceStatus { STARTING, RUNNING, STOPPED, FAILED, UNKNOWN }

/** Mirrors `choosh_protocol::host_rpc::ItemSummary`. */
data class ItemSummary(
    val itemId: String,
    val itemType: ItemType,
    val name: String,
    val tabTarget: String,
    val status: WebServiceStatus,
    val port: Int?,
)

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

/**
 * Outcome of [ChooshEngine.requestEnrollmentToken], per
 * docs/specs/auth-and-enrollment.md's "Enrollment tokens" section.
 */
sealed interface EnrollmentTokenResult {
    /** `expiresAt` is the RFC 3339 timestamp `relayd` returned verbatim — 15 minutes from issuance. */
    data class Success(val token: String, val expiresAt: String) : EnrollmentTokenResult

    data class Failure(val message: String) : EnrollmentTokenResult
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
