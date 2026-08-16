package ai.choosh.engine

import ai.choosh.fleet.FleetFixtures
import java.time.Instant
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

    override suspend fun webauthnRegisterStart(bootstrapSecret: String): String {
        delay(FAKE_LATENCY_MS)
        // bootstrapSecret is ignored here, same as every other unused parameter
        // this fake's other methods take (e.g. workspaceDiff's deviceId/workspaceId) —
        // this fake has no relayd to gate, so there's nothing to verify it against.
        // Must be a well-formed WebAuthn PublicKeyCredentialCreationOptions JSON: androidx.credentials'
        // CreatePublicKeyCredentialRequest constructor validates this eagerly (before the ceremony even
        // starts) and throws IllegalArgumentException — uncaught by ConnectionScreen's
        // `catch (failure: CreateCredentialException)`, since IllegalArgumentException isn't a
        // CreateCredentialException — if a required field like `user.name` is missing. Found via a real
        // on-device tap of "Set up with a passkey", which crashed the whole app; see
        // docs/accessibility-device-report.md.
        return """{"challenge":"ZmFrZS1jaGFsbGVuZ2U","rp":{"id":"choosh.local","name":"Choosh"},"user":{"id":"ZmFrZS11c2VyLWlk","name":"fake-user","displayName":"Fake User"},"pubKeyCredParams":[{"type":"public-key","alg":-7}],"timeout":60000,"attestation":"none"}"""
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

    /** One-shot: when non-null, the next [connect] call returns [ConnectResult.Rejected] with this message instead of succeeding, then clears itself. */
    var simulateConnectRejection: String? = null

    /** One-shot: when non-null, the next [connect] call returns [ConnectResult.TransportFailure] with this message instead of succeeding, then clears itself. */
    var simulateConnectTransportFailure: String? = null

    override suspend fun connect(sessionCredential: String): ConnectResult {
        delay(FAKE_LATENCY_MS)
        simulateConnectRejection?.let { message ->
            simulateConnectRejection = null
            connected = false
            return ConnectResult.Rejected(message)
        }
        simulateConnectTransportFailure?.let { message ->
            simulateConnectTransportFailure = null
            return ConnectResult.TransportFailure(message)
        }
        connected = true
        return ConnectResult.Connected
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

    /** Set by a test to exercise [EnrollmentTokenResult.Failure] without touching [connected]. */
    var simulateEnrollmentTokenFailure: Boolean = false
    private var enrollmentTokenCounter = 0

    override suspend fun requestEnrollmentToken(): EnrollmentTokenResult {
        delay(FAKE_LATENCY_MS)
        if (!connected) return EnrollmentTokenResult.Failure("not connected: call connect first")
        if (simulateEnrollmentTokenFailure) return EnrollmentTokenResult.Failure("fake request-enrollment-token failure")
        enrollmentTokenCounter += 1
        return EnrollmentTokenResult.Success(
            token = "fake-enroll-tok-$enrollmentTokenCounter",
            expiresAt = Instant.now().plusSeconds(FAKE_ENROLLMENT_TOKEN_LIFETIME_SECONDS).toString(),
        )
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

    /**
     * Cycles a fixture `WebService` item through `starting` -> `running` on
     * successive calls (the first two calls return `starting`), so a
     * ViewModel/screen polling this exercises the real "retrying
     * interstitial until ready" path without a real backend. A second fixed
     * `Shell` item (always `running`, no port) and a third fixed
     * `AgentTerminal` item are included so `itemList` consumers (the
     * explorer's active-agents/dev-services sections in particular) see a
     * realistic mixed-type result, matching this fake's existing style
     * elsewhere (e.g. [FIXTURE_DEVHOSTS]'s mixed online/offline rows).
     * [createdItems] holds every item [createItem] has actually registered,
     * appended to this fixed set — so a caller that creates a new
     * agent/service via this fake genuinely sees it on the next poll,
     * mirroring [workspaceOpUndo]/[workspaceOpRestore]'s "real mutation, not
     * a static stub" precedent.
     */
    private var webServicePollCount = 0
    private val createdItems = mutableListOf<ItemSummary>()

    override suspend fun itemList(deviceId: String, workspaceId: String): List<ItemSummary> {
        delay(FAKE_LATENCY_MS)
        webServicePollCount += 1
        val webServiceStatus = if (webServicePollCount <= 2) WebServiceStatus.STARTING else WebServiceStatus.RUNNING
        return listOf(
            ItemSummary(
                itemId = "item-agent-1",
                itemType = ItemType.AGENT_TERMINAL,
                name = "agent-a",
                tabTarget = "tab-agent-1",
                status = WebServiceStatus.RUNNING,
                port = null,
            ),
            ItemSummary(
                itemId = "item-web-1",
                itemType = ItemType.WEB_SERVICE,
                name = "web",
                tabTarget = "tab-web-1",
                status = webServiceStatus,
                port = 3000,
            ),
            ItemSummary(
                itemId = "item-shell-1",
                itemType = ItemType.SHELL,
                name = "shell",
                tabTarget = "tab-shell-1",
                status = WebServiceStatus.RUNNING,
                port = null,
            ),
        ) + createdItems
    }

    /** Set by a test to exercise [ChooshEngine.createItem]'s [CreateItemResult.Failure] path without touching [connected]. */
    var forceNextCreateItemFailure: String? = null

    /**
     * A real, minimal validation + mutation, mirroring `hostd`'s own
     * `item.create` posture (host-rpc.md: rejects a duplicate name as
     * `conflict`) rather than a no-op stub that always succeeds — the new
     * item is genuinely appended to [createdItems] so a subsequent
     * [itemList] reflects it, and its name is checked against every
     * already-registered fixture/created item.
     */
    override suspend fun createItem(
        deviceId: String,
        workspaceId: String,
        itemType: ItemType,
        name: String,
        agent: AgentKind?,
        command: List<String>?,
        port: Int?,
    ): CreateItemResult {
        delay(FAKE_LATENCY_MS)
        forceNextCreateItemFailure?.let { message ->
            forceNextCreateItemFailure = null
            return CreateItemResult.Failure(message)
        }
        val existingNames = (listOf("agent-a", "web", "shell") + createdItems.map { it.name })
        if (name in existingNames) {
            return CreateItemResult.Failure("conflict: an item named '$name' already exists")
        }
        if (itemType == ItemType.AGENT_TERMINAL && agent == null) {
            return CreateItemResult.Failure("invalid_argument: agent is required for AgentTerminal")
        }
        if (itemType == ItemType.WEB_SERVICE && (command.isNullOrEmpty() || port == null)) {
            return CreateItemResult.Failure("invalid_argument: command and port are required for WebService")
        }
        val itemId = "item-created-${createdItems.size + 1}"
        val tabTarget = "tab-created-${createdItems.size + 1}"
        createdItems.add(ItemSummary(itemId = itemId, itemType = itemType, name = name, tabTarget = tabTarget, status = WebServiceStatus.RUNNING, port = port))
        return CreateItemResult.Success(itemId = itemId, itemType = itemType, name = name, tabTarget = tabTarget)
    }

    /**
     * Filtered to `deviceId`'s own workspaces, per [ChooshEngine.workspaceList]'s
     * doc comment ("called once per devhost"), mirroring [projectList]'s
     * identical scoping — sourced from the same [FleetFixtures.projectsFor]
     * data [projectList] uses, so [ai.choosh.fleet.FleetViewModel]'s merge
     * of both real RPCs produces the exact same `Project.workspaces` shape
     * the pre-`workspace.list` fixture-only path used to hand it directly.
     */
    override suspend fun workspaceList(deviceId: String): List<WorkspaceSummary> {
        delay(FAKE_LATENCY_MS)
        check(connected) { "workspaceList() called before connect() succeeded" }
        return FleetFixtures.projectsFor(FIXTURE_DEVHOSTS).flatMap { project ->
            project.workspaces.filter { it.devHostId == deviceId }.map { workspace ->
                WorkspaceSummary(
                    workspaceId = workspace.workspaceId,
                    workspaceName = workspace.name,
                    devHostId = workspace.devHostId,
                    projectId = project.projectId,
                    createdAt = workspace.lastActiveAt,
                )
            }
        }
    }

    /**
     * A small, fixed fixture tree (`README.md`/`app.kt`/`docs/`/
     * [PAGINATED_DIR_NAME] at the root, `docs/guide.md` one level down) —
     * enough for [ai.choosh.explorer.ExplorerViewModel]'s drill-down/search
     * paths to be genuinely exercisable without a real backend.
     * [PAGINATED_DIR_NAME] is a genuinely paginated directory: its
     * [PAGINATED_DIR_ENTRIES] span multiple [PAGINATED_DIR_PAGE_SIZE]-sized
     * pages chained by a real `nextCursor` (an opaque string cursor here —
     * this fixture's own choice of encoding, not a contract
     * [ExplorerViewModel] is allowed to assume anything about beyond
     * "non-null means more"), mirroring `workspace.tree.list`'s real
     * 500-entries-per-page bound (host-rpc.md's Bounds) at a size small
     * enough for a test to exercise without actually needing 500+ fixture
     * rows. Unknown `pathPrefix`es return an empty page rather than an
     * error (a plausible `hostd` response for an as-yet-unpopulated
     * directory, not a failure this fake needs to simulate separately).
     */
    override suspend fun workspaceTreeList(deviceId: String, workspaceId: String, pathPrefix: String, cursor: String?): WorkspaceTreeListResult {
        delay(FAKE_LATENCY_MS)
        return when (pathPrefix) {
            "" -> WorkspaceTreeListResult(
                entries = listOf(
                    TreeEntry("README.md", TreeEntryKind.FILE, conflicted = false),
                    TreeEntry("app.kt", TreeEntryKind.FILE, conflicted = false),
                    TreeEntry("docs", TreeEntryKind.DIRECTORY, conflicted = false),
                    TreeEntry(PAGINATED_DIR_NAME, TreeEntryKind.DIRECTORY, conflicted = false),
                ),
                nextCursor = null,
            )
            "docs" -> WorkspaceTreeListResult(entries = listOf(TreeEntry("guide.md", TreeEntryKind.FILE, conflicted = false)), nextCursor = null)
            PAGINATED_DIR_NAME -> paginatedDirPage(cursor)
            else -> WorkspaceTreeListResult(entries = emptyList(), nextCursor = null)
        }
    }

    /** One page of [PAGINATED_DIR_ENTRIES], per [workspaceTreeList]'s [PAGINATED_DIR_NAME] branch — `cursor` is the (string-encoded) index of the first entry still owed, `null` meaning "from the start". */
    private fun paginatedDirPage(cursor: String?): WorkspaceTreeListResult {
        val startIndex = cursor?.toIntOrNull() ?: 0
        val page = PAGINATED_DIR_ENTRIES.drop(startIndex).take(PAGINATED_DIR_PAGE_SIZE)
        val nextIndex = startIndex + page.size
        val nextCursor = if (nextIndex < PAGINATED_DIR_ENTRIES.size) nextIndex.toString() else null
        return WorkspaceTreeListResult(entries = page, nextCursor = nextCursor)
    }

    /**
     * `setPrimaryWorkspace` overrides, keyed by `projectId` — lets a
     * subsequent [projectList] call actually reflect a successful switch,
     * mirroring [workspaceOpUndo]/[workspaceOpRestore]'s "real mutation,
     * not a static stub" precedent rather than a no-op fake.
     */
    private val projectPrimaryOverrides = mutableMapOf<String, String>()

    /** Set by a test to exercise [ChooshEngine.projectList]'s failure/error-state path without touching [connected]. */
    var simulateProjectListFailure: Boolean = false

    /**
     * Scoped to `deviceId`, per [ChooshEngine.projectList]'s doc comment —
     * only Projects with at least one [FleetFixtures] workspace on this
     * devhost are returned, the same "one devhost's own registry" scoping
     * the real RPC has. `active` mirrors [Project.isActive] computed from
     * the fixture Workspace data, standing in for `hostd`'s real
     * computation (host-rpc.md: "`hostd` computes it, the client does
     * not" — this fake plays the `hostd` role here, [FleetViewModel]
     * never recomputes it).
     */
    override suspend fun projectList(deviceId: String): List<ProjectSummary> {
        delay(FAKE_LATENCY_MS)
        check(connected) { "projectList() called before connect() succeeded" }
        if (simulateProjectListFailure) error("fake project.list failure")
        return FleetFixtures.projectsFor(FIXTURE_DEVHOSTS)
            .filter { project -> project.workspaces.any { it.devHostId == deviceId } }
            .map { project ->
                ProjectSummary(
                    projectId = project.projectId,
                    name = project.name,
                    primaryWorkspaceId = projectPrimaryOverrides[project.projectId] ?: project.primaryWorkspaceId,
                    active = project.isActive,
                )
            }
    }

    /**
     * Real validation, mirroring `hostd`'s own "`workspace_id` MUST
     * already belong to `project_id`" rejection (host-rpc.md) rather than
     * a no-op stub that always succeeds.
     */
    override suspend fun setPrimaryWorkspace(deviceId: String, projectId: String, workspaceId: String) {
        delay(FAKE_LATENCY_MS)
        check(connected) { "setPrimaryWorkspace() called before connect() succeeded" }
        val project = FleetFixtures.projectsFor(FIXTURE_DEVHOSTS).firstOrNull { it.projectId == projectId }
            ?: error("not_found: project_id is not registered")
        if (project.workspaces.none { it.workspaceId == workspaceId }) {
            error("invalid_argument: workspace_id does not belong to project_id")
        }
        projectPrimaryOverrides[projectId] = workspaceId
    }

    // --- docs/specs/resources-and-reauth.md fakes ---------------------------

    /**
     * Confirmed, listed Resources — seeded with one fixture per built-in
     * reauth pattern (a/b/c/d) plus one non-reauth Resource (`pattern =
     * null`), so [ai.choosh.resources.ResourcesScreen]/its ViewModel are
     * exercisable against every row shape without a real backend.
     */
    private val resources = mutableListOf(
        Resource(
            resourceId = "res-aws-sso",
            displayName = "Prod AWS SSO",
            resourceKind = "aws-sso",
            pattern = ResourcePattern.A,
            mobileProfile = MobileProfile.WORK,
            createdBy = "operator",
            lastUsedAt = "2026-08-15T09:00:00Z",
            lastVerifiedAt = "2026-08-15T09:00:05Z",
        ),
        Resource(
            resourceId = "res-gcloud",
            displayName = "Personal gcloud",
            resourceKind = "gcloud",
            pattern = ResourcePattern.B,
            mobileProfile = MobileProfile.PERSONAL,
            createdBy = "operator",
            lastUsedAt = null,
            lastVerifiedAt = null,
        ),
        Resource(
            resourceId = "res-twilio",
            displayName = "Twilio",
            resourceKind = "custom",
            pattern = ResourcePattern.C,
            mobileProfile = MobileProfile.ASK,
            createdBy = "agent:codex-1",
            lastUsedAt = "2026-08-10T12:00:00Z",
            lastVerifiedAt = "2026-08-10T12:00:30Z",
        ),
        Resource(
            resourceId = "res-firebase",
            displayName = "Firebase",
            resourceKind = "firebase",
            pattern = ResourcePattern.D,
            mobileProfile = MobileProfile.ASK,
            createdBy = "operator",
            lastUsedAt = null,
            lastVerifiedAt = null,
        ),
        Resource(
            resourceId = "res-test-host",
            displayName = "Second EC2 test host",
            resourceKind = "custom",
            pattern = null,
            mobileProfile = MobileProfile.ASK,
            createdBy = "operator",
            lastUsedAt = "2026-08-14T00:00:00Z",
            lastVerifiedAt = null,
        ),
    )

    /** Pending, unconfirmed proposals awaiting [resourceConfirm] — never returned by [resourceList] until approved, mirroring `RpcRequest::ResourcePropose`'s own doc comment. */
    private val pendingResourceProposals = mutableMapOf<String, Resource>()
    private var resourceProposalCounter = 0

    /** Set by a test to exercise [ResourceProposeResult.Failure]/[ResourceConfirmResult.Failure] without touching [connected]. */
    var simulateResourceRpcFailure: String? = null

    override suspend fun resourceList(deviceId: String): List<Resource> {
        delay(FAKE_LATENCY_MS)
        check(connected) { "resourceList() called before connect() succeeded" }
        return resources.toList()
    }

    override suspend fun resourcePropose(
        deviceId: String,
        displayName: String,
        resourceKind: String,
        pattern: ResourcePattern?,
        reauthCommand: String?,
        mobileProfile: MobileProfile,
    ): ResourceProposeResult {
        delay(FAKE_LATENCY_MS)
        simulateResourceRpcFailure?.let { message ->
            simulateResourceRpcFailure = null
            return ResourceProposeResult.Failure(message)
        }
        resourceProposalCounter += 1
        val resourceId = "res-pending-$resourceProposalCounter"
        pendingResourceProposals[resourceId] = Resource(
            resourceId = resourceId,
            displayName = displayName,
            resourceKind = resourceKind,
            pattern = pattern,
            mobileProfile = mobileProfile,
            createdBy = "operator",
            lastUsedAt = null,
            lastVerifiedAt = null,
        )
        return ResourceProposeResult.Success(resourceId = resourceId)
    }

    override suspend fun resourceConfirm(deviceId: String, resourceId: String, approve: Boolean): ResourceConfirmResult {
        delay(FAKE_LATENCY_MS)
        simulateResourceRpcFailure?.let { message ->
            simulateResourceRpcFailure = null
            return ResourceConfirmResult.Failure(message)
        }
        val proposal = pendingResourceProposals.remove(resourceId) ?: return ResourceConfirmResult.Failure("not_found: no such pending proposal")
        if (!approve) return ResourceConfirmResult.Success(resource = null)
        resources.add(proposal)
        return ResourceConfirmResult.Success(resource = proposal)
    }

    /** Set by a test to exercise the `verified = false` path of [resourceReauthComplete] without touching [connected]. */
    var simulateResourceReauthVerifiedFailure: Boolean = false

    override suspend fun resourceReauthStart(deviceId: String, resourceId: String): Boolean {
        delay(FAKE_LATENCY_MS)
        return connected
    }

    override suspend fun resourceReauthComplete(deviceId: String, resourceId: String, value: String): Boolean {
        delay(FAKE_LATENCY_MS)
        if (simulateResourceReauthVerifiedFailure) {
            simulateResourceReauthVerifiedFailure = false
            return false
        }
        return connected && value.isNotEmpty()
    }

    // --- docs/specs/agent-events.md fakes -----------------------------------

    /** Test/demo-injected live pushes, drained (in order) by [pollAgentEvents] — never populated on its own. */
    private val agentEventQueue = ArrayDeque<AgentEventPush>()

    /** Lets a test/preview simulate a live agent-event push arriving, without a real backend. */
    fun enqueueAgentEvent(push: AgentEventPush) {
        agentEventQueue.addLast(push)
    }

    override suspend fun pollAgentEvents(): List<AgentEventPush> {
        delay(FAKE_LATENCY_MS)
        val drained = agentEventQueue.toList()
        agentEventQueue.clear()
        return drained
    }

    /** Per-`workspaceId` fixture history [agentEventsResume] replays from — seeded by a test via [setAgentEventHistory]. */
    private val agentEventHistory = mutableMapOf<String, List<SequencedAgentEvent>>()

    /** Set by a test to exercise [AgentEventsResumeOutcome.SnapshotRequired] without clearing [agentEventHistory]. */
    var simulateSnapshotRequired: Boolean = false

    /** Seeds [agentEventsResume]'s fixture history for `workspaceId`, oldest first. */
    fun setAgentEventHistory(workspaceId: String, events: List<SequencedAgentEvent>) {
        agentEventHistory[workspaceId] = events
    }

    override suspend fun agentEventsResume(deviceId: String, workspaceId: String, afterSequence: Long?): AgentEventsResumeOutcome {
        delay(FAKE_LATENCY_MS)
        if (simulateSnapshotRequired) return AgentEventsResumeOutcome.SnapshotRequired
        val history = agentEventHistory[workspaceId].orEmpty()
        val after = afterSequence ?: 0L
        val replay = history.filter { it.sequence > after }
        return AgentEventsResumeOutcome.Replayed(events = replay, latestSequence = history.maxOfOrNull { it.sequence } ?: after)
    }

    override fun close() {
        connected = false
    }

    private data class FakeDocument(var contentBase64: String, var revision: Int)

    companion object {
        private const val FAKE_LATENCY_MS = 120L

        /** Matches auth-and-enrollment.md's real 15-minute enrollment-token lifetime. */
        private const val FAKE_ENROLLMENT_TOKEN_LIFETIME_SECONDS = 15L * 60
        const val FIXTURE_DEVICE_ID = "dev-mbp-home"
        const val FIXTURE_WORKSPACE_ID = "ws-choosh-app"

        /** [workspaceTreeList]'s genuinely-paginated fixture directory name, at the workspace root. */
        const val PAGINATED_DIR_NAME = "big"
        private const val PAGINATED_DIR_PAGE_SIZE = 5

        /** 12 entries over a 5-per-page fixture spans three pages (5, 5, 2) — enough to prove a consumer follows `nextCursor` more than once, not just a single hop. */
        private val PAGINATED_DIR_ENTRIES: List<TreeEntry> = (1..12).map { i -> TreeEntry("file-$i.txt", TreeEntryKind.FILE, conflicted = false) }

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
