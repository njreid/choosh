package ai.choosh.connection

import android.util.Log
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.credentials.CreatePublicKeyCredentialRequest
import androidx.credentials.CredentialManager
import androidx.credentials.CreateCredentialResponse
import androidx.credentials.exceptions.CreateCredentialCancellationException
import androidx.credentials.exceptions.CreateCredentialException
import androidx.credentials.exceptions.CreateCredentialInterruptedException
import androidx.credentials.exceptions.CreateCredentialNoCreateOptionException
import androidx.credentials.exceptions.CreateCredentialProviderConfigurationException
import kotlinx.coroutines.launch

/**
 * Cold-start screen: triggers the passkey registration ceremony via Android
 * Credential Manager and hands the result to [ConnectionViewModel]. This is
 * the one place in the app that talks to `androidx.credentials` directly —
 * the ceremony needs an Activity context, which a `ViewModel` deliberately
 * doesn't hold (see that class's doc comment).
 */
@Composable
fun ConnectionScreen(viewModel: ConnectionViewModel, onConnected: () -> Unit) {
    val state by viewModel.state.collectAsState()
    val context = LocalContext.current
    val scope = rememberCoroutineScope()

    if (state is ConnectionUiState.Connected) {
        onConnected()
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp)
            .testTag("connection-screen"),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text("Choosh", style = MaterialTheme.typography.headlineMedium)
        Text(
            "A personal control plane for your development fleet.",
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(top = 8.dp, bottom = 24.dp),
        )

        when (val current = state) {
            is ConnectionUiState.CheckingStoredCredential -> CircularProgressIndicator()
            is ConnectionUiState.Connecting -> {
                CircularProgressIndicator()
                Text("Connecting…", modifier = Modifier.padding(top = 8.dp))
            }
            is ConnectionUiState.Registering -> {
                CircularProgressIndicator()
                Text("Waiting for passkey…", modifier = Modifier.padding(top = 8.dp))
            }
            is ConnectionUiState.NeedsRegistration -> Column(horizontalAlignment = Alignment.CenterHorizontally) {
                Button(
                    modifier = Modifier.testTag("register-passkey-button"),
                    onClick = {
                        scope.launch {
                            runRegistrationCeremony(context, viewModel)
                        }
                    },
                ) { Text("Set up with a passkey") }
                // See ai.choosh.connection.DevPasskeyHooks's doc comment: this
                // branch is unreachable in a release build (`devPasskeyAvailable`
                // is a compile-time-fixed `false` there), not just hidden by a
                // runtime check.
                if (viewModel.devPasskeyAvailable) {
                    Button(
                        modifier = Modifier.testTag("dev-passkey-button").padding(top = 8.dp),
                        onClick = viewModel::registerWithDevPasskey,
                    ) { Text("Dev: register without a platform passkey") }
                }
            }
            is ConnectionUiState.Error -> Column(horizontalAlignment = Alignment.CenterHorizontally) {
                Text(
                    current.message,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.testTag("connection-error").padding(bottom = 16.dp),
                )
                Button(onClick = viewModel::retry) { Text("Try again") }
                if (viewModel.devPasskeyAvailable) {
                    Button(
                        modifier = Modifier.testTag("dev-passkey-button").padding(top = 8.dp),
                        onClick = viewModel::registerWithDevPasskey,
                    ) { Text("Dev: register without a platform passkey") }
                }
            }
            is ConnectionUiState.Connected -> Unit // handled by onConnected() above
        }
    }
}

private suspend fun runRegistrationCeremony(context: android.content.Context, viewModel: ConnectionViewModel) {
    val creationOptionsJson = viewModel.beginRegistration()
    val credentialManager = CredentialManager.create(context)
    try {
        val response: CreateCredentialResponse = credentialManager.createCredential(
            context = context,
            request = CreatePublicKeyCredentialRequest(requestJson = creationOptionsJson),
        )
        val responseJson = (response as? androidx.credentials.CreatePublicKeyCredentialResponse)
            ?.registrationResponseJson
        if (responseJson != null) {
            viewModel.finishRegistration(responseJson)
        } else {
            viewModel.onRegistrationCancelledOrFailed("Credential Manager returned an unexpected response type")
        }
    } catch (failure: CreateCredentialException) {
        // The raw platform errorMessage still goes to Logcat (real diagnostic value for
        // debugging a real device) — describeCreateCredentialFailure below is what the user
        // actually sees, per UX-friction audit finding #6: the raw platform string
        // ("No create options available.") was shown to the user verbatim, telling them
        // nothing about what actually went wrong or what to do about it.
        Log.w("ConnectionScreen", "passkey registration ceremony failed: ${failure.errorMessage}", failure)
        viewModel.onRegistrationCancelledOrFailed(describeCreateCredentialFailure(failure))
    }
}

/**
 * App-authored replacements for [CreateCredentialException]'s raw platform
 * [CreateCredentialException.errorMessage] — UX-friction audit finding #6.
 * Deliberately doesn't fabricate false certainty beyond what each typed
 * exception actually tells us: [CreateCredentialNoCreateOptionException]
 * specifically means no credential provider was available to even offer a
 * creation option (this project's own Genymotion test instances hit this —
 * see [DevPasskeyHooks]'s doc comment — no Google Play Services means no
 * platform passkey provider at all), so that's named directly rather than
 * left as an opaque platform string; every other case gets a plain,
 * honest description of what its own type actually represents, never a
 * guess about the underlying cause a type this generic can't actually tell
 * us. `internal` (not `private`): unit-tested directly in
 * `ConnectionScreenMessagesTest`, the same "pure derivation, unit-tested
 * directly" precedent as `ai.choosh.webservice.deriveWebServiceUiState`.
 */
internal fun describeCreateCredentialFailure(failure: CreateCredentialException): String = when (failure) {
    is CreateCredentialCancellationException -> "Passkey setup was cancelled."
    is CreateCredentialNoCreateOptionException ->
        "No passkey provider is available on this device — this usually means there's no " +
            "credential provider (e.g. Google Play Services) configured here, so a passkey can't " +
            "be created."
    is CreateCredentialProviderConfigurationException -> "This device's passkey provider isn't configured correctly."
    is CreateCredentialInterruptedException -> "Passkey setup was interrupted before it could finish. Please try again."
    else -> "Passkey setup couldn't be completed on this device."
}
