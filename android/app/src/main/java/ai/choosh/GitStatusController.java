package ai.choosh;

import java.util.Objects;

/** Headless presentation seam for one authenticated, registered-workspace status refresh. */
public final class GitStatusController {
    private final AuthenticatedSshOperationCoordinator.AuthenticatedOperations operations;
    private final GitStatusRpc.RequestSource requests;
    private final StateListener listener;
    private State state = State.idle();

    public GitStatusController(
        AuthenticatedSshOperationCoordinator.AuthenticatedOperations operations,
        GitStatusRpc.RequestSource requests,
        StateListener listener
    ) {
        this.operations = Objects.requireNonNull(operations, "operations");
        this.requests = Objects.requireNonNull(requests, "requests");
        this.listener = Objects.requireNonNull(listener, "listener");
    }

    public State state() { return state; }

    /** Starts at most one request; outcomes are mapped without retaining untrusted response bytes. */
    public void refresh() {
        if (state.phase == Phase.LOADING) return;
        state = State.loading();
        listener.onStateChanged(state);
        final GitStatusRpc.Request request;
        try {
            request = Objects.requireNonNull(requests.next(), "request");
        } catch (IllegalArgumentException | NullPointerException rejected) {
            state = State.failure(Phase.PROTOCOL_REJECTED);
            listener.onStateChanged(state);
            return;
        }
        Completion completion = new Completion(request);
        try {
            operations.executeRpc(request.rpcRequest(), completion::completeOnce);
        } catch (AuthenticatedSshOperationCoordinator.SshTransportException exception) {
            completion.transportUnavailable();
        }
    }

    public interface StateListener { void onStateChanged(State state); }
    public enum Phase { IDLE, LOADING, READY, NOT_FOUND, LIMIT_EXCEEDED, PROTOCOL_REJECTED, TRANSPORT_UNAVAILABLE }

    public static final class State {
        private final Phase phase;
        private final GitStatusRpc.Snapshot snapshot;
        private State(Phase phase, GitStatusRpc.Snapshot snapshot) { this.phase = phase; this.snapshot = snapshot; }
        static State idle() { return new State(Phase.IDLE, null); }
        static State loading() { return new State(Phase.LOADING, null); }
        static State ready(GitStatusRpc.Snapshot snapshot) { return new State(Phase.READY, snapshot); }
        static State failure(Phase phase) { return new State(phase, null); }
        public Phase phase() { return phase; }
        public GitStatusRpc.Snapshot snapshot() { return snapshot; }
        public boolean canRefresh() { return phase != Phase.LOADING; }
    }

    private final class Completion {
        private final GitStatusRpc.Request request;
        private boolean complete;
        Completion(GitStatusRpc.Request request) { this.request = request; }
        void completeOnce(AuthenticatedSshOperationCoordinator.RpcResult response) {
            if (complete) return;
            complete = true;
            try {
                if (response == null) throw new GitStatusRpc.ProtocolException();
                GitStatusRpc.Result decoded = GitStatusRpc.decode(request, response);
                if (decoded.isSuccess()) state = State.ready(decoded.snapshot());
                else state = State.failure(phaseFor(decoded.error()));
            } catch (GitStatusRpc.ProtocolException | IllegalArgumentException rejected) {
                state = State.failure(Phase.PROTOCOL_REJECTED);
            }
            listener.onStateChanged(state);
        }
        void transportUnavailable() {
            if (complete) return;
            complete = true;
            state = State.failure(Phase.TRANSPORT_UNAVAILABLE);
            listener.onStateChanged(state);
        }
    }

    private static Phase phaseFor(GitStatusRpc.ErrorCode error) {
        if (error == GitStatusRpc.ErrorCode.NOT_FOUND) return Phase.NOT_FOUND;
        if (error == GitStatusRpc.ErrorCode.LIMIT_EXCEEDED) return Phase.LIMIT_EXCEEDED;
        return Phase.PROTOCOL_REJECTED;
    }
}
