package ai.choosh;

import java.util.Objects;

/**
 * Constructor-injected Android composition for one planned native connection and {@code git.status}.
 *
 * <p>The supplied {@link RustNativeConnectorJni.NativeRuntime} owns the Android socket and
 * Keystore callback registries. This class never sees either implementation: it composes their
 * opaque lease through {@link PlannedNativeConnectorPort}, then exposes only the authenticated
 * fixed-RPC capability to {@link GitStatusController}. It is deliberately framework-free so the
 * full lifecycle can be proven in JVM tests.</p>
 */
public final class AndroidGitStatusComposition {
    private final AuthenticatedSshOperationCoordinator connections;
    private final GitStatusRpc.RequestSource requests;

    public AndroidGitStatusComposition(
        AuthenticatedSshOperationCoordinator.ProfileConnectionSource profiles,
        PlannedNativeConnectorPort plannedConnector,
        GitStatusRpc.RequestSource requests
    ) {
        connections = new AuthenticatedSshOperationCoordinator(
            Objects.requireNonNull(profiles, "profiles"),
            new NativeAuthenticatedSshConnector(Objects.requireNonNull(plannedConnector, "plannedConnector"))
        );
        this.requests = Objects.requireNonNull(requests, "requests");
    }

    /**
     * Builds the complete planned-connector chain from explicit Android-owned capabilities.
     *
     * <p>The runtime is responsible for registering and releasing the bounded socket and
     * payload-only Keystore signer callback; JNI receives only its opaque handles.</p>
     */
    public static AndroidGitStatusComposition fromNativeRuntime(
        AuthenticatedSshOperationCoordinator.ProfileConnectionSource profiles,
        PlannedNativeConnectorPort.ConnectionGeneration generations,
        RustNativeConnectorJni.NativePlanBridge bridge,
        RustNativeConnectorJni.NativeRuntime runtime,
        PlannedNativeConnectorPort.PlannedTransportPort transport,
        GitStatusRpc.RequestSource requests
    ) {
        return new AndroidGitStatusComposition(
            profiles,
            new PlannedNativeConnectorPort(
                Objects.requireNonNull(generations, "generations"),
                new RustNativeConnectorJni.PlanFactory(
                    Objects.requireNonNull(bridge, "bridge"),
                    Objects.requireNonNull(runtime, "runtime")
                ),
                Objects.requireNonNull(transport, "transport")
            ),
            requests
        );
    }

    /** Opens the selected profile, then immediately refreshes its registered workspace status. */
    public void refresh(
        AuthenticatedSshOperationCoordinator.ProfileId profileId,
        Listener listener
    ) {
        Objects.requireNonNull(profileId, "profileId");
        Objects.requireNonNull(listener, "listener");
        connections.open(profileId, outcome -> {
            if (outcome.code() != AuthenticatedSshOperationCoordinator.OpenCode.CONNECTED) {
                listener.onConnectionFailure(outcome.code());
                return;
            }
            GitStatusController controller = new GitStatusController(
                outcome.operations(), requests, listener::onGitStatusState
            );
            listener.onGitStatusController(controller);
            controller.refresh();
        });
    }

    /** Presentation callbacks retain typed state only, never sockets, keys, or native handles. */
    public interface Listener {
        void onConnectionFailure(AuthenticatedSshOperationCoordinator.OpenCode failure);
        void onGitStatusController(GitStatusController controller);
        void onGitStatusState(GitStatusController.State state);
    }
}
