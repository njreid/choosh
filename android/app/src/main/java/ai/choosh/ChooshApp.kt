package ai.choosh

import ai.choosh.agentevents.AgentAttentionTracker
import ai.choosh.agentevents.AgentEventCursorStore
import ai.choosh.agentevents.AgentEventSubscription
import ai.choosh.agentevents.AgentStatusTracker
import ai.choosh.connection.ConnectionScreen
import ai.choosh.connection.ConnectionViewModel
import ai.choosh.connection.RealSessionCredentialStore
import ai.choosh.engine.ChooshEngine
import ai.choosh.fleet.DevHostWorkspacesScreen
import ai.choosh.fleet.DevHostWorkspacesViewModel
import ai.choosh.fleet.FleetDrawer
import ai.choosh.fleet.FleetNavigationEvent
import ai.choosh.fleet.FleetViewModel
import ai.choosh.markdown.MarkdownScreen
import ai.choosh.markdown.MarkdownViewModel
import ai.choosh.nav.ScreenBackStack
import ai.choosh.notifications.NotificationDeepLinkTarget
import ai.choosh.sourceeditor.SourceEditorScreen
import ai.choosh.sourceeditor.SourceEditorViewModel
import ai.choosh.terminal.TerminalScreen
import ai.choosh.webservice.WebServiceScreen
import ai.choosh.webservice.WebServiceViewModel
import ai.choosh.workspace.WorkspaceScreen
import android.content.Context
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewmodel.compose.viewModel
import com.google.firebase.messaging.FirebaseMessaging
import kotlinx.coroutines.tasks.await

/**
 * Composition root. This is the single place that chooses [ChooshEngine]'s
 * implementation; nothing else in the app needs to change to swap it.
 */
// A real choosh-relayd deployment now exists (PLAN.md's "Known follow-ups"
// — a temporary EC2 test instance, fronted by real Let's Encrypt TLS/DNS),
// so NativeChooshEngine — real and verified against the real
// choosh-android-bridge .so on a real device, see NativeChooshEngine.kt's
// NativeBridge doc comment — is now the real default, per this file's own
// previously-stated intent ("once one exists to point at"). A build with
// no reachable CHOOSH_RELAYD_URL configured (the release default,
// wss://relay.choosh.ai/connect, has no live deployment yet) will show
// real connection-failure states rather than FakeChooshEngine's polished
// fixture demo — an accurate reflection of where the project actually is,
// not a regression to paper over.
private fun buildEngine(): ChooshEngine = NativeChooshEngine()

/**
 * `null` on any failure (no Play Services, transient error, etc.) — a
 * missing FCM token is a normal, non-fatal outcome for the caller
 * ([ai.choosh.connection.ConnectionViewModel] treats it as "notifications
 * degrade to foreground-only", not a connection failure).
 */
private suspend fun fetchFcmToken(): String? = runCatching { FirebaseMessaging.getInstance().token.await() }.getOrNull()

private sealed interface Screen {
    data object Connection : Screen
    data object Fleet : Screen
    /**
     * `workspaceName` is [ai.choosh.fleet.Workspace.name] — carried through
     * from wherever this screen was navigated to from (see
     * [ai.choosh.fleet.FleetNavigationEvent.OpenWorkspace]'s own doc
     * comment), so [ai.choosh.workspace.WorkspaceScreen] can title itself
     * with the real name instead of the raw [workspaceId] (UX-friction
     * audit finding #11). `null` only where genuinely unavailable at the
     * navigating call site — [ai.choosh.workspace.WorkspaceScreen] falls
     * back to [workspaceId] in that case, never a crash or a fabricated name.
     */
    data class Workspace(val workspaceId: String, val deviceId: String, val workspaceName: String? = null) : Screen

    /**
     * The real per-devhost Workspace list (docs/specs/android-navigation.md's
     * "Workspace entry": `Fleet drawer -> Workspace list -> Workspace`),
     * backed by `workspace.list` via [DevHostWorkspacesViewModel]. Replaces
     * the old `DevHostPlaceholder` literal placeholder screen — the
     * UX-friction audit's finding #3 ("Fleet drawer can't actually reach
     * most workspaces... Host mode renders `DevHostRow`s only, and tapping
     * one routes to `Screen.DevHostPlaceholder`... a literal placeholder
     * screen").
     */
    data class DevHostWorkspaces(val deviceId: String) : Screen

