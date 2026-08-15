package ai.choosh

import ai.choosh.engine.ChangeGraphNode
import ai.choosh.engine.ChangeKind
import ai.choosh.engine.ChangedPath
import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.ConnectionState
import ai.choosh.engine.DevHostPresence
import ai.choosh.engine.DiffFileEntry
import ai.choosh.engine.DiffHunk
import ai.choosh.engine.DiffSegment
import ai.choosh.engine.DiffSegmentKind
import ai.choosh.engine.DocumentOpenResult
import ai.choosh.engine.DocumentSaveResult
import ai.choosh.engine.ItemSummary
import ai.choosh.engine.ItemType
import ai.choosh.engine.OperationLogEntry
import ai.choosh.engine.ProjectSummary
import ai.choosh.engine.WebServiceStatus
import ai.choosh.engine.WebauthnResult
import ai.choosh.engine.WorkspaceStatus
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.DeserializationStrategy
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonContentPolymorphicSerializer
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.jsonObject

/**
 * The real, JNI-backed [ChooshEngine] implementation, wrapping
 * `rust/choosh-android-bridge`'s native surface.
 *
 * Every native method there is `static`, takes an explicit `handle: jlong`
 * (except `native_init`, which *produces* that handle), and — because
 * standard JNI symbol resolution (`Java_<package>_<class>_<method>`) binds
 * a native implementation to the exact class the `external fun` is declared
 * in — must be declared inside a Kotlin type literally named
 * `ai.choosh.NativeBridge`, matching every `java_type = "ai.choosh.NativeBridge"`
 * in `lib.rs`'s `native_method!` calls exactly. `NativeChooshEngine` itself
 * is *not* that class (a prior pass declared these as instance methods
 * directly on `NativeChooshEngine`, with mismatched arity/return types on
 * top of the wrong class name — that binding could never have resolved at
 * runtime; it just went unnoticed because the composition root has always
 * defaulted to `FakeChooshEngine`, so `NativeChooshEngine` was never
 * actually instantiated). [NativeBridge] below is the real JNI surface;
 * this class owns the per-instance `handle` and adapts it to [ChooshEngine].
 */
class NativeChooshEngine : ChooshEngine {

    private val handle: Long = NativeBridge.nativeInit(BuildConfig.CHOOSH_RELAYD_HTTP_URL, BuildConfig.CHOOSH_RELAYD_URL)

    override suspend fun webauthnRegisterStart(): String =
        withContext(Dispatchers.IO) { NativeBridge.nativeWebauthnRegisterStart(handle) }

    override suspend fun webauthnRegisterFinish(credentialJson: String): WebauthnResult =
        withContext(Dispatchers.IO) { decodeWebauthnResult(NativeBridge.nativeWebauthnRegisterFinish(handle, credentialJson)) }

    override suspend fun webauthnLoginStart(): String =
        withContext(Dispatchers.IO) { NativeBridge.nativeWebauthnLoginStart(handle) }

    override suspend fun webauthnLoginFinish(credentialJson: String): WebauthnResult =
        withContext(Dispatchers.IO) { decodeWebauthnResult(NativeBridge.nativeWebauthnLoginFinish(handle, credentialJson)) }

    override suspend fun connect(sessionCredential: String): Boolean =
        withContext(Dispatchers.IO) { NativeBridge.nativeConnect(handle, sessionCredential) }

    override suspend fun listDevhosts(): List<DevHostPresence> = withContext(Dispatchers.IO) {
        json.decodeFromString<List<WireDevHostPresence>>(NativeBridge.nativeListDevhosts(handle)).map { it.toDomain() }
    }

    override suspend fun registerFcmToken(fcmToken: String): Boolean =
        withContext(Dispatchers.IO) { NativeBridge.nativeRegisterFcmToken(handle, fcmToken) }

    override suspend fun workspaceDiff(deviceId: String, workspaceId: String, from: String?, to: String?): List<DiffFileEntry> =
        withContext(Dispatchers.IO) {
            val raw = NativeBridge.nativeWorkspaceDiff(handle, deviceId, workspaceId, from.orEmpty(), to.orEmpty())
            decodeOrThrow(raw) { body -> json.decodeFromString<List<WireDiffFileEntry>>(body).map { it.toDomain() } }
        }

