package ai.choosh;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public final class WorkspaceStatusControllerTest {
    @Test public void refreshesThroughAuthenticatedRpcAndPublishesValidatedStatus() {
        RecordingOperations operations = new RecordingOperations();
        RecordingState listener = new RecordingState();
        WorkspaceStatusController controller = new WorkspaceStatusController(
            operations, () -> new AuthenticatedSshOperationCoordinator.RpcRequest(new byte[] {1}),
            response -> new WorkspaceStatusController.WorkspaceStatus(
                WorkspaceStatusController.Availability.READY, 3
            ), listener
        );

        controller.refresh();

        assertEquals(WorkspaceStatusController.Phase.LOADING, listener.value.phase());
        operations.callback.onResult(new AuthenticatedSshOperationCoordinator.RpcResult(new byte[] {2}));
        assertEquals(WorkspaceStatusController.Phase.READY, controller.state().phase());
        assertEquals(3, controller.state().status().itemCount());
    }

    @Test public void rejectsMalformedProtocolResultWithoutRetainingHostData() {
        RecordingOperations operations = new RecordingOperations();
        RecordingState listener = new RecordingState();
        WorkspaceStatusController controller = new WorkspaceStatusController(
            operations, () -> new AuthenticatedSshOperationCoordinator.RpcRequest(new byte[] {1}),
            response -> { throw new WorkspaceStatusController.ProtocolException(); }, listener
        );

        controller.refresh();
        operations.callback.onResult(new AuthenticatedSshOperationCoordinator.RpcResult(new byte[] {2}));

        assertEquals(WorkspaceStatusController.Phase.PROTOCOL_REJECTED, listener.value.phase());
        assertNull(listener.value.status());
    }

    @Test public void mapsTransportFailureAndIgnoresDuplicateCompletion() {
        RecordingState listener = new RecordingState();
        WorkspaceStatusController controller = new WorkspaceStatusController(
            (request, callback) -> { throw new AuthenticatedSshOperationCoordinator.SshTransportException(); },
            () -> new AuthenticatedSshOperationCoordinator.RpcRequest(new byte[] {1}),
            response -> new WorkspaceStatusController.WorkspaceStatus(
                WorkspaceStatusController.Availability.EMPTY, 0
            ), listener
        );

        controller.refresh();

        assertEquals(WorkspaceStatusController.Phase.TRANSPORT_UNAVAILABLE, listener.value.phase());
        assertTrue(listener.value.canRefresh());
    }

    @Test public void boundsPresentationItemCount() {
        try {
            new WorkspaceStatusController.WorkspaceStatus(WorkspaceStatusController.Availability.READY, 10_001);
            throw new AssertionError("out-of-range item count was accepted");
        } catch (IllegalArgumentException expected) {
            // expected
        }
    }

    private static final class RecordingOperations
        implements AuthenticatedSshOperationCoordinator.AuthenticatedOperations {
        AuthenticatedSshOperationCoordinator.RpcCallback callback;

        @Override public void executeRpc(
            AuthenticatedSshOperationCoordinator.RpcRequest request,
            AuthenticatedSshOperationCoordinator.RpcCallback value
        ) {
            callback = value;
        }
    }

    private static final class RecordingState implements WorkspaceStatusController.StateListener {
        WorkspaceStatusController.State value;
        @Override public void onStateChanged(WorkspaceStatusController.State state) { value = state; }
    }
}
