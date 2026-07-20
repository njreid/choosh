package ai.choosh;

import java.util.Objects;

/**
 * Framework-free presentation state for the initial profile connection screen.
 *
 * <p>It exposes no endpoint, username, fingerprint, or credential data to the view. The outer
 * Android composition root supplies the authenticated-operation coordinator and chooses how to
 * render state changes on the main thread. This makes the profile selection and connect flow
 * deterministic in JVM tests.</p>
 */
public final class ConnectionScreenController {
    private final AuthenticatedSshOperationCoordinator coordinator;
    private final StateListener listener;
    private State state = State.awaitingProfile();

    public ConnectionScreenController(
        AuthenticatedSshOperationCoordinator coordinator,
        StateListener listener
    ) {
        this.coordinator = Objects.requireNonNull(coordinator, "coordinator");
        this.listener = Objects.requireNonNull(listener, "listener");
    }

    public State state() { return state; }

    /** Selects an opaque durable profile identifier; invalid input leaves no profile selected. */
    public void selectProfile(String value) {
        try {
            state = State.ready(new AuthenticatedSshOperationCoordinator.ProfileId(value));
        } catch (IllegalArgumentException invalid) {
            state = State.invalidProfile();
        }
        listener.onStateChanged(state);
    }

    /** Starts exactly one authenticated connection attempt for the selected profile. */
    public void connect() {
        if (state.profileId == null || state.phase == Phase.CONNECTING) return;
        AuthenticatedSshOperationCoordinator.ProfileId selected = state.profileId;
        state = State.connecting(selected);
        listener.onStateChanged(state);
        coordinator.open(selected, outcome -> complete(selected, outcome));
    }

    private void complete(
        AuthenticatedSshOperationCoordinator.ProfileId profileId,
        AuthenticatedSshOperationCoordinator.OpenOutcome outcome
    ) {
        if (outcome == null) {
            state = State.failure(profileId, Phase.TRANSPORT_UNAVAILABLE);
        } else {
            switch (outcome.code()) {
                case CONNECTED:
                    state = State.connected(profileId);
                    break;
                case PROFILE_UNAVAILABLE:
                    state = State.failure(profileId, Phase.PROFILE_UNAVAILABLE);
                    break;
                case HOST_KEY_REJECTED:
                    state = State.failure(profileId, Phase.HOST_KEY_REJECTED);
                    break;
                case AUTHENTICATION_FAILED:
                    state = State.failure(profileId, Phase.AUTHENTICATION_FAILED);
                    break;
                case TRANSPORT_UNAVAILABLE:
                default:
                    state = State.failure(profileId, Phase.TRANSPORT_UNAVAILABLE);
                    break;
            }
        }
        listener.onStateChanged(state);
    }

    public interface StateListener {
        void onStateChanged(State state);
    }

    public enum Phase {
        AWAITING_PROFILE,
        INVALID_PROFILE,
        READY,
        CONNECTING,
        CONNECTED,
        PROFILE_UNAVAILABLE,
        HOST_KEY_REJECTED,
        AUTHENTICATION_FAILED,
        TRANSPORT_UNAVAILABLE
    }

    /** UI-safe state: profile IDs and all connection metadata remain deliberately redacted. */
    public static final class State {
        private final Phase phase;
        private final AuthenticatedSshOperationCoordinator.ProfileId profileId;

        private State(Phase phase, AuthenticatedSshOperationCoordinator.ProfileId profileId) {
            this.phase = phase;
            this.profileId = profileId;
        }

        static State awaitingProfile() { return new State(Phase.AWAITING_PROFILE, null); }
        static State invalidProfile() { return new State(Phase.INVALID_PROFILE, null); }
        static State ready(AuthenticatedSshOperationCoordinator.ProfileId id) { return new State(Phase.READY, id); }
        static State connecting(AuthenticatedSshOperationCoordinator.ProfileId id) { return new State(Phase.CONNECTING, id); }
        static State connected(AuthenticatedSshOperationCoordinator.ProfileId id) { return new State(Phase.CONNECTED, id); }
        static State failure(AuthenticatedSshOperationCoordinator.ProfileId id, Phase phase) { return new State(phase, id); }

        public Phase phase() { return phase; }
        public boolean canConnect() { return phase == Phase.READY || isFailure(); }
        public boolean isBusy() { return phase == Phase.CONNECTING; }
        private boolean isFailure() {
            return phase == Phase.PROFILE_UNAVAILABLE || phase == Phase.HOST_KEY_REJECTED
                || phase == Phase.AUTHENTICATION_FAILED || phase == Phase.TRANSPORT_UNAVAILABLE;
        }
    }
}
