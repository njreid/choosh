package ai.choosh;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertNull;

import java.nio.charset.StandardCharsets;
import org.junit.Test;

/** Headless vertical Java composition from planned native connection to validated Git status. */
public final class AndroidGitStatusCompositionTest {
    @Test public void planned_runtime_connection_reaches_validated_git_status() {
        RecordingBridge bridge = new RecordingBridge(29);
        RecordingRuntime runtime = new RecordingRuntime();
        RecordingSession session = new RecordingSession();
        AndroidGitStatusComposition composition = AndroidGitStatusComposition.fromNativeRuntime(
            ignored -> request(), () -> 7, bridge, runtime,
            (plan, callback) -> callback.onComplete(NativeAuthenticatedSshConnector.NativeOpenResult.connected(session)),
            () -> GitStatusRpc.request(
                new GitStatusRpc.WorkspaceId("00000000-0000-4000-8000-000000000001"),
                new GitStatusRpc.RequestId("00000000-0000-4000-8000-000000000002")
            )
        );
        RecordingListener listener = new RecordingListener();

        composition.refresh(new AuthenticatedSshOperationCoordinator.ProfileId("fixture_profile"), listener);

        assertEquals(7, bridge.generation);
        assertEquals(1, bridge.cancels);
        assertEquals(1, runtime.releases);
        assertNotNull(listener.controller);
        assertNull(listener.failure);
        assertEquals(GitStatusController.Phase.READY, listener.state.phase());
        assertEquals(0, listener.state.snapshot().entries().size());
        assertEquals("{\"id\":\"00000000-0000-4000-8000-000000000002\",\"kind\":\"request\",\"method\":\"git.status\",\"params\":{\"workspace_id\":\"00000000-0000-4000-8000-000000000001\"}}",
            new String(session.request, StandardCharsets.UTF_8));
    }

    @Test public void host_rejection_never_constructs_or_refreshes_git_controller() {
        RecordingBridge bridge = new RecordingBridge(31);
        RecordingRuntime runtime = new RecordingRuntime();
        AndroidGitStatusComposition composition = AndroidGitStatusComposition.fromNativeRuntime(
            ignored -> request(), () -> 9, bridge, runtime,
            (plan, callback) -> callback.onComplete(NativeAuthenticatedSshConnector.NativeOpenResult.failure(
                NativeAuthenticatedSshConnector.Code.HOST_KEY_REJECTED
            )),
            () -> { throw new AssertionError("host rejection must not build a git request"); }
        );
        RecordingListener listener = new RecordingListener();

        composition.refresh(new AuthenticatedSshOperationCoordinator.ProfileId("fixture_profile"), listener);

        assertEquals(AuthenticatedSshOperationCoordinator.OpenCode.HOST_KEY_REJECTED, listener.failure);
        assertNull(listener.controller);
        assertNull(listener.state);
        assertEquals(1, bridge.cancels);
        assertEquals(1, runtime.releases);
    }

    private static AuthenticatedSshOperationCoordinator.ConnectionRequest request() {
        return new AuthenticatedSshOperationCoordinator.ConnectionRequest(
            new AuthenticatedSshOperationCoordinator.ProfileId("fixture_profile"),
            new ProfileConnectionMetadataSource.SshEndpoint("ssh-fixture.example", 22),
            new ProfileConnectionMetadataSource.SshUsername("fixture_user"),
            new ProfileConnectionMetadataSource.KnownHost(ProfileConnectionMetadataSource.HostKeyAlgorithm.ED25519,
                "SHA256:0123456789012345678901234567890123456789012"),
            new SshKeyImportCoordinator.OpaqueCredentialRef("android_keystore_key_42"),
            new SshKeyImportCoordinator.PublicKeyMetadata(SshKeyImportCoordinator.SshPublicKeyAlgorithm.ED25519,
                "SHA256:0123456789012345678901234567890123456789012")
        );
    }

    private static final class RecordingBridge implements RustNativeConnectorJni.NativePlanBridge {
        final long plan;
        int generation;
        int cancels;
        RecordingBridge(long plan) { this.plan = plan; }
        @Override public int abiVersion() { return 3; }
        @Override public long beginAuthenticatedPlan(int generation, RustNativeConnectorJni.NativeHandles handles) {
            this.generation = generation;
            return plan;
        }
        @Override public int openAuthenticatedPlan(int generation, long plan) { return 5; }
        @Override public int cancelAuthenticatedPlan(int generation, long plan) { cancels++; return 0; }
    }

    private static final class RecordingRuntime implements RustNativeConnectorJni.NativeRuntime {
        int releases;
        @Override public RustNativeConnectorJni.NativeLease acquire(
            NativeAuthenticatedSshConnector.NativeConnectionInput input
        ) {
            return new RustNativeConnectorJni.NativeLease(
                new RustNativeConnectorJni.NativeHandles(1, 2, 3, 4, 5, 6, 7), () -> releases++
            );
        }
    }

    private static final class RecordingSession implements NativeAuthenticatedSshConnector.NativeSession {
        byte[] request;
        @Override public void executeRpc(byte[] request, NativeAuthenticatedSshConnector.NativeRpcCallback callback) {
            this.request = request.clone();
            callback.onComplete(new NativeAuthenticatedSshConnector.NativeRpcResult(
                "{\"id\":\"00000000-0000-4000-8000-000000000002\",\"kind\":\"response\",\"result\":{\"workspace_id\":\"00000000-0000-4000-8000-000000000001\",\"entries\":[]}}"
                    .getBytes(StandardCharsets.UTF_8)
            ));
        }
    }

    private static final class RecordingListener implements AndroidGitStatusComposition.Listener {
        AuthenticatedSshOperationCoordinator.OpenCode failure;
        GitStatusController controller;
        GitStatusController.State state;
        @Override public void onConnectionFailure(AuthenticatedSshOperationCoordinator.OpenCode failure) {
            this.failure = failure;
        }
        @Override public void onGitStatusController(GitStatusController controller) { this.controller = controller; }
        @Override public void onGitStatusState(GitStatusController.State state) { this.state = state; }
    }
}
