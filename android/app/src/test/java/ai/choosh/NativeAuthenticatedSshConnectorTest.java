package ai.choosh;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertSame;
import static org.junit.Assert.assertThrows;

import java.util.concurrent.atomic.AtomicInteger;
import org.junit.Test;

/** Deterministic JNI-composition proof using an injected native port, not a device or socket. */
public final class NativeAuthenticatedSshConnectorTest {
    @Test public void forwards_only_typed_nonsecret_input_and_exposes_rpc_after_verified_open() throws Exception {
        RecordingPort port = new RecordingPort();
        RecordingSession session = new RecordingSession();
        port.result = NativeAuthenticatedSshConnector.NativeOpenResult.connected(session);
        Outcome outcome = new Outcome();

        new AuthenticatedSshOperationCoordinator(profileId -> request(), new NativeAuthenticatedSshConnector(port))
            .open(new AuthenticatedSshOperationCoordinator.ProfileId("fixture_profile"), outcome);

        assertEquals(1, port.calls.get());
        assertEquals("ssh-fixture.example", port.input.endpoint().hostForNativeConnector());
        assertEquals("fixture_user", port.input.username().valueForNativeConnector());
        assertEquals("SHA256:0123456789012345678901234567890123456789012",
            port.input.knownHost().sha256FingerprintForVerifier());
        assertEquals("android_keystore_key_42", port.input.credentialRef().valueForCredentialStore());
        assertEquals("NativeConnectionInput(endpoint=REDACTED, username=REDACTED, knownHost=ED25519, credential=REDACTED, publicKey=ED25519)",
            port.input.toString());
        assertEquals(AuthenticatedSshOperationCoordinator.OpenCode.CONNECTED, outcome.result.code());

        byte[] bytes = new byte[] {1, 2, 3};
        outcome.result.operations().executeRpc(new AuthenticatedSshOperationCoordinator.RpcRequest(bytes), response -> {
            assertArrayEquals(new byte[] {4, 5}, response.copyBytesForProtocolDecoder());
        });
        bytes[0] = 9;
        assertArrayEquals(new byte[] {1, 2, 3}, session.request);
    }

    @Test public void maps_native_failures_and_deduplicates_completion() throws Exception {
        RecordingPort port = new RecordingPort();
        port.result = NativeAuthenticatedSshConnector.NativeOpenResult.failure(
            NativeAuthenticatedSshConnector.Code.HOST_KEY_REJECTED
        );
        Outcome outcome = new Outcome();

        new AuthenticatedSshOperationCoordinator(profileId -> request(), new NativeAuthenticatedSshConnector(port))
            .open(new AuthenticatedSshOperationCoordinator.ProfileId("fixture_profile"), outcome);

        assertEquals(1, outcome.calls.get());
        assertEquals(AuthenticatedSshOperationCoordinator.OpenCode.HOST_KEY_REJECTED, outcome.result.code());
        assertNull(outcome.result.operations());
    }

    @Test public void native_throw_is_a_transport_failure_for_the_coordinator() {
        NativeAuthenticatedSshConnector connector = new NativeAuthenticatedSshConnector(
            (input, callback) -> { throw new NativeAuthenticatedSshConnector.NativeBridgeException(); }
        );

        assertThrows(AuthenticatedSshOperationCoordinator.SshTransportException.class,
            () -> connector.openVerified(request(), result -> { }));
    }

    @Test public void null_native_rpc_result_is_reported_to_the_callback_without_throwing() throws Exception {
        NativeAuthenticatedSshConnector.NativeSession session = (request, callback) -> callback.onComplete(null);
        NativeAuthenticatedSshConnector.NativeOpenResult open =
            NativeAuthenticatedSshConnector.NativeOpenResult.connected(session);
        Outcome outcome = new Outcome();

        new AuthenticatedSshOperationCoordinator(profileId -> request(),
            new NativeAuthenticatedSshConnector((input, callback) -> callback.onComplete(open)))
            .open(new AuthenticatedSshOperationCoordinator.ProfileId("fixture_profile"), outcome);

        AtomicInteger callbacks = new AtomicInteger();
        outcome.result.operations().executeRpc(
            new AuthenticatedSshOperationCoordinator.RpcRequest(new byte[] {1}),
            response -> {
                callbacks.incrementAndGet();
                assertNull(response);
            }
        );
        assertEquals(1, callbacks.get());
    }

    private static AuthenticatedSshOperationCoordinator.ConnectionRequest request() {
        return new AuthenticatedSshOperationCoordinator.ConnectionRequest(
            new AuthenticatedSshOperationCoordinator.ProfileId("fixture_profile"),
            new ProfileConnectionMetadataSource.SshEndpoint("ssh-fixture.example", 22),
            new ProfileConnectionMetadataSource.SshUsername("fixture_user"),
            new ProfileConnectionMetadataSource.KnownHost(
                ProfileConnectionMetadataSource.HostKeyAlgorithm.ED25519,
                "SHA256:0123456789012345678901234567890123456789012"
            ),
            new SshKeyImportCoordinator.OpaqueCredentialRef("android_keystore_key_42"),
            new SshKeyImportCoordinator.PublicKeyMetadata(
                SshKeyImportCoordinator.SshPublicKeyAlgorithm.ED25519,
                "SHA256:0123456789012345678901234567890123456789012"
            )
        );
    }

    private static final class RecordingPort implements NativeAuthenticatedSshConnector.NativeConnectorPort {
        final AtomicInteger calls = new AtomicInteger();
        NativeAuthenticatedSshConnector.NativeConnectionInput input;
        NativeAuthenticatedSshConnector.NativeOpenResult result;

        @Override public void open(
            NativeAuthenticatedSshConnector.NativeConnectionInput input,
            NativeAuthenticatedSshConnector.NativeOpenCallback callback
        ) {
            calls.incrementAndGet();
            this.input = input;
            callback.onComplete(result);
            callback.onComplete(result);
        }
    }

    private static final class RecordingSession implements NativeAuthenticatedSshConnector.NativeSession {
        byte[] request;

        @Override public void executeRpc(byte[] request, NativeAuthenticatedSshConnector.NativeRpcCallback callback) {
            this.request = request.clone();
            callback.onComplete(new NativeAuthenticatedSshConnector.NativeRpcResult(new byte[] {4, 5}));
        }
    }

    private static final class Outcome implements AuthenticatedSshOperationCoordinator.OutcomeListener {
        final AtomicInteger calls = new AtomicInteger();
        AuthenticatedSshOperationCoordinator.OpenOutcome result;

        @Override public void onComplete(AuthenticatedSshOperationCoordinator.OpenOutcome result) {
            calls.incrementAndGet();
            this.result = result;
        }
    }
}
