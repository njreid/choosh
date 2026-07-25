package ai.choosh;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import org.junit.Test;

/** Headless proof that application composition does not eagerly acquire runtime capabilities. */
public final class AndroidConnectionCompositionTest {
    @Test public void composition_is_inert_until_a_selected_profile_is_opened() {
        RecordingSocketOpener sockets = new RecordingSocketOpener();
        int[] profileLoads = {0};
        int[] publicKeyLoads = {0};
        int[] signerBinds = {0};
        AndroidRuntimeComposition runtimes = new AndroidRuntimeComposition(
            input -> new RustNativeConnectorJni.NativeHandles(1, 2, 3, 4, 5, 6, 7),
            sockets,
            new BoundedAndroidSocketAdapter.Limits(1, 1, 64),
            input -> {
                publicKeyLoads[0]++;
                return "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti"
                    .getBytes(java.nio.charset.StandardCharsets.UTF_8);
            },
            input -> {
                signerBinds[0]++;
                return payload -> new byte[] {1};
            }
        );
        AndroidConnectionComposition factory = new AndroidConnectionComposition(
            ignored -> {
                profileLoads[0]++;
                return profile();
            },
            runtimes,
            () -> 1,
            new RecordingBridge(),
            (plan, callback) -> callback.onComplete(NativeAuthenticatedSshConnector.NativeOpenResult.failure(
                NativeAuthenticatedSshConnector.Code.TRANSPORT_UNAVAILABLE)),
            () -> GitStatusRpc.request(
                new GitStatusRpc.WorkspaceId("00000000-0000-4000-8000-000000000001"),
                new GitStatusRpc.RequestId("00000000-0000-4000-8000-000000000002")
            )
        );

        AndroidGitStatusComposition composition = factory.newGitStatusComposition();
        assertEquals(0, profileLoads[0]);
        assertEquals(0, sockets.opens);
        assertEquals(0, publicKeyLoads[0]);
        assertEquals(0, signerBinds[0]);

        RecordingListener listener = new RecordingListener();
        composition.refresh(new AuthenticatedSshOperationCoordinator.ProfileId("fixture_profile"), listener);

        assertEquals(1, profileLoads[0]);
        assertEquals(1, sockets.opens);
        assertEquals(1, publicKeyLoads[0]);
        assertEquals(1, signerBinds[0]);
        assertEquals(1, sockets.socket.closes);
        assertEquals(AuthenticatedSshOperationCoordinator.OpenCode.TRANSPORT_UNAVAILABLE, listener.failure);
        assertNull(listener.controller);
    }

    private static ProfileConnectionMetadataSource.ProfileConnectionMetadata profile() {
        return new ProfileConnectionMetadataSource.ProfileConnectionMetadata(
            new AuthenticatedSshOperationCoordinator.ProfileId("fixture_profile"),
            new ProfileConnectionMetadataSource.SshEndpoint("ssh-fixture.example", 22),
            new ProfileConnectionMetadataSource.SshUsername("fixture-user"),
            new ProfileConnectionMetadataSource.KnownHost(
                ProfileConnectionMetadataSource.HostKeyAlgorithm.ED25519,
                "SHA256:0123456789012345678901234567890123456789012"),
            new SshKeyImportCoordinator.OpaqueCredentialRef("android_keystore_key_42"),
            new SshKeyImportCoordinator.PublicKeyMetadata(
                SshKeyImportCoordinator.SshPublicKeyAlgorithm.ED25519,
                "SHA256:UCUiLr7Pjs9wFFJMDByLgc3NrtdU344OgUM45wZPcIQ")
        );
    }

    private static final class RecordingBridge implements RustNativeConnectorJni.NativePlanBridge {
        @Override public int abiVersion() { return 3; }
        @Override public long beginAuthenticatedPlan(int generation, RustNativeConnectorJni.NativeHandles handles) {
            return 9;
        }
        @Override public int openAuthenticatedPlan(int generation, long plan) { return 5; }
        @Override public int cancelAuthenticatedPlan(int generation, long plan) { return 0; }
    }

    private static final class RecordingSocketOpener implements BoundedAndroidSocketAdapter.SocketOpener {
        int opens;
        final RecordingSocket socket = new RecordingSocket();
        @Override public BoundedAndroidSocketAdapter.OpenedSocket open(
            String host, int port, int connectTimeoutMillis, int readTimeoutMillis
        ) {
            opens++;
            return socket;
        }
    }

    private static final class RecordingSocket implements BoundedAndroidSocketAdapter.OpenedSocket {
        int closes;
        @Override public InputStream input() { return new ByteArrayInputStream(new byte[0]); }
        @Override public OutputStream output() { return new ByteArrayOutputStream(); }
        @Override public void close() { closes++; }
    }

    private static final class RecordingListener implements AndroidGitStatusComposition.Listener {
        AuthenticatedSshOperationCoordinator.OpenCode failure;
        GitStatusController controller;
        @Override public void onConnectionFailure(AuthenticatedSshOperationCoordinator.OpenCode value) {
            failure = value;
        }
        @Override public void onGitStatusController(GitStatusController value) { controller = value; }
        @Override public void onGitStatusState(GitStatusController.State value) { }
    }
}