    /**
     * The `SourceEditor` pinned item (docs/specs/android-navigation.md's
     * pinned-kinds table / docs/specs/editor-protocol.md). `deviceId` is
     * needed alongside `workspaceId` because
     * `PhoneConnection::call_rpc`/`Engine.openDocument`/`saveDocument` are
     * always devhost-targeted — there is no workspace-scoped connection to
     * resolve it from implicitly. Reachable from the Explorer's searchable
     * project tree (a non-Markdown file) or its own real demo affordance
     * inside [ai.choosh.workspace.WorkspaceScreen].
     */
    data class SourceEditor(val deviceId: String, val workspaceId: String, val path: String) : Screen

    /**
     * The `AgentTerminal` pinned-item page, per
     * `docs/specs/android-navigation.md`'s pinned-kinds table. `itemId` is
     * a real `item.list`/`item.create` id — reachable by tapping a real row
     * in the Explorer's "active agents" section, or from a tapped
     * `input_required` notification's deep link (see [ChooshApp]'s
     * `pendingDeepLink` handling). `workspaceId` is carried alongside purely
     * for [onBack] to return to the right Workspace — it plays no role in
     * [TerminalScreen]'s own PTY-attach logic, which is
     * `(deviceId, itemId)`-addressed per `terminal-experience.md`.
     *
     * `itemName`: same "thread the real name through instead of showing a
     * raw id" fix as [Workspace.workspaceName] (UX-friction audit finding
     * #11) — there is no real named `AgentTerminal` item to source this
     * from yet, so this is the current [Workspace]'s own name, the closest
     * genuinely-available name today. `null` falls back to [itemId]/
     * [deviceId], unchanged from before this field existed.
     */
    data class Terminal(val deviceId: String, val itemId: String, val workspaceId: String, val itemName: String? = null) : Screen

    /**
     * The real `WebService` pinned item, per `docs/specs/service-tunnels.md`.
     * `itemId` is a real `item.list`/`item.create` id, reached by tapping a
     * real row in the Explorer's "registered development services"
     * section — replaces the old `WebServiceDemo` screen, which always
     * polled a hardcoded `"item-web-1"` regardless of what the tapped
     * workspace actually had registered.
     */
    data class WebService(val deviceId: String, val workspaceId: String, val itemId: String) : Screen

    /**
     * The `MarkdownPreview` pinned item, per
     * `docs/specs/android-navigation.md`'s pinned-kinds table /
     * `docs/milestones/M5-web-and-markdown.md`. `path` is a real
     * `workspace.tree.list`-discovered `.md`/`.markdown` file, reached by
     * tapping it in the Explorer's searchable project tree — replaces the
     * old fixture-only `MarkdownDemo` screen.
     */
    data class MarkdownPreview(val deviceId: String, val workspaceId: String, val path: String) : Screen
}

