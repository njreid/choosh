package ai.choosh.connection

import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.WebauthnResult
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/**
 * Per auth-and-enrollment.md: a stored session credential means every later
 * app open reuses it silently (no ceremony); its absence — first launch, or
 * an explicit/forced sign-out — means a `WebAuthn` registration ceremony is
 * needed before the fleet drawer is reachable.
 *
 * The actual Android Credential Manager call lives in [ConnectionScreen]
 * (it needs an Activity context this ViewModel deliberately doesn't hold) —
 * this class owns everything either side of that: asking the engine for
 * ceremony options, handing the ceremony response back to the engine, and
 * persisting/loading the resulting credential.
 */
class ConnectionViewModel(
    private val engine: ChooshEngine,
    private val credentialStore: SessionCredentialStore,
    /**
     * Returns the current FCM token, or `null` if unavailable (no Play
     * Services, fetch failed, etc.) — injected rather than a direct
     * `FirebaseMessaging` call here so this class stays testable without
     * pulling in the Firebase SDK; the composition root supplies the real
     * implementation. `null` is a normal, non-fatal outcome: notifications
     * degrade to foreground-only, per notifications.md, not a connection
     * failure.
     */
    private val fcmTokenProvider: suspend () -> String? = { null },
) : ViewModel() {
    private val _state = MutableStateFlow<ConnectionUiState>(ConnectionUiState.CheckingStoredCredential)
    val state: StateFlow<ConnectionUiState> = _state.asStateFlow()

    init {
        viewModelScope.launch {
            val stored = credentialStore.load()
            if (stored != null) {
                connectWith(stored)
            } else {
                _state.value = ConnectionUiState.NeedsRegistration
            }
        }
    }

    /** Asks the engine for `WebAuthn` creation options to feed into Credential Manager. */
    suspend fun beginRegistration(): String {
        _state.value = ConnectionUiState.Registering
        return engine.webauthnRegisterStart()
    }

    /** Hands Credential Manager's response back to the engine and, on success, connects. */
    fun finishRegistration(credentialResponseJson: String) {
        viewModelScope.launch {
            when (val result = engine.webauthnRegisterFinish(credentialResponseJson)) {
                is WebauthnResult.Success -> {
                    credentialStore.save(result.sessionCredential)
                    connectWith(result.sessionCredential)
                }
                is WebauthnResult.Failure -> {
                    _state.value = ConnectionUiState.Error("Registration failed: ${result.message}")
                }
            }
        }
    }

    fun onRegistrationCancelledOrFailed(message: String) {
        _state.value = ConnectionUiState.Error(message)
    }

    fun retry() {
        _state.value = ConnectionUiState.NeedsRegistration
    }

    private suspend fun connectWith(sessionCredential: String) {
        _state.value = ConnectionUiState.Connecting
        val connected = runCatching { engine.connect(sessionCredential) }.getOrDefault(false)
        _state.value = if (connected) {
            registerFcmTokenBestEffort()
            ConnectionUiState.Connected
        } else {
            // A stored credential relayd no longer accepts (revoked, expired) forces a fresh
            // ceremony rather than a silent/ambiguous retry loop, per auth-and-enrollment.md's
            // revocation behavior.
            credentialStore.clear()
            ConnectionUiState.NeedsRegistration
        }
    }

    /**
     * Best-effort: a failed or unavailable FCM token MUST NOT block reaching
     * [ConnectionUiState.Connected] — background push is a backstop per
     * notifications.md, not a precondition for using the app in the
     * foreground. Failures are swallowed here deliberately, not surfaced as
     * connection errors.
     */
    private suspend fun registerFcmTokenBestEffort() {
        val token = runCatching { fcmTokenProvider() }.getOrNull() ?: return
        runCatching { engine.registerFcmToken(token) }
    }
}

sealed interface ConnectionUiState {
    data object CheckingStoredCredential : ConnectionUiState
    data object NeedsRegistration : ConnectionUiState
    data object Registering : ConnectionUiState
    data object Connecting : ConnectionUiState
    data object Connected : ConnectionUiState
    data class Error(val message: String) : ConnectionUiState
}
