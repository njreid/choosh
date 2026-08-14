package ai.choosh

import ai.choosh.connection.ConnectionScreen
import ai.choosh.connection.ConnectionViewModel
import ai.choosh.connection.SessionCredentialStore
import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.FakeChooshEngine
import ai.choosh.fleet.FleetDrawer
import ai.choosh.fleet.FleetNavigationEvent
import ai.choosh.fleet.FleetViewModel
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
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewmodel.compose.viewModel

/**
 * Composition root. This is the single place that chooses [ChooshEngine]'s
 * implementation — swap [FakeChooshEngine] for `NativeChooshEngine()` here
 * once the native bridge (a sibling increment) is ready to be exercised for
 * real; nothing else in the app needs to change.
 */
private fun buildEngine(): ChooshEngine = FakeChooshEngine()

private sealed interface Screen {
    data object Connection : Screen
    data object Fleet : Screen
    data class WorkspacePlaceholder(val workspaceId: String) : Screen
    data class DevHostPlaceholder(val deviceId: String) : Screen
}

@Composable
fun ChooshApp(context: Context) {
    val engine = remember { buildEngine() }
    val credentialStore = remember { SessionCredentialStore(context) }
    // Plain `remember`, not `rememberSaveable`: `Screen` isn't `Parcelable`, and this
    // navigation state is cheap to rebuild from `ConnectionViewModel`'s own stored-credential
    // check on a configuration change anyway — not worth a custom Saver for this pass.
    var screen by remember { mutableStateOf<Screen>(Screen.Connection) }

    MaterialTheme {
        Surface(modifier = Modifier.fillMaxSize()) {
            when (val current = screen) {
                is Screen.Connection -> {
                    val viewModel: ConnectionViewModel = viewModel(
                        factory = singleInstanceFactory { ConnectionViewModel(engine, credentialStore) },
                    )
                    ConnectionScreen(viewModel = viewModel, onConnected = { screen = Screen.Fleet })
                }

                is Screen.Fleet -> {
                    val viewModel: FleetViewModel = viewModel(
                        factory = singleInstanceFactory { FleetViewModel(engine) },
                    )
                    val state by viewModel.state.collectAsState()
                    FleetDrawer(
                        state = state,
                        onSortModeSelected = viewModel::setSortMode,
                        onProjectClick = { project ->
                            screen = when (val event = viewModel.onProjectTapped(project)) {
                                is FleetNavigationEvent.OpenWorkspace -> Screen.WorkspacePlaceholder(event.workspaceId)
                                is FleetNavigationEvent.OpenDevHost -> Screen.DevHostPlaceholder(event.deviceId)
                            }
                        },
                        onDevHostClick = { devHost -> screen = Screen.DevHostPlaceholder(devHost.deviceId) },
                        onWorkspaceClick = { workspace -> screen = Screen.WorkspacePlaceholder(workspace.workspaceId) },
                    )
                }

                is Screen.WorkspacePlaceholder -> PlaceholderScreen(
                    title = "Workspace ${current.workspaceId}",
                    subtitle = "Workspace browsing/terminal/diff surfaces land in M1+.",
                    onBack = { screen = Screen.Fleet },
                )

                is Screen.DevHostPlaceholder -> PlaceholderScreen(
                    title = "DevHost ${current.deviceId}",
                    subtitle = "Per-devhost workspace list lands with real workspace RPCs (M1).",
                    onBack = { screen = Screen.Fleet },
                )
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

/** A minimal single-instance [ViewModelProvider.Factory] — no DI framework, per AGENTS.md. */
private fun <T : ViewModel> singleInstanceFactory(build: () -> T): ViewModelProvider.Factory =
    object : ViewModelProvider.Factory {
        @Suppress("UNCHECKED_CAST")
        override fun <VM : ViewModel> create(modelClass: Class<VM>): VM = build() as VM
    }