@Composable
fun ChooshApp(
    context: Context,
    // A tapped `input_required` notification's resolved target, per
    // `MainActivity`'s `onCreate`/`onNewIntent` handling and
    // docs/specs/android-navigation.md's "Notification deep link" section.
    // `null` (the default) preserves every existing call site/preview/test
    // exactly as before.
    pendingDeepLink: NotificationDeepLinkTarget.OpenItem? = null,
    // Called once [pendingDeepLink] has been acted on (either navigated to
    // directly below, or handed off to `ConnectionScreen`'s `onConnected`)
    // so `MainActivity` clears its own copy — otherwise a later
    // configuration change / recomposition would re-navigate away from
    // wherever the user went next.
    onDeepLinkConsumed: () -> Unit = {},
) {
    val engine = remember { buildEngine() }
    val credentialStore = remember { RealSessionCredentialStore(context) }
    // A real back stack (UX-friction audit finding #2), not a single
    // `remember { mutableStateOf(...) }` slot: `docs/specs/android-navigation.md`'s
    // "Back behavior" section requires popping one level at a time
    // (pinned page -> explorer -> workspace list -> fleet -> connection),
    // which a single current-screen variable can't represent — there's
    // nowhere to remember what came before. Plain `remember`, not
    // `rememberSaveable`: `Screen` isn't `Parcelable`, and this navigation
    // state is cheap to rebuild from `ConnectionViewModel`'s own
    // stored-credential check on a configuration change anyway — not worth
    // a custom Saver for this pass.
    val backStack = remember { ScreenBackStack<Screen>(Screen.Connection) }
    val screen = backStack.current
    // System Back pops one level, per android-navigation.md's "Back
    // behavior" — enabled only when there's somewhere to pop back to; at
    // the root (`Connection`, with nothing pushed yet) this composable
    // lets the platform's own default back handling apply (exit the app),
    // same as any other single-Activity app's root screen.
    BackHandler(enabled = backStack.canPop) { backStack.pop() }

    // Per notifications.md's "Actionability" section ("tapping connects if
    // necessary, opens the workspace, ensures the relevant item is pinned,
    // focuses it, and acknowledges the notification") and
    // docs/specs/android-navigation.md's "Notification deep link" section.
    // "Acknowledges" (step 6, and step 1's "connects if necessary") are
    // handled outside this function: `MainActivity.consumeDeepLink` already
    // cancelled the Android notification by the time [pendingDeepLink]
    // reaches here, and reaching [Screen.Connection] below already means
    // `ConnectionViewModel` is mid-connect (or about to be) via its own
    // `init` block — there is no separate "is connected" flag to check.
    //
    // What remains — "opens the workspace, ensures the relevant item is
    // pinned, focuses it" (steps 2/3/4/5) — is approximated here as
    // "navigate straight to `Screen.Terminal`": this codebase has no real
    // per-workspace pin list yet (`WorkspaceScreen` renders three fixed
    // tabs, not a pinned-item set), so there is nothing to insert an
    // `AgentTerminal` pin into. `Screen.Terminal` is this codebase's
    // existing stand-in for "the pinned/focused `AgentTerminal` page" (see
    // its own doc comment) — landing there directly is the closest
    // available approximation of "pinned and focused" until a real pin
    // list exists. A human merging this with the concurrent `Screen`/
    // back-stack rewrite should reconcile this against real pin state once
    // it lands, and should decide whether the back stack should also
    // contain [Screen.Workspace] underneath (this pass jumps straight to
    // [Screen.Terminal] without visiting it, since nothing here reads or
    // depends on Workspace-screen state first).
    //
    // If [screen] is already [Screen.Connection], this effect intentionally
    // does nothing — `ConnectionScreen`'s `onConnected` callback below picks
    // up [pendingDeepLink] itself once `ConnectionViewModel` reaches
    // `Connected`, since navigating here would race an as-yet-unauthenticated
    // connection.
    LaunchedEffect(pendingDeepLink) {
        val target = pendingDeepLink ?: return@LaunchedEffect
        if (screen !is Screen.Connection) {
            backStack.push(Screen.Terminal(deviceId = target.hostId, itemId = target.itemId, workspaceId = target.workspaceId))
            onDeepLinkConsumed()
        }
    }

    // docs/specs/agent-events.md's live subscription — one process-wide
    // instance, per AgentEventSubscription's own doc comment on why this
    // isn't one-per-screen. `attentionTracker` feeds FleetViewModel's
    // needsAttention badges below; `cursorStore` and the subscription
    // itself are otherwise consumed only through `subscription.subscribe`/
    // `startPolling`.
    val cursorStore = remember { AgentEventCursorStore() }
    val attentionTracker = remember { AgentAttentionTracker() }
    // The per-item `AgentRunStatus` companion to `attentionTracker`'s
    // workspace-level flag (see AgentStatusTracker's own doc comment) —
    // feeds the explorer's "active agents" section's real `busy`/`waiting`/
    // `starting`/`failed` distinction, which `item.list` alone cannot
    // provide.
    val statusTracker = remember { AgentStatusTracker() }
    // `onSnapshotRequired` calls the real `workspace.status` RPC per
    // agent-events.md: "the client refreshes full workspace/item state via
    // workspace.status/workspace.list". Its result isn't routed into a
    // specific screen's visible state here: [WorkspaceScreen] constructs
    // its own [ai.choosh.explorer.ExplorerViewModel] internally rather than
    // the composition root holding a reference to it, so there's no
    // existing hook to push a background refresh's result into — that
    // workspace's own ExplorerViewModel re-fetches current state via its
    // own `init { refresh() }` the next time it's opened regardless. A
    // real cross-screen workspace-status cache is future work, not
    // something this pass silently pretends to have.
    val subscription = remember {
        AgentEventSubscription(
            engine = engine,
            cursorStore = cursorStore,
            attentionTracker = attentionTracker,
            onSnapshotRequired = { deviceId, workspaceId -> runCatching { engine.workspaceStatus(deviceId, workspaceId) } },
            statusTracker = statusTracker,
        )
    }
    val agentEventPollScope = rememberCoroutineScope()
    LaunchedEffect(subscription) { subscription.startPolling(agentEventPollScope) }

    MaterialTheme {
        Surface(modifier = Modifier.fillMaxSize()) {
            when (val current = screen) {
                is Screen.Connection -> {
                    val viewModel: ConnectionViewModel = viewModel(
                        factory = singleInstanceFactory {
                            ConnectionViewModel(engine, credentialStore, fcmTokenProvider = ::fetchFcmToken)
                        },
                    )
                    ConnectionScreen(
                        viewModel = viewModel,
                        onConnected = {
                            // Per notifications.md's "Actionability": "tapping
                            // connects if necessary [...]". A deep-linked
                            // notification tap that arrived while unauthenticated
                            // (cold start, or the app was on Screen.Connection
                            // already) lands here once `ConnectionViewModel`
                            // finishes connecting — route to the deep link's
                            // target instead of the plain fleet drawer, then
                            // consume it exactly once (mirrors the other
                            // consumption site in the `LaunchedEffect` above).
                            val target = pendingDeepLink
                            backStack.push(
                                if (target != null) {
                                    Screen.Terminal(deviceId = target.hostId, itemId = target.itemId, workspaceId = target.workspaceId)
                                } else {
                                    Screen.Fleet
                                },
                            )
                            if (target != null) onDeepLinkConsumed()
                        },
                    )
                }

                is Screen.Fleet -> {
                    val viewModel: FleetViewModel = viewModel(
                        factory = singleInstanceFactory { FleetViewModel(engine, attentionTracker) },
                    )
                    val state by viewModel.state.collectAsState()
                    FleetDrawer(
                        state = state,
                        onSortModeSelected = viewModel::setSortMode,
                        onProjectClick = { project ->
                            backStack.push(
                                when (val event = viewModel.onProjectTapped(project)) {
                                    is FleetNavigationEvent.OpenWorkspace -> Screen.Workspace(event.workspaceId, event.deviceId, event.workspaceName)
                                    is FleetNavigationEvent.OpenDevHost -> Screen.DevHostWorkspaces(event.deviceId)
                                },
                            )
                        },
                        onDevHostClick = { devHost -> backStack.push(Screen.DevHostWorkspaces(devHost.deviceId)) },
                        onWorkspaceClick = { workspace -> backStack.push(Screen.Workspace(workspace.workspaceId, workspace.devHostId, workspace.name)) },
                        onRequestEnrollmentToken = viewModel::requestEnrollmentToken,
                        onDismissEnrollmentToken = viewModel::dismissEnrollmentToken,
                    )
                }

                is Screen.Workspace -> {
                    // Subscribes this workspace to the live agent-event
                    // stream the moment it's opened — idempotent/re-entrant
                    // per AgentEventSubscription.subscribe's own doc
                    // comment, so this is safe on every recomposition, not
                    // just the first.
                    LaunchedEffect(current.workspaceId, current.deviceId) {
                        subscription.subscribe(current.deviceId, current.workspaceId)
                    }
                    WorkspaceScreen(
                        engine = engine,
                        workspaceId = current.workspaceId,
                        workspaceName = current.workspaceName,
                        deviceId = current.deviceId,
                        onBack = { backStack.pop() },
                        statusTracker = statusTracker,
                        // Real item-pinning entry points — every one of these
                        // is a real `item.list`/`item.create`/`workspace.tree.list`
                        // id/path resolved by ExplorerViewModel, never a
                        // hardcoded demo item (see WorkspaceScreen.kt's own
                        // doc comment on why the old fixed demo buttons are
                        // gone).
                        onOpenAgentTerminal = { itemId ->
                            backStack.push(Screen.Terminal(current.deviceId, itemId, current.workspaceId, itemName = current.workspaceName))
                        },
                        onOpenWebService = { itemId -> backStack.push(Screen.WebService(current.deviceId, current.workspaceId, itemId)) },
                        onOpenSourceEditor = { path -> backStack.push(Screen.SourceEditor(current.deviceId, current.workspaceId, path)) },
                        onOpenMarkdownPreview = { path -> backStack.push(Screen.MarkdownPreview(current.deviceId, current.workspaceId, path)) },
                    )
                }

                is Screen.DevHostWorkspaces -> {
                    val viewModel: DevHostWorkspacesViewModel = viewModel(
                        key = "devhost-workspaces:${current.deviceId}",
                        factory = singleInstanceFactory { DevHostWorkspacesViewModel(engine, current.deviceId) },
                    )
                    val state by viewModel.state.collectAsState()
                    DevHostWorkspacesScreen(
                        deviceId = current.deviceId,
                        state = state,
                        onWorkspaceClick = { workspace -> backStack.push(Screen.Workspace(workspace.workspaceId, workspace.devHostId)) },
                        onRefresh = viewModel::refresh,
                        onBack = { backStack.pop() },
                    )
                }

                is Screen.SourceEditor -> {
                    val viewModel: SourceEditorViewModel = viewModel(
                        key = "source-editor:${current.deviceId}:${current.workspaceId}:${current.path}",
                        factory = singleInstanceFactory {
                            SourceEditorViewModel(engine, current.deviceId, current.workspaceId, current.path)
                        },
                    )
                    SourceEditorScreen(
                        viewModel = viewModel,
                        path = current.path,
                        onBack = { backStack.pop() },
                    )
                }

                is Screen.Terminal -> TerminalScreen(
                    deviceId = current.deviceId,
                    itemId = current.itemId,
                    itemName = current.itemName,
                    // Real connection handle, per this file's `buildEngine`
                    // comment (`NativeChooshEngine` is the real default now)
                    // — `null` only when running against `FakeChooshEngine`
                    // (no real relay connection to attach a PTY tunnel
                    // through), in which case the extra-keys bar's demo
                    // output buttons remain the only way to see anything
                    // render, exactly as documented on that button.
                    connectionHandle = (engine as? NativeChooshEngine)?.connectionHandle,
                    onBack = { backStack.pop() },
                )

                is Screen.WebService -> {
                    val viewModel: WebServiceViewModel = viewModel(
                        key = "web-service:${current.deviceId}:${current.workspaceId}:${current.itemId}",
                        factory = singleInstanceFactory {
                            WebServiceViewModel(
                                engine,
                                current.deviceId,
                                current.workspaceId,
                                itemId = current.itemId,
                                connectionHandle = (engine as? NativeChooshEngine)?.connectionHandle,
                            )
                        },
                    )
                    LaunchedEffect(viewModel) { viewModel.startPolling() }
                    val state by viewModel.state.collectAsState()
                    Column(Modifier.fillMaxSize()) {
                        BackRow(onBack = { backStack.pop() })
                        WebServiceScreen(state, Modifier.weight(1f), onRetry = viewModel::retry)
                    }
                }

                is Screen.MarkdownPreview -> {
                    val viewModel: MarkdownViewModel = viewModel(
                        key = "markdown-preview:${current.deviceId}:${current.workspaceId}:${current.path}",
                        factory = singleInstanceFactory {
                            MarkdownViewModel(current.deviceId, current.workspaceId, current.path, (engine as? NativeChooshEngine)?.connectionHandle)
                        },
                    )
                    // Explicitly stops this document's local gateway when the
                    // screen leaves composition — MarkdownViewModel.onClose's
                    // own doc comment is exactly this "screen teardown" case.
                    DisposableEffect(viewModel) { onDispose { viewModel.onClose() } }
                    val state by viewModel.state.collectAsState()
                    Column(Modifier.fillMaxSize()) {
                        BackRow(onBack = { backStack.pop() })
                        MarkdownScreen(state, Modifier.weight(1f))
                    }
                }
            }
        }
    }
}

