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
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
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
import com.google.mlkit.vision.codescanner.GmsBarcodeScanning
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

/**
 * The literal prefix of a Choosh pairing QR code's raw scanned text —
 * `choosh-pair:v1:<secret>`, printed by the operator-side `choosh-relayd
 * pair` CLI subcommand. Fixed contract, not this app's to change: a scan
 * that doesn't start with this exact prefix is rejected as "not a Choosh
 * pairing code" rather than silently proceeding with garbage or failing
 * unexplained.
 */
private const val PAIRING_QR_PREFIX = "choosh-pair:v1:"

/**
 * Cold-start screen: triggers the passkey registration ceremony via Android
 * Credential Manager and hands the result to [ConnectionViewModel]. This is
 * the one place in the app that talks to `androidx.credentials` directly —
 * the ceremony needs an Activity context, which a `ViewModel` deliberately
 * doesn't hold (see that class's doc comment).
 *
 * [ConnectionUiState.NeedsRegistration] is a deliberately two-step affordance
 * per the app's "no settings panel, just scan a QR code" pairing design:
 * this app is a generic, reproducibly-built Obtainium APK with no
 * per-installation secret baked in, so `relayd` now requires the operator's
 * bootstrap secret (see `PAIRING_QR_PREFIX`'s doc comment) before it will
 * even start a `WebAuthn` ceremony. Only after a successful scan does the
 * "Set up with a passkey" button (and, in a debug build, the dev-passkey
 * button) appear at all — the scanned secret is held purely in this
 * composable's own [remember]ed state, never handed to [ConnectionViewModel]
 * until the ceremony itself begins, and never persisted anywhere; backing
 * out before finishing the ceremony just discards it on recomposition.
 */
@Composable
fun ConnectionScreen(viewModel: ConnectionViewModel, onConnected: () -> Unit) {
    val state by viewModel.state.collectAsState()
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var bootstrapSecret by remember { mutableStateOf<String?>(null) }

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
                val secret = bootstrapSecret
                if (secret == null) {
                    Button(
                        modifier = Modifier.testTag("scan-pairing-qr-button"),
                        onClick = {
                            scope.launch {
                                val scanned = try {
                                    scanPairingQrCode(context)
                                } catch (cancellation: kotlinx.coroutines.CancellationException) {
                                    // This coroutine's own structured-concurrency cancellation
                                    // (e.g. leaving composition mid-scan) — MUST propagate, never
                                    // be mistaken for a genuine scanner failure below.
                                    throw cancellation
                                } catch (failure: Exception) {
                                    Log.w("ConnectionScreen", "pairing QR scan failed", failure)
                                    viewModel.onRegistrationCancelledOrFailed("Couldn't scan the pairing QR code: ${failure.message}")
                                    return@launch
                                }
                                if (scanned == null) {
                                    // The user backed out of the scanner without completing a scan —
                                    // not an error, just stay on this same "Scan QR to pair" button.
                                    return@launch
                                }
                                if (scanned.startsWith(PAIRING_QR_PREFIX)) {
                                    bootstrapSecret = scanned.removePrefix(PAIRING_QR_PREFIX)
                                } else {
                                    viewModel.onRegistrationCancelledOrFailed("That QR code doesn't look like a Choosh pairing code.")
                                }
                            }
                        },
                    ) { Text("Scan QR to pair") }
                } else {
                    Button(
                        modifier = Modifier.testTag("register-passkey-button"),
                        onClick = {
                            scope.launch {
                                runRegistrationCeremony(context, viewModel, secret)
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
                            onClick = { viewModel.registerWithDevPasskey(secret) },
                        ) { Text("Dev: register without a platform passkey") }
                    }
                }
            }
            is ConnectionUiState.Error -> Column(horizontalAlignment = Alignment.CenterHorizontally) {
                Text(
                    current.message,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.testTag("connection-error").padding(bottom = 16.dp),
                )
                Button(onClick = viewModel::retry) { Text("Try again") }
                // Only offered once a pairing secret is actually held (e.g. this
                // Error followed a failed ceremony after a successful QR scan) —
                // an Error reached via a plain connect/transport failure, or an
                // invalid-QR rejection before any secret was ever scanned, has no
                // secret to register with, so this button stays hidden rather than
                // calling registerWithDevPasskey with nothing meaningful to send.
                if (viewModel.devPasskeyAvailable && bootstrapSecret != null) {
                    Button(
                        modifier = Modifier.testTag("dev-passkey-button").padding(top = 8.dp),
                        onClick = { viewModel.registerWithDevPasskey(bootstrapSecret!!) },
                    ) { Text("Dev: register without a platform passkey") }
                }
            }
            is ConnectionUiState.Connected -> Unit // handled by onConnected() above
        }
    }
}

/**
 * Opens Google Play Services' Code Scanner (`GmsBarcodeScanning`) — no
 * camera permission needed, hosts its own scanner UI — and returns the
 * scanned barcode's raw text, or `null` if the user backed out of the
 * scanner without completing a scan. A genuine scanner failure (as opposed
 * to a user cancel) is rethrown, matching this screen's existing
 * `CreateCredentialException`-only `try`/`catch` precedent in
 * [runRegistrationCeremony] of only swallowing the specific "the user
 * didn't complete this" case, not every possible failure. A manual
 * [suspendCancellableCoroutine] wrapper (rather than this project's already-present
 * `kotlinx-coroutines-play-services` `Task.await()` extension) is used
 * deliberately: `Task.await()` surfaces a cancelled `Task` as a
 * `CancellationException`, which is awkward to distinguish from this
 * coroutine's own structured-concurrency cancellation — resuming the
 * continuation with a plain `null` here keeps "user cancelled the scan"
 * an ordinary, unambiguous return value instead.
 */
private suspend fun scanPairingQrCode(context: android.content.Context): String? = suspendCancellableCoroutine { continuation ->
    val scanner = GmsBarcodeScanning.getClient(context)
    scanner.startScan()
        .addOnSuccessListener { barcode -> continuation.resume(barcode.rawValue) }
        .addOnCanceledListener { continuation.resume(null) }
        .addOnFailureListener { failure -> continuation.resumeWithException(failure) }
}

private suspend fun runRegistrationCeremony(context: android.content.Context, viewModel: ConnectionViewModel, bootstrapSecret: String) {
    val creationOptionsJson = viewModel.beginRegistration(bootstrapSecret)
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
