package ai.choosh;

import java.util.Objects;

/**
 * UI-safe workspace summary driven by the authenticated fixed-command RPC capability.
 *
 * <p>The controller owns neither an SSH connection nor a protocol codec.  The Android
 * composition root supplies the narrow post-authentication capability, a versioned request,
 * and a decoder that validates untrusted host bytes before they reach presentation state.  No
 * remote paths, workspace names, Git output, or service metadata are retained here.</p>
 */
public final class WorkspaceStatusController {
    private final AuthenticatedSshOperationCoordinator.AuthenticatedOperations operations;
    private final StatusRequest request;
    private final StatusDecoder decoder;
    private final StateListener listener;
    private State state = State.idle();

    public WorkspaceStatusController(
        AuthenticatedSshOperationCoordinator.AuthenticatedOperations operations,
        StatusRequest request,
        StatusDecoder decoder,
        StateListener listener
    ) {
        this.operations = Objects.requireNonNull(operations, "operations");
        this.request = Objects.requireNonNull(request, "request");
        this.decoder = Objects.requireNonNull(decoder, "decoder");
        this.listener = Objects.requireNonNull(listener, "listener");
    }

    public State state() { return state; }

    /** Requests one bounded, versioned workspace summary through the verified connection. */
    public void refresh() {
        if (state.phase == Phase.LOADING) return;
        state = State.loading();
        listener.onStateChanged(state);
        Completion completion = new Completion();
        try {
            operations.executeRpc(request.request(), completion::completeOnce);
        } catch (AuthenticatedSshOperationCoordinator.SshTransportException unavailable) {
            completion.failTransport();
        }
    }

    public interface StatusRequest {
        /** Returns the versioned, bounded RPC payload for a workspace-status query. */
        AuthenticatedSshOperationCoordinator.RpcRequest request();
    }

    public interface StatusDecoder {
        /** Converts untrusted bounded protocol bytes into UI-safe, validated status only. */
        WorkspaceStatus decode(AuthenticatedSshOperationCoordinator.RpcResult response)
            throws ProtocolException;
    }

    public interface StateListener {
        void onStateChanged(State state);
    }

    public enum Phase { IDLE, LOADING, READY, PROTOCOL_REJECTED, TRANSPORT_UNAVAILABLE }

    /** Bounded UI-safe facts; it intentionally carries no host-controlled display strings. */
    public static final class WorkspaceStatus {
        private static final int MAX_ITEMS = 10_000;
        private final Availability availability;
        private final int itemCount;

        public WorkspaceStatus(Availability availability, int itemCount) {
            this.availability = Objects.requireNonNull(availability, "availability");
            if (itemCount < 0 || itemCount > MAX_ITEMS) {
                throw new IllegalArgumentException("invalid workspace item count");
            }
            this.itemCount = itemCount;
        }

        public Availability availability() { return availability; }
        public int itemCount() { return itemCount; }
    }

    public enum Availability { READY, EMPTY, UNAVAILABLE }

    public static final class State {
        private final Phase phase;
        private final WorkspaceStatus status;

        private State(Phase phase, WorkspaceStatus status) {
            this.phase = phase;
            this.status = status;
        }

        static State idle() { return new State(Phase.IDLE, null); }
        static State loading() { return new State(Phase.LOADING, null); }
        static State ready(WorkspaceStatus value) { return new State(Phase.READY, value); }
        static State failure(Phase phase) { return new State(phase, null); }

        public Phase phase() { return phase; }
        public WorkspaceStatus status() { return status; }
        public boolean canRefresh() { return phase != Phase.LOADING; }
    }

    public static final class ProtocolException extends Exception {
        public ProtocolException() { super(); }
    }

    private final class Completion {
        private boolean completed;

        void completeOnce(AuthenticatedSshOperationCoordinator.RpcResult response) {
            if (completed) return;
            completed = true;
            if (response == null) {
                state = State.failure(Phase.PROTOCOL_REJECTED);
            } else {
                try {
                    WorkspaceStatus decoded = decoder.decode(response);
                    state = decoded == null
                        ? State.failure(Phase.PROTOCOL_REJECTED)
                        : State.ready(decoded);
                } catch (ProtocolException | IllegalArgumentException rejected) {
                    state = State.failure(Phase.PROTOCOL_REJECTED);
                }
            }
            listener.onStateChanged(state);
        }

        void failTransport() {
            if (completed) return;
            completed = true;
            state = State.failure(Phase.TRANSPORT_UNAVAILABLE);
            listener.onStateChanged(state);
        }
    }
}