/**
 * The standard top-left Back affordance (UX-friction audit finding #10):
 * an `IconButton` + `ArrowBack`, matching [ai.choosh.sourceeditor.SourceEditorScreen]'s
 * existing pattern — the one other screens previously diverged from (a
 * top-left text `Button("Back")` in [ai.choosh.workspace.WorkspaceScreen]/here,
 * a top-*right* text `Button("Back")` in [ai.choosh.terminal.TerminalScreen]).
 * Used here for the screens this composition root renders inline
 * ([Screen.WebService]/[Screen.MarkdownPreview]).
 */
@Composable
private fun BackRow(onBack: () -> Unit) {
    Row(modifier = Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        IconButton(onClick = onBack) {
            Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
        }
    }
}

/**
 * A minimal single-instance [ViewModelProvider.Factory] — no DI framework, per AGENTS.md.
 * `internal`, not `private`: [ai.choosh.workspace.WorkspaceScreen] needs this identical
 * factory for its own per-tab ViewModels, so this is the one shared definition rather than
 * two independently-forked copies.
 */
internal fun <T : ViewModel> singleInstanceFactory(build: () -> T): ViewModelProvider.Factory =
    object : ViewModelProvider.Factory {
        @Suppress("UNCHECKED_CAST")
        override fun <VM : ViewModel> create(modelClass: Class<VM>): VM = build() as VM
    }
