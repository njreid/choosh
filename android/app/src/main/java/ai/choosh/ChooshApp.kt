package ai.choosh

import ai.choosh.agentevents.AgentAttentionTracker
import ai.choosh.agentevents.AgentEventCursorStore
import ai.choosh.agentevents.AgentEventSubscription
import ai.choosh.connection.ConnectionScreen
import ai.choosh.connection.ConnectionViewModel
import ai.choosh.connection.SessionCredentialStore
import ai.choosh.engine.ChooshEngine
import ai.choosh.fleet.FleetDrawer
import ai.choosh.fleet.FleetNavigationEvent
import ai.choosh.fleet.FleetViewModel
import ai.choosh.markdown.MarkdownFixtureDemoScreen
import ai.choosh.notifications.NotificationDeepLinkTarget
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
     * from [Workspace]'s demo affordance (which leaves [workspaceId] `null`,
     * falling back to treating [itemId] as the workspace id below — its own
     * pre-existing, documented convention since a real `item.list` wiring
     * into the explorer is a later increment) and now also from a tapped
     * `input_required` notification's deep link (see [ChooshApp]'s
     * `pendingDeepLink` handling), which always supplies a real
     * [workspaceId] distinct from [itemId].
     */
    data class Terminal(val deviceId: String, val itemId: String, val workspaceId: String? = null) : Screen

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
    val credentialStore = remember { SessionCredentialStore(context) }
    // Plain `remember`, not `rememberSaveable`: `Screen` isn't `Parcelable`, and this
    // navigation state is cheap to rebuild from `ConnectionViewModel`'s own stored-credential
    // check on a configuration change anyway — not worth a custom Saver for this pass.
    var screen by remember { mutableStateOf<Screen>(Screen.Connection) }

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
            screen = Screen.Terminal(deviceId = target.hostId, itemId = target.itemId, workspaceId = target.workspaceId)
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
                            screen = if (target != null) {
                                Screen.Terminal(deviceId = target.hostId, itemId = target.itemId, workspaceId = target.workspaceId)
                            } else {
                                Screen.Fleet
                            }
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
                    // Hardcoded null, not yet wired to `(engine as?
                    // NativeChooshEngine)?.connectionHandle` — a real, tracked gap
                    // (UX-friction audit finding #4/#12: this composition root's demo
                    // buttons predate real item-pinning and haven't been rewired to it
                    // yet), not a "no live relayd" limitation anymore. The demo-output
                    // buttons still exercise the real VT parser + renderer end to end.
                    connectionHandle = null,
                    // `current.workspaceId` is the real workspace id when this
                    // screen was reached via a notification deep link;
                    // `?: current.itemId` preserves the pre-existing demo
                    // affordance's behavior exactly (its `itemId` IS the
                    // workspace id, per `Screen.Terminal`'s own doc comment).
                    onBack = { screen = Screen.Workspace(current.workspaceId ?: current.itemId, current.deviceId) },
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