    override suspend fun workspaceLog(deviceId: String, workspaceId: String, revset: String?, limit: Int): List<ChangeGraphNode> =
        withContext(Dispatchers.IO) {
            val raw = NativeBridge.nativeWorkspaceLog(handle, deviceId, workspaceId, revset.orEmpty(), limit.toLong())
            decodeOrThrow(raw) { body -> json.decodeFromString<List<WireChangeGraphNode>>(body).map { it.toDomain() } }
        }

    override suspend fun workspaceOpLog(deviceId: String, workspaceId: String, limit: Int): List<OperationLogEntry> =
        withContext(Dispatchers.IO) {
            val raw = NativeBridge.nativeWorkspaceOpLog(handle, deviceId, workspaceId, limit.toLong())
            decodeOrThrow(raw) { body -> json.decodeFromString<List<WireOperationLogEntry>>(body).map { it.toDomain() } }
        }

    override suspend fun workspaceOpUndo(deviceId: String, workspaceId: String, opId: String): String =
        withContext(Dispatchers.IO) {
            val raw = NativeBridge.nativeWorkspaceOpUndo(handle, deviceId, workspaceId, opId)
            decodeOrThrow(raw) { body -> json.decodeFromString<WireNewOpId>(body).new_op_id }
        }

    override suspend fun workspaceOpRestore(deviceId: String, workspaceId: String, opId: String): String =
        withContext(Dispatchers.IO) {
            val raw = NativeBridge.nativeWorkspaceOpRestore(handle, deviceId, workspaceId, opId)
            decodeOrThrow(raw) { body -> json.decodeFromString<WireNewOpId>(body).new_op_id }
        }

    override suspend fun workspaceStatus(deviceId: String, workspaceId: String): WorkspaceStatus =
        withContext(Dispatchers.IO) {
            val raw = NativeBridge.nativeWorkspaceStatus(handle, deviceId, workspaceId)
            decodeOrThrow(raw) { body -> json.decodeFromString<WireWorkspaceStatus>(body).toDomain() }
        }

    override suspend fun openDocument(deviceId: String, workspaceId: String, path: String): DocumentOpenResult =
        withContext(Dispatchers.IO) {
            decodeDocumentOpenResult(NativeBridge.nativeOpenDocument(handle, deviceId, workspaceId, path))
        }

    override suspend fun saveDocument(
        deviceId: String,
        workspaceId: String,
        path: String,
        baseRevision: String,
        contentBase64: String,
    ): DocumentSaveResult = withContext(Dispatchers.IO) {
        decodeDocumentSaveResult(NativeBridge.nativeSaveDocument(handle, deviceId, workspaceId, path, baseRevision, contentBase64))
    }

    override suspend fun itemList(deviceId: String, workspaceId: String): List<ItemSummary> = withContext(Dispatchers.IO) {
        val raw = NativeBridge.nativeItemList(handle, deviceId, workspaceId)
        decodeOrThrow(raw) { body -> json.decodeFromString<List<WireItemSummary>>(body).map { it.toDomain() } }
    }

    override suspend fun projectList(deviceId: String): List<ProjectSummary> = withContext(Dispatchers.IO) {
        val raw = NativeBridge.nativeProjectList(handle, deviceId)
        decodeOrThrow(raw) { body -> json.decodeFromString<List<WireProjectSummary>>(body).map { it.toDomain() } }
    }

    override suspend fun setPrimaryWorkspace(deviceId: String, projectId: String, workspaceId: String) {
        withContext(Dispatchers.IO) {
            val raw = NativeBridge.nativeProjectSetPrimaryWorkspace(handle, deviceId, projectId, workspaceId)
            decodeOrThrow(raw) { /* {"ok":true} on success; decodeOrThrow already throws on the shared {"error":...} shape. */ }
        }
    }

    /** Exposes this instance's raw JNI connection handle to `WebGatewayBridge`/`MarkdownGatewayBridge`, mirroring [ai.choosh.terminal.TerminalSession.attachPty]'s existing `connectionHandle` pattern. */
    val connectionHandle: Long get() = handle

    override fun close() = NativeBridge.nativeClose(handle)
}

/**
 * The literal JNI surface — every signature here must match `lib.rs`'s
 * `native_method!` declarations exactly (type, arity, and `static`-ness).
 * Nothing but [NativeChooshEngine] should call these directly.
 */
private object NativeBridge {
    init { System.loadLibrary("choosh_android_bridge") }

