package ai.choosh;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertSame;
import static org.junit.Assert.assertThrows;

import java.util.concurrent.atomic.AtomicInteger;
import org.junit.Test;

public final class AuthenticatedSshOperationCoordinatorTest {
    private static final AuthenticatedSshOperationCoordinator.ProfileId PROFILE =
        new AuthenticatedSshOperationCoordinator.ProfileId("workstation_01");

    @Test public void opensOnlyThroughInjectedVerifiedConnectorAndReturnsNarrowCapability() {
        AuthenticatedSshOperationCoordinator.ConnectionRequest request = request();
        RecordingConnector connector = new RecordingConnector();
        FakeOperations operations = new FakeOperations();
        connector.result = AuthenticatedSshOperationCoordinator.ConnectorResult.connected(operations);
        Outcome outcome = new Outcome();

        new AuthenticatedSshOperationCoordinator(profileId -> request, connector).open(PROFILE, outcome);

        assertSame(request, connector.request);
        assertEquals(AuthenticatedSshOperationCoordinator.OpenCode.CONNECTED, outcome.value.code());
        assertSame(operations, outcome.value.operations());
        assertEquals("ConnectionRequest(profile=ProfileId(REDACTED), endpoint=REDACTED, username=REDACTED, knownHost=ED25519, credential=REDACTED, publicKey=ED25519)", request.toString());
    }

    @Test public void unavailableProfileDoesNotOpenTransport() {
        RecordingConnector connector = new RecordingConnector();
        Outcome outcome = new Outcome();

        new AuthenticatedSshOperationCoordinator(profileId -> null, connector).open(PROFILE, outcome);

        assertEquals(0, connector.calls.get());
        assertEquals(AuthenticatedSshOperationCoordinator.OpenCode.PROFILE_UNAVAILABLE, outcome.value.code());
        assertNull(outcome.value.operations());
    }

    @Test public void hostKeyRejectionDoesNotExposeOperations() {
        RecordingConnector connector = new RecordingConnector();
        connector.result = AuthenticatedSshOperationCoordinator.ConnectorResult.failure(
            AuthenticatedSshOperationCoordinator.ConnectorCode.HOST_KEY_REJECTED
        );
        Outcome outcome = new Outcome();

        new AuthenticatedSshOperationCoordinator(profileId -> request(), connector).open(PROFILE, outcome);

        assertEquals(AuthenticatedSshOperationCoordinator.OpenCode.HOST_KEY_REJECTED, outcome.value.code());
        assertNull(outcome.value.operations());
    }

    @Test public void duplicateAdapterCallbacksHaveOneUiOutcome() {
        RecordingConnector connector = new RecordingConnector();
        connector.result = AuthenticatedSshOperationCoordinator.ConnectorResult.failure(
            AuthenticatedSshOperationCoordinator.ConnectorCode.AUTHENTICATION_FAILED
        );
        Outcome outcome = new Outcome();

        new AuthenticatedSshOperationCoordinator(profileId -> request(), connector).open(PROFILE, outcome);

        assertEquals(1, outcome.calls.get());
        assertEquals(AuthenticatedSshOperationCoordinator.OpenCode.AUTHENTICATION_FAILED, outcome.value.code());
    }

    @Test public void rpcBytesAreDefensivelyCopiedAndBounded() {
        byte[] request = new byte[] {1, 2, 3};
        AuthenticatedSshOperationCoordinator.RpcRequest rpc = new AuthenticatedSshOperationCoordinator.RpcRequest(request);
        request[0] = 9;
        byte[] nativeBytes = rpc.copyBytesForNativeAdapter();
        nativeBytes[1] = 9;

        assertArrayEquals(new byte[] {1, 2, 3}, rpc.copyBytesForNativeAdapter());
        assertThrows(IllegalArgumentException.class, () -> new AuthenticatedSshOperationCoordinator.RpcRequest(new byte[0]));
        assertThrows(IllegalArgumentException.class, () -> new AuthenticatedSshOperationCoordinator.RpcResult(new byte[1_048_577]));
    }

    @Test public void profileIdRejectsPathsAndControlCharacters() {
        assertThrows(IllegalArgumentException.class, () -> new AuthenticatedSshOperationCoordinator.ProfileId("../home"));
        assertThrows(IllegalArgumentException.class, () -> new AuthenticatedSshOperationCoordinator.ProfileId("profile\n1"));
    }

    private static AuthenticatedSshOperationCoordinator.ConnectionRequest request() {
        return new AuthenticatedSshOperationCoordinator.ConnectionRequest(
            PROFILE,
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

    private static final class RecordingConnector implements AuthenticatedSshOperationCoordinator.VerifiedSshConnector {
        final AtomicInteger calls = new AtomicInteger();
        AuthenticatedSshOperationCoordinator.ConnectionRequest request;
        AuthenticatedSshOperationCoordinator.ConnectorResult result;

        @Override public void openVerified(
            AuthenticatedSshOperationCoordinator.ConnectionRequest request,
            AuthenticatedSshOperationCoordinator.ConnectorCallback callback
        ) {
            calls.incrementAndGet();
            this.request = request;
            callback.onResult(result);
            callback.onResult(result);
        }
    }

    private static final class FakeOperations implements AuthenticatedSshOperationCoordinator.AuthenticatedOperations {
        @Override public void executeRpc(
            AuthenticatedSshOperationCoordinator.RpcRequest request,
            AuthenticatedSshOperationCoordinator.RpcCallback callback
        ) { }
    }

    private static final class Outcome implements AuthenticatedSshOperationCoordinator.OutcomeListener {
        final AtomicInteger calls = new AtomicInteger();
        AuthenticatedSshOperationCoordinator.OpenOutcome value;

        @Override public void onComplete(AuthenticatedSshOperationCoordinator.OpenOutcome outcome) {
            calls.incrementAndGet();
            value = outcome;
        }
    }
}
