package ai.choosh.connection

import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.ConnectResult
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
    /**
     * Invoked every time [connectWith] reaches [ConnectResult.Connected] —
     * both the very first connect ([init]'s stored-credential path) and
     * every later reconnect ([retry]'s). The composition root wires this to
     * [ai.choosh.agentevents.AgentEventSubscription.resubscribeAll], per
     * that method's own doc comment: "call this after a fresh
     * `ChooshEngine.connect` succeeds (a reconnect) ... MUST resume from the
     * last acknowledged sequence." Calling it unconditionally on every
     * successful connect (not just ones this class can itself identify as
     * "a reconnect" rather than "the first connect") is deliberate and safe:
     * on a genuine first connect [resubscribeAll]-equivalent has nothing
     * registered yet to resubscribe, so this is a no-op; the default no-op
     * lambda preserves every existing call site/test that doesn't pass one.
     */
    private val onConnectSucceeded: suspend () -> Unit = {},
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

    /**
     * Whether [registerWithDevPasskey] can do anything — see
     * [DevPasskeyHooks]'s doc comment. Always `false` in a release build;
     * [ConnectionScreen] uses this to decide whether to show that option
     * at all, so a release build's UI never even offers it.
     */
    val devPasskeyAvailable: Boolean get() = DevPasskeyHooks.available

    /**
     * The [DevPasskeyHooks] equivalent of [ConnectionScreen]'s
     * `runRegistrationCeremony` — self-contained here rather than split
     * across the screen/view-model boundary the way the real ceremony is,
     * since [DevPasskeyHooks.register] needs no `Activity` context.
     */
    fun registerWithDevPasskey() {
        viewModelScope.launch {
            val creationOptionsJson = beginRegistration()
            val result = runCatching { DevPasskeyHooks.register(creationOptionsJson) }
            result.fold(
                onSuccess = ::finishRegistration,
                onFailure = { failure -> onRegistrationCancelledOrFailed("Dev passkey registration failed: ${failure.message}") },
            )
        }
    }

    /**
     * Credential-aware retry: if a session credential is still stored (a
     * prior [connectWith] hit [ConnectResult.TransportFailure], which
     * deliberately does NOT clear it — see that method's doc comment),
     * this re-attempts [connectWith] against it rather than forcing a full
     * `WebAuthn` ceremony. Only when nothing is stored (either first launch,
     * or a prior [ConnectResult.Rejected] already cleared it) does this fall
     * back to [ConnectionUiState.NeedsRegistration]. This mirrors [init]'s
     * own "load stored credential, connect if present" shape rather than
     * duplicating it inline.
     */
    fun retry() {
        viewModelScope.launch {
            val stored = credentialStore.load()
            if (stored != null) {
                connectWith(stored)
            } else {
                _state.value = ConnectionUiState.NeedsRegistration
            }
        }
    }

    private suspend fun connectWith(sessionCredential: String) {
        _state.value = ConnectionUiState.Connecting
        when (val result = runCatching { engine.connect(sessionCredential) }.getOrElse { failure ->
            ConnectResult.TransportFailure(failure.message ?: "connect failed")
        }) {
            is ConnectResult.Connected -> {
                registerFcmTokenBestEffort()
                onConnectSucceeded()
                _state.value = ConnectionUiState.Connected
            }
            is ConnectResult.Rejected -> {
                // relayd itself rejected this credential (revoked, expired) — forces a fresh
                // ceremony rather than a silent/ambiguous retry loop, per auth-and-enrollment.md's
                // revocation behavior. A plain connectivity failure (ConnectResult.TransportFailure,
                // below) deliberately does NOT do this: UX-friction audit finding #5 was exactly this
                // distinction missing, when this method's only signal was a bare Boolean.
                credentialStore.clear()
                _state.value = ConnectionUiState.NeedsRegistration
            }
            is ConnectResult.TransportFailure -> {
                // The stored credential may still be perfectly valid — only unreachable right now.
                // Left in place so retry() (and the next app open) can use it again, no re-registration.
                _state.value = ConnectionUiState.Error("Couldn't connect: ${result.message}")
            }
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
