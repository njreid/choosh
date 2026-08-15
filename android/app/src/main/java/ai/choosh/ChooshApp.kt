package ai.choosh

import ai.choosh.agentevents.AgentAttentionTracker
import ai.choosh.agentevents.AgentEventCursorStore
import ai.choosh.agentevents.AgentEventSubscription
import ai.choosh.connection.ConnectionScreen
import ai.choosh.connection.ConnectionViewModel
import ai.choosh.connection.SessionCredentialStore
import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.FakeChooshEngine
import ai.choosh.fleet.FleetDrawer
import ai.choosh.fleet.FleetNavigationEvent
import ai.choosh.fleet.FleetViewModel
import ai.choosh.markdown.MarkdownFixtureDemoScreen
import ai.choosh.sourceeditor.SourceEditorScreen
import ai.choosh.sourceeditor.SourceEditorViewModel
import ai.choosh.terminal.TerminalScreen
import ai.choosh.webservice.WebServiceScreen
import ai.choosh.webservice.WebServiceViewModel
import ai.choosh.workspace.WorkspaceScreen
import android.content.Context
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
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
 * implementation — swap [FakeChooshEngine] for `NativeChooshEngine()` here
 * once the native bridge (a sibling increment) is ready to be exercised for
 * real; nothing else in the app needs to change.
 */
// NativeChooshEngine is real and verified (confirmed against the real
// choosh-android-bridge .so on a real device — see NativeChooshEngine.kt's
// NativeBridge doc comment) but needs a live choosh-relayd deployment to be
// useful; FakeChooshEngine stays the default until one exists to point at.
private fun buildEngine(): ChooshEngine = FakeChooshEngine()

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
    data class Workspace(val workspaceId: String, val deviceId: String) : Screen
    data class DevHostPlaceholder(val deviceId: String) : Screen

    /**
     * The `SourceEditor` pinned item (docs/specs/android-navigation.md's
     * pinned-kinds table / docs/specs/editor-protocol.md). `deviceId` is
     * needed alongside `workspaceId` because
     * `PhoneConnection::call_rpc`/`Engine.openDocument`/`saveDocument` are
     * always devhost-targeted — there is no workspace-scoped connection to
     * resolve it from implicitly.
     */
    data class SourceEditor(val deviceId: String, val workspaceId: String, val path: String) : Screen

    /**
     * The `AgentTerminal` pinned-item page, per
     * `docs/specs/android-navigation.md`'s pinned-kinds table. Reachable
     * today only from [Workspace]'s demo affordance — a real entry point
     * (tapping an actual `AgentTerminal` item in the explorer) lands with
     * `item.list` wiring into the explorer itself, a later increment.
     */
    data class Terminal(val deviceId: String, val itemId: String) : Screen

    /**
     * `WebService` pinned-item demo, per `docs/specs/service-tunnels.md`.
     * Reachable only from [Workspace]'s demo affordance, same "no real
     * `item.list` entry point wired into the explorer yet" caveat as
     * [Terminal] — `connectionHandle` is `null` here for the identical
     * reason (see [Terminal]'s doc comment and this file's `buildEngine`
     * comment), so this demonstrates the retrying-interstitial/failed
     * states genuinely, not a live gateway/`WebView` load.
     */
    data class WebServiceDemo(val deviceId: String, val workspaceId: String) : Screen

    /**
     * The Markdown/Datastar `WebView` demo, per
     * `docs/milestones/M5-web-and-markdown.md`. Unlike [WebServiceDemo],
     * this one *does* render a real `WebView` loading real
     * `choosh-web::markdown::render_markdown` output, via
     * `MarkdownGatewayBridge`'s fixture-backed start (no relay connection
     * needed) — see `MarkdownFixtureDemoScreen`'s doc comment.
     */
    data object MarkdownDemo : Screen
}