    // `native_method!` on the Rust side registers these as true JVM `static`
    // methods — a plain `external fun` in a Kotlin `object` compiles as an
    // instance method on the singleton (called via a static `INSTANCE`
    // field), which is a different JVM method shape and fails to link
    // ("registered as static but called as instance method", confirmed by
    // a real crash against the real .so before `@JvmStatic` was added
    // here). `@JvmStatic` is required on every one of these, not optional.
    @JvmStatic external fun nativeInit(httpBaseUrl: String, wsUrl: String): Long
    @JvmStatic external fun nativeWebauthnRegisterStart(handle: Long): String
    @JvmStatic external fun nativeWebauthnRegisterFinish(handle: Long, credentialJson: String): String
    @JvmStatic external fun nativeWebauthnLoginStart(handle: Long): String
    @JvmStatic external fun nativeWebauthnLoginFinish(handle: Long, credentialJson: String): String
    @JvmStatic external fun nativeConnect(handle: Long, sessionCredential: String): Boolean
    @JvmStatic external fun nativeListDevhosts(handle: Long): String
    @JvmStatic external fun nativeRegisterFcmToken(handle: Long, fcmToken: String): Boolean
    @JvmStatic external fun nativeClose(handle: Long)

    // M3 jj RPCs (docs/specs/jj-integration.md). `from`/`to`/`revset` take
    // an empty string for "omitted" — there is no nullable-`JString`
    // special case at this boundary, matching every other native method
    // here's plain-`String`-in shape; `Engine::none_if_empty` on the Rust
    // side is the single place that convention is documented and applied.
    @JvmStatic external fun nativeWorkspaceDiff(handle: Long, targetDeviceId: String, workspaceId: String, from: String, to: String): String
    @JvmStatic external fun nativeWorkspaceLog(handle: Long, targetDeviceId: String, workspaceId: String, revset: String, limit: Long): String
    @JvmStatic external fun nativeWorkspaceOpLog(handle: Long, targetDeviceId: String, workspaceId: String, limit: Long): String
    @JvmStatic external fun nativeWorkspaceOpUndo(handle: Long, targetDeviceId: String, workspaceId: String, opId: String): String
    @JvmStatic external fun nativeWorkspaceOpRestore(handle: Long, targetDeviceId: String, workspaceId: String, opId: String): String
    @JvmStatic external fun nativeWorkspaceStatus(handle: Long, targetDeviceId: String, workspaceId: String): String

    // M4 editor-protocol.md document RPCs.
    @JvmStatic external fun nativeOpenDocument(handle: Long, targetDeviceId: String, workspaceId: String, path: String): String
    @JvmStatic external fun nativeSaveDocument(
        handle: Long,
        targetDeviceId: String,
        workspaceId: String,
        path: String,
        baseRevision: String,
        contentBase64: String,
    ): String

    // M5 service-tunnels.md item listing.
    @JvmStatic external fun nativeItemList(handle: Long, targetDeviceId: String, workspaceId: String): String

    // host-rpc.md's fleet-drawer Project-mode RPCs.
    @JvmStatic external fun nativeProjectList(handle: Long, targetDeviceId: String): String
    @JvmStatic external fun nativeProjectSetPrimaryWorkspace(handle: Long, targetDeviceId: String, projectId: String, workspaceId: String): String
}

/**
 * Every M3/M4 native method returns this module's shared `{"error": ...}`
 * shape on failure (not connected, a transport error, or `hostd`'s own
 * `RpcResponse::Error`) — decoding as [WireError] first lets every call
 * site throw a clear message instead of a raw "expected array, got object"
 * `SerializationException` when the RPC failed. A genuine success body
 * never has an `"error"` key, so this never misfires on real data.
 */
@Serializable
private data class WireError(val error: String)

/** The single shared [Json] config for this whole file — every decode function below (including [NativeChooshEngine]'s own methods, which see this unqualified) reuses this instance rather than constructing its own. */
private val json = Json { ignoreUnknownKeys = true }

private fun <T> decodeOrThrow(raw: String, decode: (String) -> T): T {
    val error = runCatching { json.decodeFromString<WireError>(raw) }.getOrNull()
    if (error != null) throw IllegalStateException(error.error)
    return decode(raw)
}

/**
 * Matches the wire shape `relayd`/`hostd` actually serialize
 * (`choosh_protocol::relay::DevHostPresence`'s `snake_case` JSON field
 * names) before translating into this module's `camelCase` domain type.
 */
