package ai.choosh.connection

import ai.choosh.engine.FakeChooshEngine
import ai.choosh.fleet.MainDispatcherRule
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/**
 * In-memory [SessionCredentialStore], mirroring
 * `ai.choosh.webservice.WebServiceViewModelTest`'s `FakeWebServiceGatewayController`-style
 * fakes elsewhere in this test suite — [RealSessionCredentialStore]'s
 * Android Keystore dependency has no JVM unit test equivalent, the reason
 * [SessionCredentialStore] is an interface at all (see that file's own doc
 * comment).
 */
private class FakeSessionCredentialStore(initial: String? = null) : SessionCredentialStore {
    private var stored: String? = initial

    override fun load(): String? = stored
    override fun save(sessionCredential: String) {
        stored = sessionCredential
    }
    override fun clear() {
        stored = null
    }
}

/**
 * [ConnectionViewModel.connectWith]'s core UX-friction audit finding #5 fix:
 * a genuine relayd rejection ([ai.choosh.engine.ConnectResult.Rejected])
 * MUST clear the stored credential and force a fresh registration ceremony,
 * while a plain connectivity failure
 * ([ai.choosh.engine.ConnectResult.TransportFailure]) MUST NOT — the
 * credential may still be perfectly valid, only unreachable right now.
 * [ConnectionViewModel.retry]'s credential-aware branching (reconnect with
 * the still-stored credential vs. fall back to [ConnectionUiState.NeedsRegistration])
 * is exercised here too, since it's what actually makes the distinction
 * meaningful to a user.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ConnectionViewModelTest {
    @get:Rule
    val mainDispatcherRule = MainDispatcherRule()

    @Test
    fun `a stored credential connects automatically on init`() = runTest(mainDispatcherRule.dispatcher) {
        val credentialStore = FakeSessionCredentialStore(initial = "stored-cred")
        val viewModel = ConnectionViewModel(FakeChooshEngine(), credentialStore)
        advanceUntilIdle()

        assertEquals(ConnectionUiState.Connected, viewModel.state.value)
    }

    @Test
    fun `no stored credential shows NeedsRegistration on init`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ConnectionViewModel(FakeChooshEngine(), FakeSessionCredentialStore())
        advanceUntilIdle()

        assertEquals(ConnectionUiState.NeedsRegistration, viewModel.state.value)
    }

    @Test
    fun `a genuine rejection clears the stored credential and requires fresh registration`() = runTest(mainDispatcherRule.dispatcher) {
        val credentialStore = FakeSessionCredentialStore(initial = "revoked-cred")
        val engine = FakeChooshEngine().apply { simulateConnectRejection = "session credential revoked" }
        val viewModel = ConnectionViewModel(engine, credentialStore)
        advanceUntilIdle()

        assertEquals(ConnectionUiState.NeedsRegistration, viewModel.state.value)
        assertNull("a genuine rejection must wipe the stored credential", credentialStore.load())
    }

    @Test
    fun `a transport failure keeps the stored credential and surfaces a retryable error`() = runTest(mainDispatcherRule.dispatcher) {
        val credentialStore = FakeSessionCredentialStore(initial = "still-valid-cred")
        val engine = FakeChooshEngine().apply { simulateConnectTransportFailure = "timed out reaching relayd" }
        val viewModel = ConnectionViewModel(engine, credentialStore)
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue("a transport failure must surface as an Error state, not NeedsRegistration", state is ConnectionUiState.Error)
        assertTrue(
            "the error message should carry the transport failure's own message",
            (state as ConnectionUiState.Error).message.contains("timed out reaching relayd"),
        )
        assertEquals(
            "a transport failure must NOT clear a still-possibly-valid stored credential",
            "still-valid-cred",
            credentialStore.load(),
        )
    }

    @Test
    fun `retry after a transport failure reconnects with the still-stored credential, not a fresh ceremony`() =
        runTest(mainDispatcherRule.dispatcher) {
            val credentialStore = FakeSessionCredentialStore(initial = "still-valid-cred")
            val engine = FakeChooshEngine().apply { simulateConnectTransportFailure = "timed out reaching relayd" }
            val viewModel = ConnectionViewModel(engine, credentialStore)
            advanceUntilIdle()
            assertTrue(viewModel.state.value is ConnectionUiState.Error)

            // The simulated failure is one-shot (see FakeChooshEngine.connect); retry() should
            // reconnect using the credential retry() itself reloads from the store, which was
            // never cleared, succeeding this time.
            viewModel.retry()
            advanceUntilIdle()

            assertEquals(ConnectionUiState.Connected, viewModel.state.value)
        }

    @Test
    fun `retry with no stored credential falls back to NeedsRegistration`() = runTest(mainDispatcherRule.dispatcher) {
        val credentialStore = FakeSessionCredentialStore(initial = "revoked-cred")
        val engine = FakeChooshEngine().apply { simulateConnectRejection = "session credential revoked" }
        val viewModel = ConnectionViewModel(engine, credentialStore)
        advanceUntilIdle()
        assertEquals(ConnectionUiState.NeedsRegistration, viewModel.state.value)
        assertNull(credentialStore.load())

        // Tapping "Try again" from NeedsRegistration's own Error-branch precedent: nothing is
        // stored anymore, so this must stay NeedsRegistration rather than attempt a connect with
        // no credential to connect with.
        viewModel.retry()
        advanceUntilIdle()

        assertEquals(ConnectionUiState.NeedsRegistration, viewModel.state.value)
    }

    // --- onConnectSucceeded / AgentEventSubscription.resubscribeAll wiring ---
    //
    // Per AgentEventSubscription.resubscribeAll's own doc comment, a reconnect after any
    // gap (network loss, app backgrounding, the relay connection cycling) "MUST resume
    // from the last acknowledged sequence ... MUST NOT silently drop events" — this was,
    // until this pass, never actually called anywhere in production. ConnectionViewModel
    // exposes onConnectSucceeded as the hook the composition root wires resubscribeAll
    // through; these tests exercise that ConnectionViewModel itself invokes it reliably,
    // independent of whatever AgentEventSubscription happens to be wired in production.

    @Test
    fun `a successful connect invokes onConnectSucceeded`() = runTest(mainDispatcherRule.dispatcher) {
        val credentialStore = FakeSessionCredentialStore(initial = "stored-cred")
        var connectSucceededCount = 0
        val viewModel = ConnectionViewModel(
            FakeChooshEngine(),
            credentialStore,
            onConnectSucceeded = { connectSucceededCount++ },
        )
        advanceUntilIdle()

        assertEquals(ConnectionUiState.Connected, viewModel.state.value)
        assertEquals(
            "the very first connect must also invoke the callback — a harmless no-op " +
                "resubscribe with nothing yet subscribed, per onConnectSucceeded's own doc comment",
            1,
            connectSucceededCount,
        )
    }

    @Test
    fun `a reconnect via retry invokes onConnectSucceeded again, so a dropped agent-event stream resumes`() =
        runTest(mainDispatcherRule.dispatcher) {
            val credentialStore = FakeSessionCredentialStore(initial = "still-valid-cred")
            val engine = FakeChooshEngine().apply { simulateConnectTransportFailure = "timed out reaching relayd" }
            var connectSucceededCount = 0
            val viewModel = ConnectionViewModel(
                engine,
                credentialStore,
                onConnectSucceeded = { connectSucceededCount++ },
            )
            advanceUntilIdle()
            assertTrue(viewModel.state.value is ConnectionUiState.Error)
            assertEquals("a failed connect must not invoke the reconnect callback", 0, connectSucceededCount)

            // The simulated transport failure is one-shot (see FakeChooshEngine.connect);
            // this reconnect succeeds — the exact "reconnect after a dropped connection"
            // case resubscribeAll exists for.
            viewModel.retry()
            advanceUntilIdle()

            assertEquals(ConnectionUiState.Connected, viewModel.state.value)
            assertEquals(
                "a successful reconnect must invoke the callback so agent-event subscriptions resume",
                1,
                connectSucceededCount,
            )
        }

    @Test
    fun `an engine throwing on connect is treated as a transport failure, not a crash`() = runTest(mainDispatcherRule.dispatcher) {
        val credentialStore = FakeSessionCredentialStore(initial = "some-cred")
        val throwingEngine = object : ai.choosh.engine.ChooshEngine by FakeChooshEngine() {
            override suspend fun connect(sessionCredential: String): ai.choosh.engine.ConnectResult =
                error("simulated unexpected exception")
        }
        val viewModel = ConnectionViewModel(throwingEngine, credentialStore)
        advanceUntilIdle()

        assertTrue(viewModel.state.value is ConnectionUiState.Error)
        assertNotNull("a thrown exception must not be mistaken for a rejection", credentialStore.load())
    }

    // --- beginRegistration / finishRegistration ---
    //
    // The QR-pairing pass ([ConnectionScreen]'s "Scan QR to pair" button) threads a
    // scanned bootstrapSecret through beginRegistration, but until now nothing at this
    // ViewModel layer exercised the registration round trip itself (Registering ->
    // Connected on a successful ceremony, Registering -> Error on a rejected one) — only
    // the already-stored-credential connect/retry paths above were covered.

    @Test
    fun `beginRegistration moves to Registering and returns the engine's creation options`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ConnectionViewModel(FakeChooshEngine(), FakeSessionCredentialStore())
        advanceUntilIdle()
        assertEquals(ConnectionUiState.NeedsRegistration, viewModel.state.value)

        val creationOptionsJson = viewModel.beginRegistration("scanned-bootstrap-secret")

        assertEquals(ConnectionUiState.Registering, viewModel.state.value)
        assertTrue(
            "must return whatever the engine's webauthnRegisterStart produced, unmodified",
            creationOptionsJson.contains("\"challenge\""),
        )
    }

    @Test
    fun `finishRegistration on success saves the credential and connects`() = runTest(mainDispatcherRule.dispatcher) {
        val credentialStore = FakeSessionCredentialStore()
        val viewModel = ConnectionViewModel(FakeChooshEngine(), credentialStore)
        advanceUntilIdle()
        viewModel.beginRegistration("scanned-bootstrap-secret")
        advanceUntilIdle()

        viewModel.finishRegistration("fake-credential-response-json")
        advanceUntilIdle()

        assertEquals(ConnectionUiState.Connected, viewModel.state.value)
        assertEquals(
            "a successful ceremony must persist the session credential for next launch",
            "fake-session-credential",
            credentialStore.load(),
        )
    }

    @Test
    fun `finishRegistration on failure surfaces as Error and does not store a credential`() = runTest(mainDispatcherRule.dispatcher) {
        val credentialStore = FakeSessionCredentialStore()
        val failingEngine = object : ai.choosh.engine.ChooshEngine by FakeChooshEngine() {
            override suspend fun webauthnRegisterFinish(credentialJson: String, correlationId: String): ai.choosh.engine.WebauthnResult =
                ai.choosh.engine.WebauthnResult.Failure(code = "rejected", message = "bootstrap secret was invalid")
        }
        val viewModel = ConnectionViewModel(failingEngine, credentialStore)
        advanceUntilIdle()
        viewModel.beginRegistration("scanned-bootstrap-secret")
        advanceUntilIdle()

        viewModel.finishRegistration("fake-credential-response-json")
        advanceUntilIdle()

        val state = viewModel.state.value
        assertTrue("a rejected ceremony must surface as an Error state, not Connected", state is ConnectionUiState.Error)
        assertTrue((state as ConnectionUiState.Error).message.contains("bootstrap secret was invalid"))
        assertNull("a failed ceremony must never persist a credential", credentialStore.load())
    }

    @Test
    fun `finishRegistration sends the correlation_id beginRegistration's response carried`() = runTest(mainDispatcherRule.dispatcher) {
        // Regression test: relayd's real register_finish rejects any request
        // that doesn't carry the exact correlation_id its matching
        // register_start response minted (rust/choosh-relayd/src/webauthn.rs's
        // CeremonyQuery) — a prior version of this class never captured or
        // sent it at all, so every real ceremony would have failed this way.
        var receivedCorrelationId: String? = null
        val capturingEngine = object : ai.choosh.engine.ChooshEngine by FakeChooshEngine() {
            override suspend fun webauthnRegisterStart(bootstrapSecret: String): String =
                """{"challenge":"c","correlation_id":"corr-xyz-789"}"""
            override suspend fun webauthnRegisterFinish(credentialJson: String, correlationId: String): ai.choosh.engine.WebauthnResult {
                receivedCorrelationId = correlationId
                return ai.choosh.engine.WebauthnResult.Success(sessionCredential = "fake-session-credential")
            }
        }
        val viewModel = ConnectionViewModel(capturingEngine, FakeSessionCredentialStore())
        advanceUntilIdle()
        viewModel.beginRegistration("scanned-bootstrap-secret")
        advanceUntilIdle()

        viewModel.finishRegistration("fake-credential-response-json")
        advanceUntilIdle()

        assertEquals("corr-xyz-789", receivedCorrelationId)
    }

    @Test
    fun `finishRegistration without a prior beginRegistration fails cleanly instead of sending a missing correlation_id`() =
        runTest(mainDispatcherRule.dispatcher) {
            val viewModel = ConnectionViewModel(FakeChooshEngine(), FakeSessionCredentialStore())
            advanceUntilIdle()

            // No beginRegistration call first — pendingCorrelationId is still null.
            viewModel.finishRegistration("fake-credential-response-json")
            advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue("must fail cleanly, not crash or silently proceed", state is ConnectionUiState.Error)
        }

    @Test
    fun `onRegistrationCancelledOrFailed surfaces as Error, e g for an invalid pairing QR scan`() = runTest(mainDispatcherRule.dispatcher) {
        val viewModel = ConnectionViewModel(FakeChooshEngine(), FakeSessionCredentialStore())
        advanceUntilIdle()

        viewModel.onRegistrationCancelledOrFailed("That QR code doesn't look like a Choosh pairing code.")

        val state = viewModel.state.value
        assertTrue(state is ConnectionUiState.Error)
        assertEquals("That QR code doesn't look like a Choosh pairing code.", (state as ConnectionUiState.Error).message)
    }
}
