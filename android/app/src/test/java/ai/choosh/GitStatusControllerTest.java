package ai.choosh;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;

import java.nio.charset.StandardCharsets;
import org.junit.Test;

public final class GitStatusControllerTest {
    @Test public void publishes_a_validated_snapshot_and_maps_typed_failure() {
        RecordingOperations operations = new RecordingOperations();
        RecordingState state = new RecordingState();
        GitStatusController controller = new GitStatusController(operations, () -> request(), state);
        controller.refresh();
        assertEquals(GitStatusController.Phase.LOADING, state.value.phase());
        operations.callback.onResult(result("{\"id\":\"00000000-0000-4000-8000-000000000002\",\"kind\":\"response\",\"result\":{\"workspace_id\":\"00000000-0000-4000-8000-000000000001\",\"entries\":[]}}"));
        assertEquals(GitStatusController.Phase.READY, state.value.phase());
        assertEquals(0, state.value.snapshot().entries().size());
        controller.refresh();
        operations.callback.onResult(result("{\"id\":\"00000000-0000-4000-8000-000000000002\",\"kind\":\"response\",\"error\":{\"code\":\"not_found\",\"message\":\"x\"}}"));
        assertEquals(GitStatusController.Phase.NOT_FOUND, state.value.phase());
        assertNull(state.value.snapshot());
    }
    private static GitStatusRpc.Request request() { return GitStatusRpc.request(new GitStatusRpc.WorkspaceId("00000000-0000-4000-8000-000000000001"), new GitStatusRpc.RequestId("00000000-0000-4000-8000-000000000002")); }
    private static AuthenticatedSshOperationCoordinator.RpcResult result(String value) { return new AuthenticatedSshOperationCoordinator.RpcResult(value.getBytes(StandardCharsets.UTF_8)); }
    private static final class RecordingOperations implements AuthenticatedSshOperationCoordinator.AuthenticatedOperations { AuthenticatedSshOperationCoordinator.RpcCallback callback; @Override public void executeRpc(AuthenticatedSshOperationCoordinator.RpcRequest request, AuthenticatedSshOperationCoordinator.RpcCallback callback) { this.callback = callback; } }
    private static final class RecordingState implements GitStatusController.StateListener { GitStatusController.State value; @Override public void onStateChanged(GitStatusController.State value) { this.value = value; } }
}
