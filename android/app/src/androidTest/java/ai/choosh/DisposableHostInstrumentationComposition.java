package ai.choosh;

import android.os.Bundle;
import java.util.Objects;

/** Test-only constructor root for an externally provisioned M0-R5/R6 host fixture. */
public final class DisposableHostInstrumentationComposition {
    public static final String ARG_HOST = "choosh.fixture.host";
    public static final String ARG_PORT = "choosh.fixture.port";
    public static final String ARG_USERNAME = "choosh.fixture.username";
    public static final String ARG_HOST_FINGERPRINT = "choosh.fixture.host_fingerprint";
    private static final String PROFILE = "disposable_host";

    private DisposableHostInstrumentationComposition() { }

    /** Parses the four strict non-secret fixture arguments. */
    public static HostFixture fixture(Bundle arguments) throws FixtureException {
        Objects.requireNonNull(arguments, "arguments");
        try {
            return new HostFixture(required(arguments, ARG_HOST), required(arguments, ARG_PORT),
                required(arguments, ARG_USERNAME), required(arguments, ARG_HOST_FINGERPRINT));
        } catch (IllegalArgumentException | NullPointerException exception) {
            throw new FixtureException();
        }
    }

    /** Builds the real native connector without any Activity or production-profile UI state. */
    public static AndroidGitStatusComposition compose(
        HostFixture fixture, Identity identity, AndroidRuntimeComposition runtime,
        PlannedNativeConnectorPort.ConnectionGeneration generations, GitStatusRpc.RequestSource requests
    ) {
        Objects.requireNonNull(fixture, "fixture");
        Objects.requireNonNull(identity, "identity");
        Objects.requireNonNull(runtime, "runtime");
        AuthenticatedSshOperationCoordinator.ProfileConnectionSource profiles = profile -> {
            if (!PROFILE.equals(profile.valueForProfileStore())) {
                throw new AuthenticatedSshOperationCoordinator.ProfileUnavailableException();
            }
            return new AuthenticatedSshOperationCoordinator.ConnectionRequest(
                profile, fixture.endpoint, fixture.username, fixture.knownHost,
                identity.credential(), identity.publicKey()
            );
        };
        return AndroidGitStatusComposition.fromNativeRuntime(
            profiles, Objects.requireNonNull(generations, "generations"),
            new RustNativeConnectorJni.JniPlanBridge(), runtime.newRuntime(),
            new PlannedNativeConnectorPort.JniPlannedTransport(),
            Objects.requireNonNull(requests, "requests")
        );
    }

    /** Fixed test profile ID; production IDs cannot select this composition. */
    public static AuthenticatedSshOperationCoordinator.ProfileId profileId() {
        return new AuthenticatedSshOperationCoordinator.ProfileId(PROFILE);
    }

    /** Test-owned credential/public-key pairing produced by Android Keystore setup. */
    public interface Identity {
        SshKeyImportCoordinator.OpaqueCredentialRef credential();
        SshKeyImportCoordinator.PublicKeyMetadata publicKey();
    }

    /** Parsed endpoint data; it contains no path, command, or key material. */
    public static final class HostFixture {
        private final ProfileConnectionMetadataSource.SshEndpoint endpoint;
        private final ProfileConnectionMetadataSource.SshUsername username;
        private final ProfileConnectionMetadataSource.KnownHost knownHost;
        private HostFixture(String host, String port, String username, String fingerprint) {
            endpoint = new ProfileConnectionMetadataSource.SshEndpoint(host, parsePort(port));
            this.username = new ProfileConnectionMetadataSource.SshUsername(username);
            knownHost = new ProfileConnectionMetadataSource.KnownHost(
                ProfileConnectionMetadataSource.HostKeyAlgorithm.ED25519, fingerprint
            );
        }
    }

    private static String required(Bundle arguments, String name) {
        String value = arguments.getString(name);
        if (value == null || value.isEmpty() || value.length() > 255) throw new IllegalArgumentException();
        return value;
    }

    private static int parsePort(String value) {
        try {
            int port = Integer.parseInt(value);
            if (port < 1 || port > 65_535) throw new IllegalArgumentException();
            return port;
        } catch (NumberFormatException exception) {
            throw new IllegalArgumentException();
        }
    }

    /** Content-free runner argument/preflight failure. */
    public static final class FixtureException extends Exception { public FixtureException() { super(); } }
}