@Composable
fun ChooshApp(context: Context) {
    val engine = remember { buildEngine() }
    val credentialStore = remember { SessionCredentialStore(context) }
    // Plain `remember`, not `rememberSaveable`: `Screen` isn't `Parcelable`, and this
    // navigation state is cheap to rebuild from `ConnectionViewModel`'s own stored-credential
    // check on a configuration change anyway — not worth a custom Saver for this pass.
    var screen by remember { mutableStateOf<Screen>(Screen.Connection) }

    // docs/specs/agent-events.md's live subscription — one process-wide
    // instance, per AgentEventSubscription's own doc comment on why this
    // isn't one-per-screen. `attentionTracker` feeds FleetViewModel's
    // needsAttention badges below; `cursorStore` and the subscription
    // itself are otherwise consumed only through `subscription.subscribe`/
    // `startPolling`.
    val cursorStore = remember { AgentEventCursorStore() }
    val attentionTracker = remember { AgentAttentionTracker() }
    // `onSnapshotRequired` calls the real `workspace.status` RPC per
    // agent-events.md: "the client refreshes full workspace/item state via
    // workspace.status/workspace.list" (`workspace.list` itself isn't
    // wired into [ChooshEngine] yet — a separate, tracked gap; see
    // FleetViewModel's own doc comment — so `workspace.status` is the real
    // refresh available to wire to). Its result isn't routed into a
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
                    ConnectionScreen(viewModel = viewModel, onConnected = { screen = Screen.Fleet })
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
                            screen = when (val event = viewModel.onProjectTapped(project)) {
                                is FleetNavigationEvent.OpenWorkspace -> Screen.Workspace(event.workspaceId, event.deviceId)
                                is FleetNavigationEvent.OpenDevHost -> Screen.DevHostPlaceholder(event.deviceId)
                            }
                        },
                        onDevHostClick = { devHost -> screen = Screen.DevHostPlaceholder(devHost.deviceId) },
                        onWorkspaceClick = { workspace -> screen = Screen.Workspace(workspace.workspaceId, workspace.devHostId) },
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
                        deviceId = current.deviceId,
                        onBack = { screen = Screen.Fleet },
                        // Demo entry points — the explorer doesn't yet surface real
                        // AgentTerminal/SourceEditor pinned items to tap directly
                        // (item.list wiring is a later increment), so these open a
                        // fixed item/path against the current workspace instead.
                        onOpenTerminal = { screen = Screen.Terminal(deviceId = current.deviceId, itemId = current.workspaceId) },
                        onOpenEditor = { path -> screen = Screen.SourceEditor(current.deviceId, current.workspaceId, path) },
                        onOpenWebServiceDemo = { screen = Screen.WebServiceDemo(current.deviceId, current.workspaceId) },
                        onOpenMarkdownDemo = { screen = Screen.MarkdownDemo },
                    )
                }

                is Screen.DevHostPlaceholder -> PlaceholderScreen(
                    title = "DevHost ${current.deviceId}",
                    subtitle = "Per-devhost workspace list lands with real workspace RPCs (M1).",
                    onBack = { screen = Screen.Fleet },
                )

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
                        onBack = { screen = Screen.Workspace(current.workspaceId, current.deviceId) },
                    )
                }

                is Screen.Terminal -> TerminalScreen(
                    deviceId = current.deviceId,
                    itemId = current.itemId,
                    // No live relayd deployment exists yet (see NativeChooshEngine.kt's
                    // doc comment) — FakeChooshEngine has no connection handle to hand
                    // TerminalScreen, so real PTY attach isn't reachable from this
                    // composition root today; the demo-output buttons still exercise
                    // the real VT parser + renderer end to end.
                    connectionHandle = null,
                    onBack = { screen = Screen.Workspace(current.itemId, current.deviceId) },
                )

                is Screen.WebServiceDemo -> {
                    val viewModel: WebServiceViewModel = viewModel(
                        key = "web-service-demo:${current.deviceId}:${current.workspaceId}",
                        factory = singleInstanceFactory {
                            WebServiceViewModel(engine, current.deviceId, current.workspaceId, itemId = "item-web-1", connectionHandle = null)
                        },
                    )
                    LaunchedEffect(viewModel) { viewModel.startPolling() }
                    val state by viewModel.state.collectAsState()
                    Column(Modifier.fillMaxSize()) {
                        Button(onClick = { screen = Screen.Workspace(current.workspaceId, current.deviceId) }, modifier = Modifier.padding(8.dp)) {
                            Text("Back")
                        }
                        WebServiceScreen(state, Modifier.weight(1f))
                    }
                }

                is Screen.MarkdownDemo -> Column(Modifier.fillMaxSize()) {
                    Button(onClick = { screen = Screen.Fleet }, modifier = Modifier.padding(8.dp)) { Text("Back") }
                    MarkdownFixtureDemoScreen(Modifier.weight(1f))
                }
            }
        }
    }
}

@Composable
private fun PlaceholderScreen(title: String, subtitle: String, onBack: () -> Unit) {
    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text(title, style = MaterialTheme.typography.headlineSmall)
        Text(subtitle, modifier = Modifier.padding(top = 8.dp, bottom = 24.dp))
        Button(onClick = onBack) { Text("Back to fleet") }
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