@Serializable
private data class WireDevHostPresence(
    val device_id: String,
    val alias: String,
    val platform: String,
    val account_label: String?,
    val connection_state: String,
    val last_seen: String,
) {
    fun toDomain() = DevHostPresence(
        deviceId = device_id,
        alias = alias,
        platform = platform,
        accountLabel = account_label,
        connectionState = if (connection_state == "online") ConnectionState.ONLINE else ConnectionState.OFFLINE,
        lastSeen = last_seen,
    )
}

@Serializable
private data class WireWebauthnResult(
    val ok: Boolean,
    val session_credential: String? = null,
    val code: String? = null,
    val message: String? = null,
)

private fun decodeWebauthnResult(raw: String): WebauthnResult {
    val wire = json.decodeFromString<WireWebauthnResult>(raw)
    return if (wire.ok && wire.session_credential != null) {
        WebauthnResult.Success(wire.session_credential)
    } else {
        WebauthnResult.Failure(wire.code ?: "unknown", wire.message ?: "native engine returned no message")
    }
}

// --- M3 jj RPC wire shapes ---------------------------------------------
//
// Every type below mirrors `choosh_protocol::host_rpc`'s corresponding
// Rust type field-for-field (plain `snake_case` field names — none of
// those wire structs use `rename_all` on their fields, only their enum
// variant tags), the same "wire struct decodes then maps to a camelCase
// domain type" split [WireDevHostPresence] above already establishes.

private fun wireChangeKindToDomain(kind: String): ChangeKind = when (kind) {
    "added" -> ChangeKind.ADDED
    "deleted" -> ChangeKind.DELETED
    else -> ChangeKind.MODIFIED
}

private fun wireDiffSegmentKindToDomain(kind: String): DiffSegmentKind = when (kind) {
    "added" -> DiffSegmentKind.ADDED
    "removed" -> DiffSegmentKind.REMOVED
    else -> DiffSegmentKind.CONTEXT
}

@Serializable
private data class WireDiffSegment(val kind: String, val text: String) {
    fun toDomain() = DiffSegment(kind = wireDiffSegmentKindToDomain(kind), text = text)
}

@Serializable
private data class WireDiffHunk(
    val old_start: Long,
    val old_lines: Long,
    val new_start: Long,
    val new_lines: Long,
    val segments: List<WireDiffSegment>,
) {
    fun toDomain() = DiffHunk(
        oldStart = old_start,
        oldLines = old_lines,
        newStart = new_start,
        newLines = new_lines,
        segments = segments.map { it.toDomain() },
    )
}

/**
 * `choosh_protocol::host_rpc::DiffFileEntry` is `#[serde(untagged)]` — the
 * wire shape has no discriminant key, only the presence of `"hunks"` vs.
 * `"byte_size"` distinguishes the two cases. [JsonContentPolymorphicSerializer]
 * is kotlinx.serialization's dedicated tool for exactly this "untagged,
 * pick by which fields are present" shape.
 */
private object WireDiffFileEntrySerializer : JsonContentPolymorphicSerializer<WireDiffFileEntry>(WireDiffFileEntry::class) {
    override fun selectDeserializer(element: JsonElement): DeserializationStrategy<WireDiffFileEntry> =
        if ("hunks" in element.jsonObject) WireDiffFileEntry.Hunks.serializer() else WireDiffFileEntry.Binary.serializer()
}

@Serializable(with = WireDiffFileEntrySerializer::class)
private sealed interface WireDiffFileEntry {
    fun toDomain(): DiffFileEntry

    @Serializable
    data class Hunks(val old_path: String? = null, val new_path: String? = null, val hunks: List<WireDiffHunk>) : WireDiffFileEntry {
        override fun toDomain() = DiffFileEntry.Hunks(oldPath = old_path, newPath = new_path, hunks = hunks.map { it.toDomain() })
    }

    @Serializable
    data class Binary(val path: String, val status: String, val byte_size: Long) : WireDiffFileEntry {
        override fun toDomain() = DiffFileEntry.Binary(path = path, status = wireChangeKindToDomain(status), byteSize = byte_size)
    }
}

@Serializable
private data class WireChangeGraphNode(
    val change_id: String,
    val commit_id: String,
    val description: String,
    val author: String,
    val parent_change_ids: List<String>,
    val is_working_copy: Boolean,
    val bookmarks: List<String>,
) {
    fun toDomain() = ChangeGraphNode(
        changeId = change_id,
        commitId = commit_id,
        description = description,
        author = author,
        parentChangeIds = parent_change_ids,
        isWorkingCopy = is_working_copy,
        bookmarks = bookmarks,
    )
}

