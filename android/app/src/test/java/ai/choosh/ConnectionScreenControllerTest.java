package ai.choosh;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

/** Headless proof of presentation state; no Activity, sockets, or wall-clock timing. */
public final class ConnectionScreenControllerTest {
    @Test public void selectionAndSuccessfulConnectionProduceSafeStates() {
        RecordingView view = new RecordingView();
        ConnectionScreenController screen = new ConnectionScreenController(
            new AuthenticatedSshOperationCoordinator(
                id -> { throw new AuthenticatedSshOperationCoordinator.ProfileUnavailableException(); },
                (request, callback) -> { }
            ),
            view
        );

        screen.selectProfile("workstation_01");
        assertEquals(ConnectionScreenController.Phase.READY, view.state.phase());
        assertTrue(view.state.canConnect());
    }

    @Test public void invalidProfileNeveropensAConnection() {
        RecordingView view = new RecordingView();
        int[] opens = {0};
        ConnectionScreenController screen = new ConnectionScreenController(
            new AuthenticatedSshOperationCoordinator(
                id -> { throw new AuthenticatedSshOperationCoordinator.ProfileUnavailableException(); },
                (request, callback) -> opens[0]++
            ),
            view
        );

        screen.selectProfile("../not-a-profile");
        screen.connect();

        assertEquals(ConnectionScreenController.Phase.INVALID_PROFILE, view.state.phase());
        assertEquals(0, opens[0]);
        assertFalse(view.state.canConnect());
    }

    @Test public void hostKeyFailureIsDistinctAndRetryable() {
        RecordingView view = new RecordingView();
        ConnectionScreenController screen = new ConnectionScreenController(
            new AuthenticatedSshOperationCoordinator(
                id -> request(id),
                (request, callback) -> callback.onResult(
                    AuthenticatedSshOperationCoordinator.ConnectorResult.failure(
                        AuthenticatedSshOperationCoordinator.ConnectorCode.HOST_KEY_REJECTED
                    )
                )
            ),
            view
        );

        screen.selectProfile("workstation_01");
        screen.connect();

        assertEquals(ConnectionScreenController.Phase.HOST_KEY_REJECTED, view.state.phase());
        assertTrue(view.state.canConnect());
    }

    private static AuthenticatedSshOperationCoordinator.ConnectionRequest request(
        AuthenticatedSshOperationCoordinator.ProfileId id
    ) {
        return new AuthenticatedSshOperationCoordinator.ConnectionRequest(
            id, new ProfileConnectionMetadataSource.SshEndpoint("fixture.example", 22),
            new ProfileConnectionMetadataSource.SshUsername("fixture"),
            new ProfileConnectionMetadataSource.KnownHost(
                ProfileConnectionMetadataSource.HostKeyAlgorithm.ED25519,
                "SHA256:0123456789012345678901234567890123456789012"
            ), new SshKeyImportCoordinator.OpaqueCredentialRef("fixture_credential"),
            new SshKeyImportCoordinator.PublicKeyMetadata(
                SshKeyImportCoordinator.SshPublicKeyAlgorithm.ED25519,
                "SHA256:0123456789012345678901234567890123456789012"
            )
        );
    }

    private static final class RecordingView implements ConnectionScreenController.StateListener {
        ConnectionScreenController.State state;
        @Override public void onStateChanged(ConnectionScreenController.State state) { this.state = state; }
    }
}
