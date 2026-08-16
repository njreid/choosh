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
}