@Serializable
private data class WireOperationLogEntry(
    val op_id: String,
    val description: String,
    val start_time: String,
    val end_time: String,
    val tags: Map<String, String>,
) {
    fun toDomain() = OperationLogEntry(opId = op_id, description = description, startTime = start_time, endTime = end_time, tags = tags)
}

@Serializable
private data class WireNewOpId(val new_op_id: String)

@Serializable
private data class WireChangedPath(val path: String, val kind: String) {
    fun toDomain() = ChangedPath(path = path, kind = wireChangeKindToDomain(kind))
}

@Serializable
private data class WireWorkspaceStatus(val changed: List<WireChangedPath>, val conflicted: List<String>) {
    fun toDomain() = WorkspaceStatus(changed = changed.map { it.toDomain() }, conflicted = conflicted)
}

// --- M4 editor-protocol.md document wire shapes -------------------------

/**
 * Matches `choosh-android-bridge::engine::Engine::open_document`'s/
 * `save_document`'s discriminated-by-`type` JSON exactly — see that
 * module's doc comments for the wire shape.
 */
@Serializable
private data class WireDocumentResult(
    val type: String,
    val content_base64: String? = null,
    val revision: String? = null,
    val total_size: Long? = null,
    val current_revision: String? = null,
    val current_content_base64: String? = null,
    val code: String? = null,
    val message: String? = null,
)

private fun decodeDocumentOpenResult(raw: String): DocumentOpenResult {
    val wire = json.decodeFromString<WireDocumentResult>(raw)
    return when (wire.type) {
        "ok" -> DocumentOpenResult.Success(
            contentBase64 = wire.content_base64 ?: "",
            revision = wire.revision ?: "",
            totalSize = wire.total_size ?: 0L,
        )
        "offline" -> DocumentOpenResult.Offline(wire.message ?: "offline")
        else -> DocumentOpenResult.Rejected(wire.code ?: "unknown", wire.message ?: "native engine returned no message")
    }
}

// --- M5 service-tunnels.md item wire shapes ------------------------------

private fun wireItemTypeToDomain(type: String): ItemType = when (type) {
    "agent-terminal" -> ItemType.AGENT_TERMINAL
    "shell" -> ItemType.SHELL
    "web-service" -> ItemType.WEB_SERVICE
    else -> ItemType.UNKNOWN
}

/**
 * `choosh_protocol::host_rpc::ItemStatus` is `snake_case` on the wire
 * (`running`/`stopped`, as of this pass — see [WebServiceStatus]'s doc
 * comment on why every other value maps to [WebServiceStatus.UNKNOWN]
 * rather than failing to decode).
 */
private fun wireItemStatusToDomain(status: String): WebServiceStatus = when (status) {
    "starting" -> WebServiceStatus.STARTING
    "running" -> WebServiceStatus.RUNNING
    "stopped" -> WebServiceStatus.STOPPED
    "failed" -> WebServiceStatus.FAILED
    else -> WebServiceStatus.UNKNOWN
}

@Serializable
private data class WireItemSummary(
    val item_id: String,
    val item_type: String,
    val name: String,
    val tab_target: String,
    val status: String,
    val port: Int? = null,
) {
    fun toDomain() = ItemSummary(
        itemId = item_id,
        itemType = wireItemTypeToDomain(item_type),
        name = name,
        tabTarget = tab_target,
        status = wireItemStatusToDomain(status),
        port = port,
    )
}

// --- host-rpc.md Project-mode wire shapes -------------------------------

/** Mirrors `choosh_protocol::host_rpc::ProjectSummary` field-for-field. */
@Serializable
private data class WireProjectSummary(
    val project_id: String,
    val name: String,
    val primary_workspace_id: String? = null,
    val active: Boolean,
) {
    fun toDomain() = ProjectSummary(projectId = project_id, name = name, primaryWorkspaceId = primary_workspace_id, active = active)
}

private fun decodeDocumentSaveResult(raw: String): DocumentSaveResult {
    val wire = json.decodeFromString<WireDocumentResult>(raw)
    return when (wire.type) {
        "ok" -> DocumentSaveResult.Success(revision = wire.revision ?: "")
        "stale" -> DocumentSaveResult.Stale(
            currentRevision = wire.current_revision ?: "",
            currentContentBase64 = wire.current_content_base64 ?: "",
        )
        "offline" -> DocumentSaveResult.Offline(wire.message ?: "offline")
        else -> DocumentSaveResult.Rejected(wire.code ?: "unknown", wire.message ?: "native engine returned no message")
    }
}
