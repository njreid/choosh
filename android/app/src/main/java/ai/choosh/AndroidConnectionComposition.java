package ai.choosh;

import java.util.Objects;

/**
 * Framework-free application composition for the first real profile-to-native connection path.
 *
 * <p>It joins durable non-secret profile metadata, one lazy Android runtime factory, the JNI
 * plan bridge, and the fixed native transport. Construction is inert: it neither loads a profile
 * nor opens a socket nor obtains a credential capability. A fresh runtime is acquired only when
 * a caller opens an explicitly selected profile.</p>
 */
public final class AndroidConnectionComposition {
    private final ProfileConnectionMetadataSource.ProfileMetadataStore profiles;
    private final AndroidRuntimeComposition runtimes;
    private final PlannedNativeConnectorPort.ConnectionGeneration generations;
    private final RustNativeConnectorJni.NativePlanBridge bridge;
    private final PlannedNativeConnectorPort.PlannedTransportPort transport;
    private final GitStatusRpc.RequestSource requests;

    /**
     * Production outer-root constructor. JNI loading is confined to the selected bridge; all
     * storage, socket, public-key, and signing implementations remain explicit dependencies.
     */
    public AndroidConnectionComposition(
        ProfileConnectionMetadataSource.ProfileMetadataStore profiles,
        AndroidRuntimeComposition runtimes,
        PlannedNativeConnectorPort.ConnectionGeneration generations,
        GitStatusRpc.RequestSource requests
    ) {
        this(
            profiles,
            runtimes,
            generations,
            new RustNativeConnectorJni.JniPlanBridge(),
            new PlannedNativeConnectorPort.JniPlannedTransport(),
            requests
        );
    }

    /** Injectable constructor for deterministic JVM tests and alternate outer platform roots. */
    public AndroidConnectionComposition(
        ProfileConnectionMetadataSource.ProfileMetadataStore profiles,
        AndroidRuntimeComposition runtimes,
        PlannedNativeConnectorPort.ConnectionGeneration generations,
        RustNativeConnectorJni.NativePlanBridge bridge,
        PlannedNativeConnectorPort.PlannedTransportPort transport,
        GitStatusRpc.RequestSource requests
    ) {
        this.profiles = Objects.requireNonNull(profiles, "profiles");
        this.runtimes = Objects.requireNonNull(runtimes, "runtimes");
        this.generations = Objects.requireNonNull(generations, "generations");
        this.bridge = Objects.requireNonNull(bridge, "bridge");
        this.transport = Objects.requireNonNull(transport, "transport");
        this.requests = Objects.requireNonNull(requests, "requests");
    }

    /**
     * Builds an inert, presentation-independent connection capability.
     *
     * <p>The runtime lambda creates a fresh callback owner only when the planned connector
     * starts a connection attempt. In particular, selecting this composition does not retain a
     * socket, callback object, or credential binding.</p>
     */
    public AndroidGitStatusComposition newGitStatusComposition() {
        return AndroidGitStatusComposition.fromNativeRuntime(
            new ProfileConnectionMetadataSource(profiles),
            generations,
            bridge,
            input -> runtimes.newRuntime().acquire(input),
            transport,
            requests
        );
    }
}
